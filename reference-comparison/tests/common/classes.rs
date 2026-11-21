//! The places a change to an input can take, derived from the reference's CDDL.

use std::collections::{BTreeMap, BTreeSet};

use cddl::ast::{
    CDDL, GenericArgs, GroupChoice, GroupEntry, MemberKey, Occur, RangeCtlOp, Rule, Type, Type1,
    Type2,
};
use cddl::token::{ControlOperator, Value};
use ledger_oracle_judge::comparison::{ChangeKind, Part, Place, PlaceCoverage, Step};

/// Each records class prefix and the rule its inputs are values of.
pub const ROOTS: &[(&str, &str)] = &[
    ("tx", "mempool_transaction"),
    ("body", "transaction_body"),
    ("wits", "transaction_witness_set"),
    ("ppu", "protocol_param_update"),
    ("block", "block"),
    ("eb", "eb_references"),
];

/// The kinds of change made to a whole input of any root.
const WHOLE: [ChangeKind; 2] = [ChangeKind::WideHeads, ChangeKind::Zap];

/// The kinds of change no rule's shape states, each with the rule it applies
/// to.
const HAND_ADDED: &[(&str, &[ChangeKind])] = &[
    (
        "mempool_transaction",
        &[ChangeKind::TrailingBytes, ChangeKind::BlockForm],
    ),
    (
        "unit_interval",
        &[ChangeKind::UnitAbove, ChangeKind::UnitZeroDenominator],
    ),
];

const SET: [ChangeKind; 3] = [
    ChangeKind::SetDuplicate,
    ChangeKind::SetTwoWays,
    ChangeKind::SetReorder,
];

const RATIONAL: [ChangeKind; 6] = [
    ChangeKind::RationalTag,
    ChangeKind::RationalThird,
    ChangeKind::RationalUnreduced,
    ChangeKind::RationalOne,
    ChangeKind::RationalEmpty,
    ChangeKind::RationalIndefinite,
];

const RECORD: [ChangeKind; 2] = [ChangeKind::ArrayExtra, ChangeKind::ArrayShort];

/// The tag of a rational number.
const RATIONAL_TAG: u64 = 30;

/// The tag of a set.
const SET_TAG: u64 = 258;

/// The CDDL prelude's names this grammar uses.
const PRELUDE: &[&str] = &[
    "any",
    "bool",
    "bstr",
    "bytes",
    "false",
    "float",
    "int",
    "nil",
    "nint",
    "null",
    "text",
    "true",
    "tstr",
    "uint",
    "undefined",
];

/// The grammar of `text`.
pub fn parse(text: &str) -> CDDL<'_> {
    cddl::cddl_from_str(text, false).unwrap_or_else(|e| panic!("defs.cddl does not parse: {e}"))
}

/// The name of each rule `cddl` defines, in the order it defines them.
pub fn rule_names(cddl: &CDDL) -> Vec<String> {
    cddl.rules
        .iter()
        .map(|rule| match rule {
            Rule::Type { rule, .. } => rule.name.ident.to_owned(),
            Rule::Group { rule, .. } => rule.name.ident.to_owned(),
        })
        .collect()
}

/// The records class of the inputs changed at `place`.
pub fn class(place: &Place) -> String {
    let (prefix, _) = ROOTS
        .iter()
        .find(|(_, root)| *root == place.root)
        .unwrap_or_else(|| panic!("{} is not a root", place.root));
    format!("{prefix} {}", place.kind)
}

/// Each derived place with its count of changed inputs, then the number of
/// places and of changed ones per root and kind.
pub fn report(coverage: &PlaceCoverage) -> String {
    let mut out = String::new();
    let mut per: BTreeMap<(&str, ChangeKind), (usize, usize)> = BTreeMap::new();
    for (place, n) in coverage.counts() {
        out.push_str(&format!("{n} {place}\n"));
        let (all, changed) = per.entry((place.root.as_str(), place.kind)).or_default();
        *all += 1;
        *changed += usize::from(*n > 0);
    }
    for ((root, kind), (all, changed)) in &per {
        out.push_str(&format!("{root} {kind}: {all} places, {changed} changed\n"));
    }
    out
}

/// Every place under each of `roots` where a change of some kind is made.
pub fn places(cddl: &CDDL, roots: &[&str]) -> BTreeSet<Place> {
    let defs = definitions(cddl);
    let mut out = BTreeSet::new();
    for root in roots {
        let mut walk = Walk {
            defs: &defs,
            root,
            entered: Vec::new(),
            out: &mut out,
        };
        walk.emit(&[], &WHOLE);
        let name = walk.known(root);
        walk.rule(name, Vec::new(), &mut Vec::new());
    }
    out
}

/// A rule's generic parameters and its value.
enum Def<'a, 'g> {
    Type(Vec<&'a str>, &'g Type<'a>),
    Group(&'g GroupEntry<'a>),
}

fn definitions<'a, 'g>(cddl: &'g CDDL<'a>) -> BTreeMap<&'a str, Def<'a, 'g>> {
    let mut defs = BTreeMap::new();
    for rule in &cddl.rules {
        let (name, def) = match rule {
            Rule::Type { rule, .. } => {
                assert!(
                    !rule.is_type_choice_alternate,
                    "{} adds a choice with /=",
                    rule.name.ident
                );
                let params = rule
                    .generic_params
                    .iter()
                    .flat_map(|g| g.params.iter().map(|p| p.param.ident))
                    .collect();
                (rule.name.ident, Def::Type(params, &rule.value))
            }
            Rule::Group { rule, .. } => {
                assert!(
                    !rule.is_group_choice_alternate && rule.generic_params.is_none(),
                    "{} is a group rule with a choice added or parameters",
                    rule.name.ident
                );
                (rule.name.ident, Def::Group(&rule.entry))
            }
        };
        assert!(defs.insert(name, def).is_none(), "{name} is defined twice");
    }
    defs
}

/// The rule whose steps a value is named by, and the names its generic
/// parameters are bound to.
struct Scope<'a> {
    rule: &'a str,
    env: BTreeMap<&'a str, &'a str>,
}

struct Walk<'w, 'a, 'g> {
    defs: &'w BTreeMap<&'a str, Def<'a, 'g>>,
    root: &'w str,
    entered: Vec<String>,
    out: &'w mut BTreeSet<Place>,
}

impl<'a> Walk<'_, 'a, '_> {
    fn emit(&mut self, path: &[Step], kinds: &[ChangeKind]) {
        for kind in kinds {
            self.out.insert(Place {
                root: self.root.to_owned(),
                path: path.to_vec(),
                kind: *kind,
            });
        }
    }

    /// The defined or prelude name `name`, borrowed from the grammar.
    fn known(&self, name: &str) -> &'a str {
        match self.defs.get_key_value(name) {
            Some((known, _)) => known,
            None => PRELUDE
                .iter()
                .copied()
                .find(|p| *p == name)
                .unwrap_or_else(|| panic!("{name} is not defined")),
        }
    }

    /// The name a type name stands for in `scope`.
    fn resolve(&self, name: &str, scope: &Scope<'a>) -> &'a str {
        scope
            .env
            .get(name)
            .copied()
            .unwrap_or_else(|| self.known(name))
    }

    fn args(&self, args: Option<&GenericArgs<'a>>, scope: &Scope<'a>) -> Vec<&'a str> {
        args.iter()
            .flat_map(|a| a.args.iter())
            .map(|a| match (&a.arg.type2, &a.arg.operator) {
                (
                    Type2::Typename {
                        ident,
                        generic_args: None,
                        ..
                    },
                    None,
                ) => self.resolve(ident.ident, scope),
                _ => panic!("{}: a generic argument that is not a name", scope.rule),
            })
            .collect()
    }

    fn rule(&mut self, name: &'a str, args: Vec<&'a str>, path: &mut Vec<Step>) {
        if PRELUDE.contains(&name) {
            return;
        }
        let key = if args.is_empty() {
            name.to_owned()
        } else {
            format!("{name}<{}>", args.join(","))
        };
        if self.entered.contains(&key) {
            return;
        }
        self.entered.push(key);
        let defs = self.defs;
        match defs.get(name) {
            Some(Def::Type(params, value)) => {
                assert_eq!(
                    params.len(),
                    args.len(),
                    "{name} takes {} arguments",
                    params.len()
                );
                let scope = Scope {
                    rule: name,
                    env: params.iter().copied().zip(args).collect(),
                };
                self.ty(value, &scope, path, true);
            }
            Some(Def::Group(_)) => panic!("the group rule {name} is used as a type"),
            None => panic!("{name} is not defined"),
        }
        if let Some((_, kinds)) = HAND_ADDED.iter().find(|(rule, _)| *rule == name) {
            self.emit(path, kinds);
        }
        self.entered.pop();
    }

    fn typename(
        &mut self,
        name: &str,
        args: Option<&GenericArgs<'a>>,
        scope: &Scope<'a>,
        path: &mut Vec<Step>,
    ) {
        let args = self.args(args, scope);
        let name = self.resolve(name, scope);
        self.rule(name, args, path);
    }

    fn ty(&mut self, t: &Type<'a>, scope: &Scope<'a>, path: &mut Vec<Step>, record: bool) {
        let arms: Vec<&Type1<'a>> = t.type_choices.iter().map(|c| &c.type1).collect();
        if let [only] = arms.as_slice() {
            return self.type1(only, scope, path, record);
        }
        let mut arm_record = true;
        if is_set(&arms) {
            self.emit(path, &SET);
        } else if distinct(arms.iter().map(|a| self.lead(a))) {
            self.emit(path, &[ChangeKind::UnknownArm]);
            if arms.iter().all(|a| is_array(a)) {
                self.emit(path, &RECORD);
                arm_record = false;
            }
        }
        for (i, arm) in arms.iter().enumerate() {
            let part = match (&arm.type2, &arm.operator) {
                (Type2::Typename { ident, .. }, None) => {
                    Part::Named(self.resolve(ident.ident, scope).to_owned())
                }
                _ => Part::Position(i),
            };
            path.push(Step::Arm {
                rule: scope.rule.to_owned(),
                arm: part,
            });
            self.type1(arm, scope, path, arm_record);
            path.pop();
        }
    }

    fn type1(&mut self, t: &Type1<'a>, scope: &Scope<'a>, path: &mut Vec<Step>, record: bool) {
        let Some(op) = &t.operator else {
            return self.type2(&t.type2, scope, path, record);
        };
        match &op.operator {
            RangeCtlOp::RangeOp { is_inclusive, .. } => {
                let low = self.number(&t.type2);
                let high = self.number(&op.type2) - i128::from(!is_inclusive);
                self.range(path, low, high);
            }
            RangeCtlOp::CtlOp { ctrl, .. } => match ctrl {
                ControlOperator::CBOR => self.type2(&op.type2, scope, path, true),
                ControlOperator::LE => {
                    assert_eq!(base(t), "uint", "{}: .le on a non number", scope.rule);
                    let high = self.number(&op.type2);
                    self.range(path, 0, high);
                }
                ControlOperator::SIZE => self.size(base(t), &op.type2, scope, path),
                other => panic!("{}: the control {other:?}", scope.rule),
            },
        }
    }

    fn type2(&mut self, t: &Type2<'a>, scope: &Scope<'a>, path: &mut Vec<Step>, record: bool) {
        match t {
            Type2::Typename {
                ident,
                generic_args,
                ..
            } => self.typename(ident.ident, generic_args.as_ref(), scope, path),
            Type2::ParenthesizedType { pt, .. } => self.ty(pt, scope, path, record),
            Type2::Array { group, .. } => match group.group_choices.as_slice() {
                [one] => self.array(one, scope, path, record),
                choices => self.arms(choices, scope, path),
            },
            Type2::Map { group, .. } => match group.group_choices.as_slice() {
                [one] => self.map(one, scope, path),
                _ => panic!("{}: a map with group choices", scope.rule),
            },
            Type2::TaggedData { tag, t, .. } => match tag.as_ref().and_then(|c| c.as_literal()) {
                Some(RATIONAL_TAG) => {
                    self.emit(path, &RATIONAL);
                    self.ty(t, scope, path, false);
                }
                Some(_) => self.ty(t, scope, path, record),
                None => panic!("{}: a tag that is not a number", scope.rule),
            },
            Type2::IntValue { .. }
            | Type2::UintValue { .. }
            | Type2::FloatValue { .. }
            | Type2::TextValue { .. }
            | Type2::UTF8ByteString { .. }
            | Type2::B16ByteString { .. }
            | Type2::B64ByteString { .. } => {}
            other => panic!("{}: the type {other}", scope.rule),
        }
    }

    /// An array of one group: a list when its one entry repeats, else a record.
    fn array(
        &mut self,
        group: &GroupChoice<'a>,
        scope: &Scope<'a>,
        path: &mut Vec<Step>,
        record: bool,
    ) {
        let entries: Vec<&GroupEntry<'a>> = group.group_entries.iter().map(|(e, _)| e).collect();
        if let [only] = entries.as_slice()
            && repeats(only, scope)
        {
            path.push(Step::Element);
            self.entry(only, scope, path);
            path.pop();
            return;
        }
        assert!(
            !entries.iter().any(|e| repeats(e, scope)),
            "{}: an array of repeated and single entries",
            scope.rule
        );
        if record {
            self.emit(path, &RECORD);
        }
        self.members(group, scope.rule, scope, path);
    }

    /// An array of several groups, told apart by their leading numbers.
    fn arms(&mut self, choices: &[GroupChoice<'a>], scope: &Scope<'a>, path: &mut Vec<Step>) {
        assert!(
            distinct(choices.iter().map(|c| self.lead_group(c))),
            "{}: array arms not told apart by a leading number",
            scope.rule
        );
        self.emit(path, &[ChangeKind::UnknownArm]);
        self.emit(path, &RECORD);
        for (i, choice) in choices.iter().enumerate() {
            let part = match choice.group_entries.as_slice() {
                [(GroupEntry::TypeGroupname { ge, .. }, _)] if ge.occur.is_none() => {
                    Part::Named(self.resolve(ge.name.ident, scope).to_owned())
                }
                _ => Part::Position(i),
            };
            path.push(Step::Arm {
                rule: scope.rule.to_owned(),
                arm: part,
            });
            self.members(choice, scope.rule, scope, path);
            path.pop();
        }
    }

    /// The declared members of an array group, with a group rule's members
    /// named by that rule.
    fn members(
        &mut self,
        group: &GroupChoice<'a>,
        rule: &'a str,
        scope: &Scope<'a>,
        path: &mut Vec<Step>,
    ) {
        for (position, (entry, _)) in group.group_entries.iter().enumerate() {
            match entry {
                GroupEntry::ValueMemberKey { ge, .. } => {
                    let member = match &ge.member_key {
                        None => Part::Position(position),
                        Some(MemberKey::Bareword { ident, .. }) => {
                            Part::Named(ident.ident.to_owned())
                        }
                        Some(_) => panic!("{rule}: a keyed member of an array"),
                    };
                    path.push(Step::Member {
                        rule: rule.to_owned(),
                        member,
                    });
                    self.ty(&ge.entry_type, scope, path, true);
                    path.pop();
                }
                GroupEntry::TypeGroupname { ge, .. } => {
                    let name = self.resolve(ge.name.ident, scope);
                    let defs = self.defs;
                    if let Some(Def::Group(entry)) = defs.get(name) {
                        self.group_rule(name, entry, path);
                    } else {
                        path.push(Step::Member {
                            rule: rule.to_owned(),
                            member: Part::Position(position),
                        });
                        self.typename(ge.name.ident, ge.generic_args.as_ref(), scope, path);
                        path.pop();
                    }
                }
                GroupEntry::InlineGroup { .. } => panic!("{rule}: an inline group in an array"),
            }
        }
    }

    fn group_rule(&mut self, name: &'a str, entry: &GroupEntry<'a>, path: &mut Vec<Step>) {
        if self.entered.iter().any(|e| e == name) {
            return;
        }
        let GroupEntry::InlineGroup {
            occur: None, group, ..
        } = entry
        else {
            panic!("{name}: a group rule that is not one parenthesised group");
        };
        let [one] = group.group_choices.as_slice() else {
            panic!("{name}: a group rule with choices");
        };
        self.entered.push(name.to_owned());
        let scope = Scope {
            rule: name,
            env: BTreeMap::new(),
        };
        self.members(one, name, &scope, path);
        self.entered.pop();
    }

    /// A map: a record of numbered keys, a table of keys of one type, or both.
    fn map(&mut self, group: &GroupChoice<'a>, scope: &Scope<'a>, path: &mut Vec<Step>) {
        let (mut record, mut table) = (false, false);
        for (entry, _) in &group.group_entries {
            let GroupEntry::ValueMemberKey { ge, .. } = entry else {
                panic!("{}: a map entry with no key", scope.rule);
            };
            match &ge.member_key {
                Some(MemberKey::Value {
                    value: Value::UINT(key),
                    ..
                }) => {
                    record = true;
                    path.push(Step::Key {
                        rule: scope.rule.to_owned(),
                        key: *key as u64,
                    });
                    self.ty(&ge.entry_type, scope, path, true);
                    path.pop();
                }
                Some(MemberKey::Type1 { t1, .. }) => {
                    table = true;
                    path.push(Step::EntryKey);
                    self.type1(t1, scope, path, true);
                    path.pop();
                    path.push(Step::EntryValue);
                    self.ty(&ge.entry_type, scope, path, true);
                    path.pop();
                }
                _ => panic!("{}: a map key that is not a number or a type", scope.rule),
            }
        }
        if record && !table {
            self.emit(path, &[ChangeKind::MapUnknownKey]);
        }
    }

    /// The value of a repeated entry.
    fn entry(&mut self, entry: &GroupEntry<'a>, scope: &Scope<'a>, path: &mut Vec<Step>) {
        match entry {
            GroupEntry::ValueMemberKey { ge, .. } if ge.member_key.is_none() => {
                self.ty(&ge.entry_type, scope, path, true)
            }
            GroupEntry::TypeGroupname { ge, .. } => {
                self.typename(ge.name.ident, ge.generic_args.as_ref(), scope, path)
            }
            _ => panic!("{}: a repeated entry that is not one value", scope.rule),
        }
    }

    fn size(&mut self, base: &str, size: &Type2<'a>, scope: &Scope<'a>, path: &[Step]) {
        let (low, high) = match size {
            Type2::ParenthesizedType { pt, .. } => match pt.type_choices.as_slice() {
                [c] => match &c.type1.operator {
                    Some(op)
                        if matches!(
                            op.operator,
                            RangeCtlOp::RangeOp {
                                is_inclusive: true,
                                ..
                            }
                        ) =>
                    {
                        (self.number(&c.type1.type2), self.number(&op.type2))
                    }
                    _ => panic!("{}: a size that is not a range", scope.rule),
                },
                _ => panic!("{}: a size that is a choice", scope.rule),
            },
            other => {
                let n = self.number(other);
                (n, n)
            }
        };
        match base {
            "uint" => {
                assert_eq!(low, high, "{}: a number of a range of sizes", scope.rule);
                self.range(path, 0, (1i128 << (8 * low)) - 1);
            }
            "bytes" | "bstr" | "text" | "tstr" => {
                let mut kinds = vec![ChangeKind::RangeAbove];
                if low > 0 {
                    kinds.push(ChangeKind::RangeBelow);
                }
                self.emit(path, &kinds);
            }
            other => panic!("{}: .size on {other}", scope.rule),
        }
    }

    /// The kinds of a range of integers: a value one past a bound, of the same
    /// CBOR major type as the bound.
    fn range(&mut self, path: &[Step], low: i128, high: i128) {
        let mut kinds = Vec::new();
        if low != 0 && low > -(1i128 << 64) {
            kinds.push(ChangeKind::RangeBelow);
        }
        if high != -1 && high < i128::from(u64::MAX) {
            kinds.push(ChangeKind::RangeAbove);
        }
        self.emit(path, &kinds);
    }

    /// The integer a literal or a rule naming one stands for.
    fn number(&self, t: &Type2<'a>) -> i128 {
        match t {
            Type2::UintValue { value, .. } => *value as i128,
            Type2::IntValue { value, .. } => *value as i128,
            Type2::Typename {
                ident,
                generic_args: None,
                ..
            } => match self.defs.get(ident.ident) {
                Some(Def::Type(params, value)) if params.is_empty() => {
                    match value.type_choices.as_slice() {
                        [c] if c.type1.operator.is_none() => self.number(&c.type1.type2),
                        _ => panic!("{} is not one number", ident.ident),
                    }
                }
                _ => panic!("{} is not one number", ident.ident),
            },
            other => panic!("{other} is not a number"),
        }
    }

    /// The number a value starts with, when it starts with a literal one.
    fn lead(&self, t: &Type1<'a>) -> Option<u64> {
        if t.operator.is_some() {
            return None;
        }
        match &t.type2 {
            Type2::UintValue { value, .. } => Some(*value as u64),
            Type2::Array { group, .. } => match group.group_choices.as_slice() {
                [one] => self.lead_group(one),
                _ => None,
            },
            Type2::Typename {
                ident,
                generic_args: None,
                ..
            } => match self.defs.get(ident.ident) {
                Some(Def::Type(params, value)) if params.is_empty() => {
                    match value.type_choices.as_slice() {
                        [c] => self.lead(&c.type1),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn lead_group(&self, group: &GroupChoice<'a>) -> Option<u64> {
        match group.group_entries.first().map(|(e, _)| e)? {
            GroupEntry::ValueMemberKey { ge, .. }
                if ge.member_key.is_none() && ge.occur.is_none() =>
            {
                match ge.entry_type.type_choices.as_slice() {
                    [c] => self.lead(&c.type1),
                    _ => None,
                }
            }
            GroupEntry::TypeGroupname { ge, .. } if ge.occur.is_none() => {
                match self.defs.get(ge.name.ident)? {
                    Def::Group(GroupEntry::InlineGroup { group, .. }) => {
                        match group.group_choices.as_slice() {
                            [one] => self.lead_group(one),
                            _ => None,
                        }
                    }
                    Def::Type(params, value) if params.is_empty() => {
                        match value.type_choices.as_slice() {
                            [c] => self.lead(&c.type1),
                            _ => None,
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// Whether every value is known and no two are equal.
fn distinct(leads: impl Iterator<Item = Option<u64>>) -> bool {
    let mut seen = BTreeSet::new();
    for lead in leads {
        match lead {
            Some(n) if seen.insert(n) => {}
            _ => return false,
        }
    }
    true
}

fn is_array(t: &Type1) -> bool {
    t.operator.is_none() && matches!(t.type2, Type2::Array { .. })
}

/// Whether two arms are one list written once under the set tag and once bare.
fn is_set(arms: &[&Type1]) -> bool {
    let [tagged, bare] = arms else {
        return false;
    };
    let Type2::TaggedData { tag, t, .. } = &tagged.type2 else {
        return false;
    };
    let inner = match t.type_choices.as_slice() {
        [c] if c.type1.operator.is_none() => &c.type1.type2,
        _ => return false,
    };
    let list = |t: &Type2| match t {
        Type2::Array { group, .. } => matches!(
            group.group_choices.as_slice(),
            [one] if matches!(one.group_entries.as_slice(), [(e, _)] if occur(e).is_some_and(|o| matches!(o, Occur::ZeroOrMore { .. } | Occur::OneOrMore { .. })))
        ),
        _ => false,
    };
    tag.as_ref().and_then(|c| c.as_literal()) == Some(SET_TAG)
        && bare.operator.is_none()
        && list(inner)
        && list(&bare.type2)
        && inner.to_string() == bare.type2.to_string()
}

fn occur<'e>(entry: &'e GroupEntry) -> Option<&'e Occur> {
    let occurrence = match entry {
        GroupEntry::ValueMemberKey { ge, .. } => ge.occur.as_ref(),
        GroupEntry::TypeGroupname { ge, .. } => ge.occur.as_ref(),
        GroupEntry::InlineGroup { occur, .. } => occur.as_ref(),
    };
    occurrence.map(|o| &o.occur)
}

/// Whether an entry stands for any number of values.
fn repeats(entry: &GroupEntry, scope: &Scope) -> bool {
    match occur(entry) {
        None | Some(Occur::Optional { .. }) => false,
        Some(Occur::ZeroOrMore { .. } | Occur::OneOrMore { .. }) => true,
        Some(other) => panic!("{}: the occurrence {other}", scope.rule),
    }
}

/// The prelude type a controlled value is a value of.
fn base<'a>(t: &Type1<'a>) -> &'a str {
    match &t.type2 {
        Type2::Typename {
            ident,
            generic_args: None,
            ..
        } => ident.ident,
        other => panic!("a control on {other}"),
    }
}
