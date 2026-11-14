use std::{collections::BTreeMap, fmt};

use serde_json::Value;
use thiserror::Error;

/// A protocol parameter value in the exact form both sides are compared in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Canon {
    Null,
    Int(i128),
    /// A reduced fraction whose denominator is above one.
    Ratio(i128, i128),
    List(Vec<Canon>),
    Map(BTreeMap<String, Canon>),
}

/// A ledger parameter value with no exact form.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("PARAMS key={key} value={value} has no exact form")]
pub struct Inexact {
    pub key: String,
    pub value: String,
}

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 {
        a.abs()
    } else {
        gcd(b, a % b)
    }
}

impl Canon {
    /// The reduced fraction, an integer when the denominator reduces to one.
    pub fn ratio(numerator: i128, denominator: i128) -> Option<Canon> {
        if denominator == 0 {
            return None;
        }
        let g = gcd(numerator, denominator);
        let sign = denominator.signum();
        let (n, d) = (sign * numerator / g, sign * denominator / g);
        Some(if d == 1 {
            Canon::Int(n)
        } else {
            Canon::Ratio(n, d)
        })
    }

    /// The exact value of a JSON number from its decimal text.
    pub fn number(text: &str) -> Option<Canon> {
        let (mantissa, exponent) = match text.split_once(['e', 'E']) {
            Some((m, e)) => (m, e.parse::<i32>().ok()?),
            None => (text, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        let digits: i128 = format!("{whole}{fraction}").parse().ok()?;
        let scale = exponent - i32::try_from(fraction.len()).ok()?;
        let ten = |p: i32| 10i128.checked_pow(p.unsigned_abs());
        if scale >= 0 {
            Canon::ratio(digits.checked_mul(ten(scale)?)?, 1)
        } else {
            Canon::ratio(digits, ten(scale)?)
        }
    }

    /// The exact value of a ledger parameter written as JSON.
    pub fn of_json(key: &str, value: &Value) -> Result<Canon, Inexact> {
        let inexact = || Inexact {
            key: key.to_string(),
            value: value.to_string(),
        };
        match value {
            Value::Null => Ok(Canon::Null),
            Value::Number(n) => Canon::number(&n.to_string()).ok_or_else(inexact),
            Value::Array(items) => items
                .iter()
                .map(|v| Canon::of_json(key, v))
                .collect::<Result<_, _>>()
                .map(Canon::List),
            // The ledger writes a ratio with no finite decimal as its numerator and denominator.
            Value::Object(fields)
                if fields.len() == 2
                    && fields.contains_key("numerator")
                    && fields.contains_key("denominator") =>
            {
                let part = |k: &str| fields[k].as_i64().map(i128::from).ok_or_else(inexact);
                Canon::ratio(part("numerator")?, part("denominator")?).ok_or_else(inexact)
            }
            Value::Object(fields) => fields
                .iter()
                .map(|(k, v)| Ok((k.clone(), Canon::of_json(&format!("{key}.{k}"), v)?)))
                .collect::<Result<_, _>>()
                .map(Canon::Map),
            Value::Bool(_) | Value::String(_) => Err(inexact()),
        }
    }
}

impl fmt::Display for Canon {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Canon::Null => write!(f, "null"),
            Canon::Int(n) => write!(f, "{n}"),
            Canon::Ratio(n, d) => write!(f, "{n}/{d}"),
            Canon::List(items) => write!(f, "list[{}]", items.len()),
            Canon::Map(fields) => {
                write!(f, "{{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{k}={v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

/// The first place two parameter values differ, as a dotted path and both
/// values.
pub fn first_difference(path: &str, ledger: &Canon, served: &Canon) -> Option<String> {
    match (ledger, served) {
        (Canon::Map(l), Canon::Map(s)) => l.keys().chain(s.keys()).find_map(|k| {
            let null = Canon::Null;
            let (a, b) = (l.get(k).unwrap_or(&null), s.get(k).unwrap_or(&null));
            first_difference(&format!("{path}.{k}"), a, b)
        }),
        (Canon::List(l), Canon::List(s)) if l.len() == s.len() => l
            .iter()
            .zip(s)
            .enumerate()
            .find_map(|(i, (a, b))| first_difference(&format!("{path}[{i}]"), a, b)),
        _ if ledger == served => None,
        _ => Some(format!("{path} ledger={ledger} dolos={served}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_text_reads_as_an_exact_fraction() {
        assert_eq!(Canon::number("0.0577"), Some(Canon::Ratio(577, 10000)));
        assert_eq!(Canon::number("7.21e-5"), Some(Canon::Ratio(721, 10000000)));
        assert_eq!(Canon::number("1.2"), Some(Canon::Ratio(6, 5)));
        assert_eq!(Canon::number("15"), Some(Canon::Int(15)));
        assert_eq!(Canon::number("-3"), Some(Canon::Int(-3)));
        assert_eq!(Canon::number("2e3"), Some(Canon::Int(2000)));
        assert_eq!(Canon::ratio(15, 1), Some(Canon::Int(15)));
    }

    #[test]
    fn serde_prints_corpus_decimals_as_their_exact_text() {
        let v: Value = serde_json::from_str("[0.0577, 7.21e-05, 0.015]").unwrap();
        assert_eq!(
            Canon::of_json("k", &v),
            Ok(Canon::List(vec![
                Canon::Ratio(577, 10000),
                Canon::Ratio(721, 10000000),
                Canon::Ratio(3, 200),
            ]))
        );
    }

    #[test]
    fn a_numerator_and_denominator_object_reads_as_its_fraction() {
        let units: Value = serde_json::from_str(r#"{"memory": 2, "steps": 3}"#).unwrap();
        assert_eq!(
            Canon::of_json("k", &units),
            Ok(Canon::Map(
                [
                    ("memory".into(), Canon::Int(2)),
                    ("steps".into(), Canon::Int(3))
                ]
                .into()
            ))
        );
        let ratio: Value = serde_json::from_str(r#"{"denominator": 3, "numerator": 2}"#).unwrap();
        assert_eq!(Canon::of_json("k", &ratio), Ok(Canon::Ratio(2, 3)));
        let whole: Value = serde_json::from_str(r#"{"denominator": 3, "numerator": 6}"#).unwrap();
        assert_eq!(Canon::of_json("k", &whole), Ok(Canon::Int(2)));
    }

    #[test]
    fn first_difference_names_the_path() {
        let l = Canon::Map([("a".into(), Canon::List(vec![Canon::Int(1), Canon::Int(2)]))].into());
        let s = Canon::Map([("a".into(), Canon::List(vec![Canon::Int(1), Canon::Int(3)]))].into());
        assert_eq!(first_difference("p", &l, &l), None);
        assert_eq!(
            first_difference("p", &l, &s).as_deref(),
            Some("p.a[1] ledger=2 dolos=3")
        );
        let short = Canon::Map([("a".into(), Canon::List(vec![Canon::Int(1)]))].into());
        assert_eq!(
            first_difference("p", &l, &short).as_deref(),
            Some("p.a ledger=list[2] dolos=list[1]")
        );
    }
}
