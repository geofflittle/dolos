//! The cardano-cbor-dataset corpus read into rows: the generator's Conway
//! rules, and each rule's samples with the ledger's answer for them.

use std::{fs, path::Path, process::Command};

use ledger_oracle_judge::comparison::{Accepted, Reference, Row};

/// How the ledger's re-encoding of a rule's value is compared with pallas's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// The ledger keeps the bytes it read, so they come back unchanged.
    Exact,
    /// The ledger writes its own encoding, which the dataset normalises.
    Normalized,
}

/// A rule the generator lists for Conway and how its encoding is compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratorRule {
    pub name: String,
    pub encoding: Encoding,
}

/// The kind of a sample, from the name of its class directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    Valid,
    ManualValid,
    ManualInvalid,
    /// A generated sample the generator mutated the given number of times.
    Zap(u8),
}

impl SampleKind {
    /// The kind a class directory names, if it names one.
    pub fn from_dir(name: &str) -> Option<SampleKind> {
        match name {
            "valid" => Some(SampleKind::Valid),
            "manual-valid" => Some(SampleKind::ManualValid),
            "manual-invalid" => Some(SampleKind::ManualInvalid),
            _ => name
                .strip_prefix("invalid-zap-")
                .and_then(|n| n.parse().ok())
                .map(SampleKind::Zap),
        }
    }

    /// Whether the ledger accepts the samples of this kind.
    pub fn accepted(self) -> bool {
        matches!(self, SampleKind::Valid | SampleKind::ManualValid)
    }
}

/// The commit the dataset submodule is checked out at, refused unless it is
/// the commit the Dolos tree records for it.
pub fn checked_out_pin(manifest_dir: &Path, submodule: &str) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(["submodule", "status", "--", submodule])
        .output()
        .map_err(|e| format!("git submodule status could not run: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next().unwrap_or_default();
    match (out.status.success(), line.chars().next()) {
        (true, Some(' ')) => line
            .split_whitespace()
            .next()
            .map(str::to_owned)
            .ok_or_else(|| format!("git submodule status printed {line:?}")),
        (true, Some('-')) => Err(format!(
            "{submodule} is not checked out, run git submodule update --init"
        )),
        (true, Some('+')) => Err(format!(
            "{submodule} is checked out at another commit than the tree records: {line}"
        )),
        _ => Err(format!(
            "git submodule status {submodule} printed {line:?} and {}",
            String::from_utf8_lossy(&out.stderr)
        )),
    }
}

/// The Conway rules the generator's rule table lists, in its order.
pub fn generator_rules(source: &str) -> Result<Vec<GeneratorRule>, String> {
    let mut lines = source
        .lines()
        .skip_while(|l| !l.starts_with("conwayRules ="));
    lines
        .next()
        .ok_or("the generator source has no conwayRules table")?;
    let mut rules = Vec::new();
    for line in lines.take_while(|l| l.is_empty() || l.starts_with(' ')) {
        let item = line
            .trim()
            .trim_start_matches("in [")
            .trim_start_matches(['[', ','])
            .trim();
        if item.is_empty()
            || item.starts_with("--")
            || item.starts_with("let version")
            || item == "]"
        {
            continue;
        }
        let constructor = item.split_whitespace().next().unwrap_or_default();
        let encoding = match constructor {
            "mkRule" | "mkAnnotatorRule" | "mkPositiveMultiAssetRule" => Encoding::Normalized,
            "mkByteExactRule" | "mkByteExactAnnotatorRule" => Encoding::Exact,
            _ => {
                return Err(format!(
                    "the Conway rule table has a line this reader does not know: {line:?}"
                ));
            }
        };
        let name = item
            .trim_end_matches(',')
            .strip_suffix('"')
            .and_then(|s| s.rsplit_once('"'))
            .map(|(_, name)| name.to_owned())
            .ok_or_else(|| format!("no rule name on {line:?}"))?;
        rules.push(GeneratorRule { name, encoding });
    }
    match rules.is_empty() {
        true => Err("the Conway rule table lists no rule".to_owned()),
        false => Ok(rules),
    }
}

/// The rule directories of an era directory, refusing any other entry but
/// the corpus summaries.
pub fn rule_dirs(era: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    for entry in fs::read_dir(era).map_err(|e| format!("{}: {e}", era.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        match entry.path().is_dir() {
            true => names.push(name),
            false if ["corpus.json", "coverage.json"].contains(&name.as_str()) => {}
            false => {
                return Err(format!(
                    "{}: {name} is neither a rule nor a summary",
                    era.display()
                ));
            }
        }
    }
    names.sort();
    Ok(names)
}

/// The classes and rows of one rule's directory, a class per sample kind
/// directory and a row per sample.
pub fn rule_rows(rule: &str, dir: &Path) -> Result<(Vec<String>, Vec<Row>), String> {
    let mut classes = Vec::new();
    let mut rows = Vec::new();
    for kind_dir in sorted_entries(dir)? {
        let kind_name = file_name(&kind_dir);
        let kind = SampleKind::from_dir(&kind_name)
            .ok_or_else(|| format!("{}: {kind_name} is not a sample kind", dir.display()))?;
        let class = format!("{rule} {kind_name}");
        let files = sorted_entries(&kind_dir)?;
        let mut stems = Vec::new();
        for file in &files {
            let name = file_name(file);
            match (
                name.strip_suffix(".input.cbor"),
                name.strip_suffix(".expected.cbor"),
            ) {
                (Some(stem), None) => stems.push(stem.to_owned()),
                (None, Some(stem)) if kind.accepted() => {
                    if !kind_dir.join(format!("{stem}.input.cbor")).is_file() {
                        return Err(format!(
                            "{}: an expected file with no input",
                            file.display()
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "{}: not a sample file of {kind_name}",
                        file.display()
                    ));
                }
            }
        }
        for stem in stems {
            let read = |suffix: &str| {
                let path = kind_dir.join(format!("{stem}.{suffix}.cbor"));
                fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
            };
            let reference = match kind.accepted() {
                true => Reference::Accepts(Accepted {
                    dump: serde_json::Value::Null,
                    encoding: Some(read("expected")?),
                }),
                false => Reference::Refuses(kind_name.clone()),
            };
            rows.push(Row {
                class: class.clone(),
                variant: Some(format!("{rule}/{kind_name}/{stem}")),
                input: read("input")?,
                reference,
                rules: Vec::new(),
                site: None,
                mutated: None,
                base: None,
            });
        }
        classes.push(class);
    }
    Ok((classes, rows))
}

fn sorted_entries(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut out = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .map(|e| e.map(|e| e.path()).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    out.sort();
    Ok(out)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
