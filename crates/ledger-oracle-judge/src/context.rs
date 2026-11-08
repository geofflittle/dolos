//! Judges the arguments the evaluator applies each redeemer's script to against the ledger's, by level, purpose and value.

use std::{
    collections::{btree_map::Entry, BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    cbor::{self, Data},
    eval::{parse_purpose, Level, Refusal, RowKey},
    judgement::{self, CellKey, EntryPoint, Judge, Verdict},
    manifest::ManifestError,
    record::{Argument, CorpusKind, Evaluation, LedgerRedeemers, Record, RecordId, Redeemers},
};

const CONTEXT_KNOWN: &str = include_str!("../../../test_data/ledger-oracle/context-known.txt");

/// The ScriptContext fields of PlutusV1 and PlutusV2, in declaration order.
const V1_SCRIPT_CONTEXT: &[&str] = &["scriptContextTxInfo", "scriptContextPurpose"];

/// The ScriptContext fields of PlutusV3, in declaration order.
const V3_SCRIPT_CONTEXT: &[&str] = &[
    "scriptContextTxInfo",
    "scriptContextRedeemer",
    "scriptContextScriptInfo",
];

/// The ScriptContext fields of PlutusV4, in declaration order.
const V4_SCRIPT_CONTEXT: &[&str] = &[
    "scriptContextTxInfo",
    "scriptContextRedeemer",
    "scriptContextScriptInfo",
    "scriptContextScriptHash",
];

/// The TxInfo fields of PlutusV1, in declaration order.
const V1_TX_INFO: &[&str] = &[
    "txInfoInputs",
    "txInfoOutputs",
    "txInfoFee",
    "txInfoMint",
    "txInfoDCert",
    "txInfoWdrl",
    "txInfoValidRange",
    "txInfoSignatories",
    "txInfoData",
    "txInfoId",
];

/// The TxInfo fields of PlutusV2, in declaration order.
const V2_TX_INFO: &[&str] = &[
    "txInfoInputs",
    "txInfoReferenceInputs",
    "txInfoOutputs",
    "txInfoFee",
    "txInfoMint",
    "txInfoDCert",
    "txInfoWdrl",
    "txInfoValidRange",
    "txInfoSignatories",
    "txInfoRedeemers",
    "txInfoData",
    "txInfoId",
];

/// The TxInfo fields of PlutusV3, in declaration order.
const V3_TX_INFO: &[&str] = &[
    "txInfoInputs",
    "txInfoReferenceInputs",
    "txInfoOutputs",
    "txInfoFee",
    "txInfoMint",
    "txInfoTxCerts",
    "txInfoWdrl",
    "txInfoValidRange",
    "txInfoSignatories",
    "txInfoRedeemers",
    "txInfoData",
    "txInfoId",
    "txInfoVotes",
    "txInfoProposalProcedures",
    "txInfoCurrentTreasuryAmount",
    "txInfoTreasuryDonation",
];

/// The TxInfo fields of PlutusV4, in declaration order.
const V4_TX_INFO: &[&str] = &[
    "txInfoId",
    "txInfoSubTxIx",
    "txInfoInputs",
    "txInfoReferenceInputs",
    "txInfoOutputs",
    "txInfoMint",
    "txInfoTxCerts",
    "txInfoWithdrawals",
    "txInfoDirectDeposits",
    "txInfoAccountBalanceIntervals",
    "txInfoValidRange",
    "txInfoGuards",
    "txInfoRequiredTopLevelGuards",
    "txInfoRedeemers",
    "txInfoData",
    "txInfoVotes",
    "txInfoProposalProcedures",
    "txInfoCurrentTreasuryAmount",
    "txInfoTreasuryDonation",
];

/// The Plutus language of a script, which fixes the fields of its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Language {
    V1,
    V2,
    V3,
    V4,
}

impl Language {
    fn script_context(self) -> &'static [&'static str] {
        match self {
            Language::V1 | Language::V2 => V1_SCRIPT_CONTEXT,
            Language::V3 => V3_SCRIPT_CONTEXT,
            Language::V4 => V4_SCRIPT_CONTEXT,
        }
    }

    fn tx_info(self) -> &'static [&'static str] {
        match self {
            Language::V1 => V1_TX_INFO,
            Language::V2 => V2_TX_INFO,
            Language::V3 => V3_TX_INFO,
            Language::V4 => V4_TX_INFO,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Language::V1 => "V1",
            Language::V2 => "V2",
            Language::V3 => "V3",
            Language::V4 => "V4",
        })
    }
}

/// What the ledger built for one redeemer, the arguments of its context or why it built none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerContext {
    Built(Vec<Argument>),
    NotBuilt(Refusal),
}

/// The context the ledger built or refused for each redeemer of a record, or that its decoder refused the record's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerContexts {
    Read(BTreeMap<RowKey, LedgerContext>),
    Undecoded,
}

/// The contexts the ledger built or refused for a record's redeemers.
pub fn ledger_contexts(record: &Record) -> LedgerContexts {
    match &record.redeemers {
        LedgerRedeemers::Read(redeemers) => LedgerContexts::Read(redeemer_contexts(redeemers)),
        LedgerRedeemers::Undecoded => LedgerContexts::Undecoded,
    }
}

/// The context the ledger built or refused for each redeemer, at its top level and in each sub transaction.
fn redeemer_contexts(redeemers: &Redeemers) -> BTreeMap<RowKey, LedgerContext> {
    let top = redeemers.top.iter().map(|r| (Level::Top, r));
    let subs = redeemers
        .subs
        .iter()
        .enumerate()
        .flat_map(|(k, sub)| sub.iter().map(move |r| (Level::Sub(k as u32), r)));
    let context = |r: &crate::record::LedgerRedeemer| match (&r.args, &r.evaluation) {
        (Some(args), _) => LedgerContext::Built(args.clone()),
        (None, Evaluation::Failed(why)) => LedgerContext::NotBuilt(Refusal::new(why)),
        (None, ran) => LedgerContext::NotBuilt(Refusal::new(&ran.to_string())),
    };
    top.chain(subs)
        .map(|(level, r)| {
            let key = RowKey {
                level,
                purpose: r.purpose,
            };
            (key, context(r))
        })
        .collect()
}

/// The arguments the evaluator applies one redeemer's script to, as bytes, with the script's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PallasContext {
    pub language: Language,
    pub args: Vec<Vec<u8>>,
}

/// What the evaluator built for the redeemers of one transaction, or why it built none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PallasContexts {
    Built(BTreeMap<RowKey, PallasContext>),
    Refused(Refusal),
}

impl PallasContexts {
    /// The contexts of each row by key, or the first key built twice.
    pub fn built(rows: impl IntoIterator<Item = (RowKey, PallasContext)>) -> Result<Self, RowKey> {
        let mut by_key = BTreeMap::new();
        for (key, context) in rows {
            if by_key.insert(key, context).is_some() {
                return Err(key);
            }
        }
        Ok(PallasContexts::Built(by_key))
    }
}

/// What the known list gives a row: the differences it is known to show, or the ledger's refusal of a context only the evaluator builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    Differences(BTreeSet<String>),
    Refused(Refusal),
}

impl Listing {
    /// The known list lines of the listing after its row.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Listing::Differences(lines) => lines.iter().cloned().collect(),
            Listing::Refused(refusal) => vec![format!("refused {refusal}")],
        }
    }
}

/// The verdict of one row from the ledger's context, the evaluator's and the known list's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextVerdict {
    /// Both sides built arguments of equal value, with whether their bytes are equal too.
    Agree { bytes_equal: bool },
    /// The row shows exactly what the known list gives it.
    Known,
    /// The ledger built a context and the evaluator refused the transaction for this reason.
    Unbuilt(Refusal),
    /// The ledger built no context, and the evaluator built none or the ledger's decoder refused the transaction's bytes.
    NoLedgerContext,
    /// The arguments differ at each of these places, and from the listing when there is one.
    Differ {
        differences: Vec<String>,
        listed: Option<Listing>,
    },
    /// The evaluator built a context the ledger refused for this reason, and the listing when there is one.
    PallasOnly {
        ledger: Refusal,
        listed: Option<Listing>,
    },
    /// The row is listed and shows neither a difference nor a context only the evaluator builds.
    Stale(Listing),
}

/// What the evaluator built for one row.
#[derive(Clone, Copy)]
enum PallasRow<'a> {
    Built(&'a PallasContext),
    Missing,
    Refused(&'a Refusal),
}

/// Each row's verdict for one transaction, with the language of the evaluator's context, for every row the ledger, the evaluator or the known list names.
pub fn judge_tx(
    ledger: &LedgerContexts,
    pallas: &PallasContexts,
    listed: &BTreeMap<RowKey, Listing>,
) -> Vec<(RowKey, Option<Language>, ContextVerdict)> {
    let built = match pallas {
        PallasContexts::Built(rows) => rows.keys().copied().collect(),
        PallasContexts::Refused(_) => Vec::new(),
    };
    let read = match ledger {
        LedgerContexts::Read(rows) => rows.keys().copied().collect(),
        LedgerContexts::Undecoded => Vec::new(),
    };
    let keys: BTreeSet<RowKey> = read
        .into_iter()
        .chain(listed.keys().copied())
        .chain(built)
        .collect();
    let row = |key: &RowKey| match pallas {
        PallasContexts::Built(rows) => rows.get(key).map_or(PallasRow::Missing, PallasRow::Built),
        PallasContexts::Refused(refusal) => PallasRow::Refused(refusal),
    };
    keys.into_iter()
        .map(|key| {
            let row = row(&key);
            let language = match row {
                PallasRow::Built(context) => Some(context.language),
                PallasRow::Missing | PallasRow::Refused(_) => None,
            };
            let verdict = match ledger {
                LedgerContexts::Read(rows) => row_verdict(rows.get(&key), row, listed.get(&key)),
                LedgerContexts::Undecoded => undecoded_verdict(listed.get(&key)),
            };
            (key, language, verdict)
        })
        .collect()
}

/// The verdict of one row of a transaction whose bytes the ledger's decoder refused, for which the ledger built no context.
fn undecoded_verdict(listed: Option<&Listing>) -> ContextVerdict {
    match listed {
        None => ContextVerdict::NoLedgerContext,
        Some(listing) => ContextVerdict::Stale(listing.clone()),
    }
}

/// The verdict of one row from the ledger's context, what the evaluator built and the row's listing.
fn row_verdict(
    ledger: Option<&LedgerContext>,
    pallas: PallasRow,
    listed: Option<&Listing>,
) -> ContextVerdict {
    let unlisted = |verdict| match listed {
        None => verdict,
        Some(listing) => ContextVerdict::Stale(listing.clone()),
    };
    match (ledger, pallas) {
        (Some(LedgerContext::Built(args)), PallasRow::Built(context)) => {
            let bytes_equal = args.len() == context.args.len()
                && args.iter().zip(&context.args).all(|(l, p)| l.bytes == *p);
            by_differences(differences(args, context), bytes_equal, listed)
        }
        (Some(LedgerContext::Built(args)), PallasRow::Missing) => {
            let missing = format!("args ledger={} pallas=none", args.len());
            by_differences(vec![missing], false, listed)
        }
        (Some(LedgerContext::Built(_)), PallasRow::Refused(refusal)) => {
            unlisted(ContextVerdict::Unbuilt(refusal.clone()))
        }
        (ledger, PallasRow::Built(_)) => {
            let ledger = match ledger {
                Some(LedgerContext::NotBuilt(refusal)) => refusal.clone(),
                _ => Refusal::new("none"),
            };
            match listed {
                Some(Listing::Refused(listed)) if *listed == ledger => ContextVerdict::Known,
                _ => ContextVerdict::PallasOnly {
                    ledger,
                    listed: listed.cloned(),
                },
            }
        }
        (_, PallasRow::Missing | PallasRow::Refused(_)) => {
            unlisted(ContextVerdict::NoLedgerContext)
        }
    }
}

/// The verdict of a row both sides built a context for, from the places its arguments differ and its listing.
fn by_differences(
    differences: Vec<String>,
    bytes_equal: bool,
    listed: Option<&Listing>,
) -> ContextVerdict {
    match (differences.is_empty(), listed) {
        (true, None) => ContextVerdict::Agree { bytes_equal },
        (true, Some(listing)) => ContextVerdict::Stale(listing.clone()),
        (false, Some(Listing::Differences(lines))) if differences.iter().eq(lines) => {
            ContextVerdict::Known
        }
        (false, listed) => ContextVerdict::Differ {
            differences,
            listed: listed.cloned(),
        },
    }
}

/// The name of an argument by its position from the last, the context.
fn role(arg: usize, args: usize) -> String {
    match args - 1 - arg {
        0 => "context".to_string(),
        1 => "redeemer".to_string(),
        2 => "datum".to_string(),
        _ => format!("arg{arg}"),
    }
}

/// The field a path names in an argument, the ScriptContext and TxInfo fields of a context by the language's declaration order.
fn field(language: Language, role: &str, path: &[String]) -> String {
    let mut names = vec![role.to_string()];
    if role == "context" {
        let index = |n: Option<&String>| n.and_then(|n| n.parse::<usize>().ok());
        let context = index(path.first()).and_then(|i| language.script_context().get(i));
        names.extend(context.map(|name| name.to_string()));
        if let (Some(0), Some(i)) = (index(path.first()), index(path.get(1))) {
            names.extend(language.tx_info().get(i).map(|name| name.to_string()));
        }
    }
    names.join(".")
}

/// Each place the evaluator's arguments differ in value from the ledger's, as `<language> arg=<i> path=<p> field=<f> ledger=<v> pallas=<v>`.
fn differences(args: &[Argument], context: &PallasContext) -> Vec<String> {
    if args.len() != context.args.len() {
        return vec![format!(
            "args ledger={} pallas={}",
            args.len(),
            context.args.len()
        )];
    }
    let language = context.language;
    let mut out = Vec::new();
    for (i, (ledger, pallas)) in args.iter().zip(&context.args).enumerate() {
        let role = role(i, args.len());
        let mut places = Vec::new();
        match cbor::data(pallas) {
            Ok(pallas) => walk(&ledger.data, &pallas, &mut Vec::new(), &mut places),
            Err(_) => places.push((Vec::new(), ledger.data.to_string(), "undecoded".to_string())),
        }
        for (path, l, p) in places {
            let field = field(language, &role, &path);
            let path = match path.is_empty() {
                true => "root".to_string(),
                false => path.join("."),
            };
            out.push(format!(
                "{language} arg={i} path={path} field={field} ledger={l} pallas={p}"
            ));
        }
    }
    out
}

/// A path into an argument with the ledger's and the evaluator's value there.
type Place = (Vec<String>, String, String);

/// Adds each place under `path` where the two values differ, a constructor, list or map of the same shape by its items and anything else whole.
fn walk(ledger: &Data, pallas: &Data, path: &mut Vec<String>, out: &mut Vec<Place>) {
    match (ledger, pallas) {
        (Data::Constr(l, lf), Data::Constr(p, pf)) if l == p => items(lf, pf, path, out),
        (Data::List(l), Data::List(p)) => items(l, p, path, out),
        (Data::Map(l), Data::Map(p)) => {
            for i in 0..l.len().max(p.len()) {
                match (l.get(i), p.get(i)) {
                    (Some((lk, lv)), Some((pk, pv))) => {
                        path.push(format!("{i}.k"));
                        walk(lk, pk, path, out);
                        path.pop();
                        path.push(format!("{i}.v"));
                        walk(lv, pv, path, out);
                        path.pop();
                    }
                    (l, p) => {
                        let entry = |e: Option<&(Data, Data)>| {
                            e.map_or("none".to_string(), |(k, v)| format!("{k}:{v}"))
                        };
                        path.push(i.to_string());
                        out.push((path.clone(), entry(l), entry(p)));
                        path.pop();
                    }
                }
            }
        }
        _ if ledger == pallas => {}
        _ => out.push((path.clone(), ledger.to_string(), pallas.to_string())),
    }
}

/// Adds each place under `path` where two item lists differ, an item one side lacks as `none`.
fn items(ledger: &[Data], pallas: &[Data], path: &mut Vec<String>, out: &mut Vec<Place>) {
    for i in 0..ledger.len().max(pallas.len()) {
        path.push(i.to_string());
        match (ledger.get(i), pallas.get(i)) {
            (Some(l), Some(p)) => walk(l, p, path, out),
            (l, p) => {
                let item = |d: Option<&Data>| d.map_or("none".to_string(), Data::to_string);
                out.push((path.clone(), item(l), item(p)));
            }
        }
        path.pop();
    }
}

/// The listing each listed row of each record is known to show.
pub type ContextKnown = BTreeMap<RecordId, BTreeMap<RowKey, Listing>>;

/// Reads the context known list, one `<record> <level> <tag>#<index> <difference>` or `<record> <level> <tag>#<index> refused <ledger refusal>` per line.
pub fn parse_context_known(file: &'static str, text: &str) -> Result<ContextKnown, ManifestError> {
    let mut known = ContextKnown::new();
    let lines = text.lines().enumerate().map(|(i, l)| (i + 1, l.trim()));
    for (line, l) in lines.filter(|(_, l)| !l.is_empty()) {
        let error = |reason: String| ManifestError { file, line, reason };
        let mut words = l.splitn(4, ' ');
        let mut word = |what: &str| words.next().ok_or(error(format!("no {what}")));
        let id: RecordId = word("record")?.parse().map_err(error)?;
        let level: Level = word("level")?.parse().map_err(error)?;
        let purpose = parse_purpose(word("purpose")?).map_err(error)?;
        let listing = word("listing")?;
        let key = RowKey { level, purpose };
        let refused = listing.strip_prefix("refused ").map(Refusal::new);
        let difference = Refusal::new(listing).to_string();
        match (known.entry(id).or_default().entry(key), refused) {
            (Entry::Vacant(entry), Some(refusal)) => {
                entry.insert(Listing::Refused(refusal));
            }
            (Entry::Vacant(entry), None) => {
                entry.insert(Listing::Differences(BTreeSet::from([difference])));
            }
            (Entry::Occupied(mut entry), None) => match entry.get_mut() {
                Listing::Differences(lines) => {
                    if !lines.insert(difference.clone()) {
                        return Err(error(format!("{id} {key} {difference} listed twice")));
                    }
                }
                Listing::Refused(_) => {
                    return Err(error(format!(
                        "{id} {key} listed both as differences and as refused"
                    )))
                }
            },
            (Entry::Occupied(entry), Some(refusal)) => match entry.get() {
                Listing::Refused(_) => {
                    return Err(error(format!("{id} {key} refused {refusal} listed twice")))
                }
                Listing::Differences(_) => {
                    return Err(error(format!(
                        "{id} {key} listed both as differences and as refused"
                    )))
                }
            },
        }
    }
    Ok(known)
}

/// The committed context known list.
pub fn context_known() -> Result<ContextKnown, ManifestError> {
    parse_context_known("context-known.txt", CONTEXT_KNOWN)
}

/// The number of compared rows of one group that drew each verdict.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    pub agree: usize,
    pub known: usize,
    pub differ: usize,
}

impl Counts {
    fn merge(&mut self, other: Counts) {
        self.agree += other.agree;
        self.known += other.known;
        self.differ += other.differ;
    }
}

impl fmt::Display for Counts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.agree, self.known, self.differ)
    }
}

/// The row verdicts of the context judgement of one or more corpora, with each line that fails it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContextTally {
    pub agree: usize,
    pub known: usize,
    pub differ: usize,
    pub unbuilt: usize,
    pub no_ledger_context: usize,
    pub pallas_only: usize,
    pub bytes_equal: usize,
    pub languages: BTreeMap<Language, Counts>,
    pub levels: BTreeMap<&'static str, Counts>,
    pub failing: Vec<Verdict>,
    pub judged: BTreeSet<RecordId>,
}

/// The cell of the field a difference line names, its path's last component.
fn field_cell(line: &str) -> Option<CellKey> {
    let field = line.split(' ').find_map(|w| w.strip_prefix("field="))?;
    Some(CellKey::new("field", field.rsplit('.').next()?))
}

impl ContextTally {
    /// Keeps a failing verdict for each line of one row of record `id`, or
    /// one for the row when it has no line, each with the row's cells, the
    /// answer kind and the field a line names.
    fn fail(
        &mut self,
        id: RecordId,
        key: &RowKey,
        language: &Option<Language>,
        answer: &'static str,
        lines: impl Iterator<Item = String>,
    ) {
        let mut lines = lines.map(Some).peekable();
        let row = lines.peek().is_none().then_some(None);
        for line in lines.chain(row) {
            let mut cells = key.cells();
            cells.extend(language.map(|l| CellKey::new("language", l)));
            cells.push(CellKey::new("answer", answer));
            cells.extend(line.as_deref().and_then(field_cell));
            let detail = match line {
                Some(line) => format!("{key} {line}"),
                None => key.to_string(),
            };
            let entry = EntryPoint::RedeemerArguments;
            self.failing
                .push(Verdict::disagree(Judge::Context, entry, id, cells, detail));
        }
    }

    /// Counts each row verdict of one record, keeping a failing verdict for each line that fails.
    pub fn add(&mut self, id: RecordId, verdicts: &[(RowKey, Option<Language>, ContextVerdict)]) {
        self.judged.insert(id);
        for (key, language, verdict) in verdicts {
            let mut counts = Counts::default();
            match verdict {
                ContextVerdict::Agree { bytes_equal } => {
                    counts.agree += 1;
                    self.bytes_equal += usize::from(*bytes_equal);
                }
                ContextVerdict::Known => counts.known += 1,
                ContextVerdict::Unbuilt(_) => self.unbuilt += 1,
                ContextVerdict::NoLedgerContext => self.no_ledger_context += 1,
                ContextVerdict::Differ {
                    differences,
                    listed,
                } => {
                    counts.differ += 1;
                    let listed = listed.iter().flat_map(Listing::lines);
                    let lines = differences
                        .iter()
                        .cloned()
                        .chain(listed.map(|l| format!("listed {l}")));
                    self.fail(id, key, language, "differ", lines);
                }
                ContextVerdict::PallasOnly { ledger, listed } => {
                    self.pallas_only += 1;
                    let listed = listed.iter().flat_map(Listing::lines);
                    let lines = std::iter::once(format!("refused {ledger}"))
                        .chain(listed.map(|l| format!("listed {l}")));
                    self.fail(id, key, language, "pallas_only", lines);
                }
                ContextVerdict::Stale(listing) => {
                    counts.differ += 1;
                    let lines = listing
                        .lines()
                        .into_iter()
                        .map(|l| format!("{l} not shown"));
                    self.fail(id, key, language, "stale", lines);
                }
            }
            self.agree += counts.agree;
            self.known += counts.known;
            self.differ += counts.differ;
            if counts != Counts::default() {
                if let Some(language) = language {
                    self.languages.entry(*language).or_default().merge(counts);
                }
                self.levels
                    .entry(key.level.kind())
                    .or_default()
                    .merge(counts);
            }
        }
    }

    /// Fails each listed row of a record of the corpus the judgement did not reach.
    pub fn unjudged(&mut self, known: &ContextKnown, corpus: CorpusKind) {
        let missed = known
            .iter()
            .filter(|(id, _)| id.corpus == corpus && !self.judged.contains(id));
        let missed: Vec<(RecordId, RowKey, Vec<String>)> = missed
            .flat_map(|(id, rows)| rows.iter().map(|(key, l)| (*id, *key, l.lines())))
            .collect();
        for (id, key, lines) in missed {
            let lines = lines.into_iter().map(|l| format!("{l} not judged"));
            self.fail(id, &key, &None, "unjudged", lines);
        }
    }

    /// Adds the counts and lines of another part after this one's.
    pub fn merge(&mut self, other: ContextTally) {
        let ContextTally {
            agree,
            known,
            differ,
            unbuilt,
            no_ledger_context,
            pallas_only,
            bytes_equal,
            languages,
            levels,
            failing,
            judged,
        } = other;
        self.agree += agree;
        self.known += known;
        self.differ += differ;
        self.unbuilt += unbuilt;
        self.no_ledger_context += no_ledger_context;
        self.pallas_only += pallas_only;
        self.bytes_equal += bytes_equal;
        for (language, counts) in languages {
            self.languages.entry(language).or_default().merge(counts);
        }
        for (level, counts) in levels {
            self.levels.entry(level).or_default().merge(counts);
        }
        self.failing.extend(failing);
        self.judged.extend(judged);
    }

    /// Adds the passing count and the failing verdicts to `out`.
    pub fn report(&self, out: &mut judgement::Tally) {
        out.count(Judge::Context, self.agree + self.known);
        self.failing.iter().cloned().for_each(|v| out.add(v));
    }
}

impl fmt::Display for ContextTally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let join = |items: Vec<String>| items.join(",");
        let languages = join(
            self.languages
                .iter()
                .map(|(l, c)| format!("{l}:{c}"))
                .collect(),
        );
        let levels = join(
            self.levels
                .iter()
                .map(|(l, c)| format!("{l}:{c}"))
                .collect(),
        );
        write!(
            f,
            "CONTEXT compared={} agree={} differ={} known={} unbuilt={} no_ledger_context={} pallas_only={} bytes_equal={} by_language={languages} by_level={levels}",
            self.agree + self.known + self.differ,
            self.agree,
            self.differ,
            self.known,
            self.unbuilt,
            self.no_ledger_context,
            self.pallas_only,
            self.bytes_equal,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cbor::Purpose, record::CorpusKind};

    fn key(level: Level, tag: u64, index: u64) -> RowKey {
        RowKey {
            level,
            purpose: Purpose { tag, index },
        }
    }

    fn ic(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Ic,
            n,
        }
    }

    fn bytes(text: &str) -> Vec<u8> {
        hex::decode(text.replace(' ', "")).unwrap()
    }

    fn argument(text: &str) -> Argument {
        let bytes = bytes(text);
        let data = cbor::data(&bytes).unwrap();
        Argument { bytes, data }
    }

    fn built(args: &[&str]) -> LedgerContext {
        LedgerContext::Built(args.iter().map(|a| argument(a)).collect())
    }

    fn pallas(language: Language, args: &[&str]) -> PallasContext {
        PallasContext {
            language,
            args: args.iter().map(|a| bytes(a)).collect(),
        }
    }

    fn one(
        ledger: Option<LedgerContext>,
        pallas: PallasContexts,
        listed: Option<Listing>,
    ) -> ContextVerdict {
        let row = key(Level::Top, 0, 0);
        let ledger = LedgerContexts::Read(ledger.into_iter().map(|l| (row, l)).collect());
        let listed = listed.into_iter().map(|l| (row, l)).collect();
        let verdicts = judge_tx(&ledger, &pallas, &listed);
        assert_eq!(verdicts.len(), 1);
        verdicts.into_iter().next().unwrap().2
    }

    fn evaluated(row: PallasContext) -> PallasContexts {
        PallasContexts::Built(BTreeMap::from([(key(Level::Top, 0, 0), row)]))
    }

    /// A V3 context whose TxInfo has sixteen fields, the fee the fourth, with the fee given.
    fn v3_context(fee: u8, indefinite: bool) -> String {
        let fields = format!("80808018{fee:02x}a080a0d8798080a0a041aba080d87a80d87a80");
        let info = match indefinite {
            true => format!("d8799f{fields}ff"),
            false => format!("d87990{fields}"),
        };
        format!("d87983{info}00d87980")
    }

    #[test]
    fn a_value_in_another_encoding_agrees_and_counts_its_bytes_apart() {
        let ledger = v3_context(7, true);
        let other = v3_context(7, false);
        assert_eq!(
            one(
                Some(built(&[&ledger])),
                evaluated(pallas(Language::V3, &[&other])),
                None
            ),
            ContextVerdict::Agree { bytes_equal: false }
        );
        assert_eq!(
            one(
                Some(built(&[&ledger])),
                evaluated(pallas(Language::V3, &[&ledger])),
                None
            ),
            ContextVerdict::Agree { bytes_equal: true }
        );
    }

    #[test]
    fn a_changed_field_is_a_difference_named_by_its_field() {
        let verdict = one(
            Some(built(&[&v3_context(7, true)])),
            evaluated(pallas(Language::V3, &[&v3_context(8, false)])),
            None,
        );
        let ContextVerdict::Differ {
            differences,
            listed,
        } = verdict
        else {
            panic!("{verdict:?}");
        };
        assert_eq!(listed, None);
        assert_eq!(
            differences,
            vec!["V3 arg=0 path=0.3 field=context.scriptContextTxInfo.txInfoFee ledger=7 pallas=8"]
        );
    }

    #[test]
    fn each_argument_of_a_v2_spend_is_named_from_the_end() {
        let ctx = "d87982d8798c808080a080a080d8798080808080d879814100";
        let verdict = one(
            Some(built(&["01", "02", ctx])),
            evaluated(pallas(Language::V2, &["01", "03", ctx])),
            None,
        );
        let ContextVerdict::Differ { differences, .. } = verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(
            differences,
            vec!["V2 arg=1 path=root field=redeemer ledger=2 pallas=3"]
        );
    }

    #[test]
    fn a_v4_sub_transaction_index_is_named_in_the_v4_table() {
        let info = |ix: &str| {
            format!("93 00 {ix} 808080a0a0a0a0a0 d87980 80 80 a0 a0 a0 80 d87a80 d87a80")
        };
        let ctx = |ix: &str| format!("84 {} 00 d87980 41ab", info(ix));
        let verdict = one(
            Some(built(&[&ctx("d8798100")])),
            evaluated(pallas(Language::V4, &[&ctx("d87a80")])),
            None,
        );
        let ContextVerdict::Differ { differences, .. } = verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(
            differences,
            vec!["V4 arg=0 path=0.1 field=context.scriptContextTxInfo.txInfoSubTxIx ledger=C0[0] pallas=C1[]"]
        );
    }

    #[test]
    fn a_shorter_list_differs_at_each_element_it_lacks() {
        let verdict = one(
            Some(built(&["83010203"])),
            evaluated(pallas(Language::V3, &["820102"])),
            None,
        );
        let ContextVerdict::Differ { differences, .. } = verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(
            differences,
            vec!["V3 arg=0 path=2 field=context.scriptContextScriptInfo ledger=3 pallas=none"]
        );
    }

    #[test]
    fn a_row_listed_with_exactly_its_differences_is_known_and_any_other_list_fails() {
        let ledger = Some(built(&[&v3_context(7, true)]));
        let row = || evaluated(pallas(Language::V3, &[&v3_context(8, true)]));
        let line =
            "V3 arg=0 path=0.3 field=context.scriptContextTxInfo.txInfoFee ledger=7 pallas=8";
        let listing =
            |lines: &[&str]| Listing::Differences(lines.iter().map(|l| l.to_string()).collect());
        assert_eq!(
            one(ledger.clone(), row(), Some(listing(&[line]))),
            ContextVerdict::Known
        );
        let other = line.replace("pallas=8", "pallas=9");
        for listed in [listing(&[&other]), listing(&[line, &other])] {
            let verdict = one(ledger.clone(), row(), Some(listed.clone()));
            assert_eq!(
                verdict,
                ContextVerdict::Differ {
                    differences: vec![line.to_string()],
                    listed: Some(listed),
                }
            );
        }
    }

    #[test]
    fn a_listed_row_that_agrees_is_stale() {
        let ctx = v3_context(7, true);
        let listed =
            Listing::Differences(["V3 arg=0 path=0.3 ledger=7 pallas=8".to_string()].into());
        let verdict = one(
            Some(built(&[&ctx])),
            evaluated(pallas(Language::V3, &[&ctx])),
            Some(listed.clone()),
        );
        assert_eq!(verdict, ContextVerdict::Stale(listed));
    }

    #[test]
    fn a_context_only_pallas_builds_fails_unless_listed_with_the_ledger_refusal() {
        let refused = Some(LedgerContext::NotBuilt(Refusal::new("ContextError  bad")));
        let row = || evaluated(pallas(Language::V3, &["00"]));
        assert_eq!(
            one(refused.clone(), row(), None),
            ContextVerdict::PallasOnly {
                ledger: Refusal::new("ContextError bad"),
                listed: None,
            }
        );
        let exact = Listing::Refused(Refusal::new("ContextError bad"));
        assert_eq!(
            one(refused.clone(), row(), Some(exact)),
            ContextVerdict::Known
        );
        let other = Listing::Refused(Refusal::new("ContextError other"));
        assert_eq!(
            one(refused.clone(), row(), Some(other.clone())),
            ContextVerdict::PallasOnly {
                ledger: Refusal::new("ContextError bad"),
                listed: Some(other),
            }
        );
        assert_eq!(
            one(None, row(), None),
            ContextVerdict::PallasOnly {
                ledger: Refusal::new("none"),
                listed: None,
            }
        );
    }

    #[test]
    fn a_context_neither_side_or_only_the_ledger_builds_is_counted_apart() {
        let pallas_refused = || PallasContexts::Refused(Refusal::new("no script"));
        let ledger_refused = LedgerContext::NotBuilt(Refusal::new("ContextError bad"));
        assert_eq!(
            one(Some(built(&["00"])), pallas_refused(), None),
            ContextVerdict::Unbuilt(Refusal::new("no script"))
        );
        assert_eq!(
            one(Some(ledger_refused.clone()), pallas_refused(), None),
            ContextVerdict::NoLedgerContext
        );
        let none = PallasContexts::Built(BTreeMap::new());
        assert_eq!(
            one(Some(ledger_refused), none, None),
            ContextVerdict::NoLedgerContext
        );
    }

    #[test]
    fn a_context_pallas_builds_for_bytes_the_ledger_could_not_decode_has_no_ledger_context() {
        let row = key(Level::Top, 0, 1);
        let built =
            || PallasContexts::Built(BTreeMap::from([(row, pallas(Language::V3, &["00"]))]));
        let verdicts = judge_tx(&LedgerContexts::Undecoded, &built(), &BTreeMap::new());
        assert_eq!(
            verdicts,
            vec![(row, Some(Language::V3), ContextVerdict::NoLedgerContext)]
        );
        let refused = PallasContexts::Refused(Refusal::new("no script"));
        assert_eq!(
            judge_tx(&LedgerContexts::Undecoded, &refused, &BTreeMap::new()),
            vec![]
        );
        let listing = Listing::Refused(Refusal::new("none"));
        let listed = BTreeMap::from([(row, listing.clone())]);
        assert_eq!(
            judge_tx(&LedgerContexts::Undecoded, &built(), &listed),
            vec![(row, Some(Language::V3), ContextVerdict::Stale(listing))]
        );
    }

    #[test]
    fn a_row_pallas_evaluates_without_a_context_differs_by_its_arguments() {
        let verdict = one(
            Some(built(&["00", "01"])),
            PallasContexts::Built(BTreeMap::new()),
            None,
        );
        assert_eq!(
            verdict,
            ContextVerdict::Differ {
                differences: vec!["args ledger=2 pallas=none".to_string()],
                listed: None,
            }
        );
    }

    #[test]
    fn undecodable_pallas_bytes_differ_at_their_root() {
        let verdict = one(
            Some(built(&["00"])),
            evaluated(pallas(Language::V3, &["6161"])),
            None,
        );
        let ContextVerdict::Differ { differences, .. } = verdict else {
            panic!("{verdict:?}");
        };
        assert_eq!(
            differences,
            vec!["V3 arg=0 path=root field=context ledger=0 pallas=undecoded"]
        );
    }

    #[test]
    fn the_tally_line_counts_each_row_once_and_fails_on_a_difference_or_stale_listing() {
        let mut tally = ContextTally::default();
        let agree = |bytes_equal| ContextVerdict::Agree { bytes_equal };
        let top = key(Level::Top, 0, 0);
        let sub = key(Level::Sub(0), 1, 2);
        tally.add(
            ic(1),
            &[
                (top, Some(Language::V3), agree(true)),
                (sub, Some(Language::V4), agree(false)),
            ],
        );
        tally.add(
            ic(2),
            &[
                (top, None, ContextVerdict::Unbuilt(Refusal::new("x"))),
                (sub, None, ContextVerdict::NoLedgerContext),
            ],
        );
        let passes = |t: &ContextTally| {
            let mut out = judgement::Tally::default();
            t.report(&mut out);
            out.verdict().to_string().starts_with("RESULT PASS")
        };
        let lines = |t: &ContextTally| t.failing.iter().map(Verdict::to_string).collect::<Vec<_>>();
        assert!(passes(&tally));
        assert_eq!(
            tally.to_string(),
            "CONTEXT compared=2 agree=2 differ=0 known=0 unbuilt=1 no_ledger_context=1 pallas_only=0 bytes_equal=1 by_language=V3:1/0/0,V4:1/0/0 by_level=sub:1/0/0,top:1/0/0"
        );
        let mut failing = tally.clone();
        failing.add(
            ic(3),
            &[(
                top,
                Some(Language::V1),
                ContextVerdict::Differ {
                    differences: vec!["V1 arg=0 path=root field=datum ledger=1 pallas=2".into()],
                    listed: None,
                },
            )],
        );
        assert!(!passes(&failing));
        assert_eq!(
            lines(&failing),
            ["VERDICT judge=context entry=redeemer_arguments record=ic#3 cells=purpose:Spend,level:top,language:V1,answer:differ,field:datum outcome=disagree top Spend#0 V1 arg=0 path=root field=datum ledger=1 pallas=2"]
        );
        let known =
            parse_context_known("context-known.txt", "ic#9 top Spend#0 refused x\n").unwrap();
        let mut stale = tally.clone();
        stale.unjudged(&known, CorpusKind::Ic);
        assert!(!passes(&stale));
        assert_eq!(
            lines(&stale),
            ["VERDICT judge=context entry=redeemer_arguments record=ic#9 cells=purpose:Spend,level:top,answer:unjudged outcome=disagree top Spend#0 refused x not judged"]
        );
        let mut other = tally.clone();
        other.unjudged(&known, CorpusKind::Supp);
        assert!(passes(&other));
        let silent = [
            ContextVerdict::Differ {
                differences: vec![],
                listed: None,
            },
            ContextVerdict::Stale(Listing::Differences(BTreeSet::new())),
        ];
        for verdict in silent {
            let mut lineless = tally.clone();
            lineless.add(ic(4), &[(top, None, verdict.clone())]);
            assert!(!passes(&lineless), "{verdict:?}");
            assert_eq!(lineless.failing.len(), 1, "{verdict:?}");
        }
    }

    #[test]
    fn the_known_list_reads_differences_and_refusals_and_refuses_a_repeat_or_a_mix() {
        let text = "\nic#5 sub0 Mint#2 V4 arg=0 path=0.1 ledger=1 pallas=2\nic#5 sub0 Mint#2 V4 arg=0 path=0.2 ledger=1 pallas=2\nic#6 top Spend#0 refused  ContextError   bad\n";
        let known = parse_context_known("context-known.txt", text).unwrap();
        assert_eq!(
            known[&ic(5)][&key(Level::Sub(0), 1, 2)],
            Listing::Differences(
                [
                    "V4 arg=0 path=0.1 ledger=1 pallas=2".to_string(),
                    "V4 arg=0 path=0.2 ledger=1 pallas=2".to_string(),
                ]
                .into()
            )
        );
        assert_eq!(
            known[&ic(6)][&key(Level::Top, 0, 0)],
            Listing::Refused(Refusal::new("ContextError bad"))
        );
        let refused = |text: &str| {
            parse_context_known("context-known.txt", text)
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            refused("ic#5 top Spend#0 a\nic#5 top Spend#0 a\n"),
            "MANIFEST context-known.txt line=2 ic#5 top Spend#0 a listed twice"
        );
        assert_eq!(
            refused("ic#5 top Spend#0 a\nic#5 top Spend#0 refused b\n"),
            "MANIFEST context-known.txt line=2 ic#5 top Spend#0 listed both as differences and as refused"
        );
        assert_eq!(
            refused("ic#5 top Spend#0\n"),
            "MANIFEST context-known.txt line=1 no listing"
        );
    }

    #[test]
    fn the_committed_known_list_reads() {
        assert!(context_known().is_ok());
    }
}
