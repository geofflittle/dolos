use std::{collections::BTreeMap, fmt};

use crate::{
    judgement::{
        self, Axis, CellKey, EntryPoint, Finding, Instrument, Judge, Route, Verdict, Version,
    },
    manifest::{KnownList, Locus},
    record::{CorpusKind, RecordId},
    row::Row,
};

/// The result of comparing the ledger's bytes for one item with the node's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ByteVerdict {
    Equal,
    Differ { at: usize },
    LedgerOnly(Vec<u8>),
    NodeOnly(Vec<u8>),
}

impl ByteVerdict {
    /// Compares the ledger's bytes with the node's, either side possibly absent.
    pub fn of(ledger: Option<&[u8]>, node: Option<&[u8]>) -> Option<Self> {
        match (ledger, node) {
            (None, None) => None,
            (Some(l), node) => Some(ByteVerdict::against(l, node)),
            (None, Some(n)) => Some(ByteVerdict::NodeOnly(n.to_vec())),
        }
    }

    /// Compares the ledger's bytes with the node's, the node's possibly absent.
    pub fn against(ledger: &[u8], node: Option<&[u8]>) -> Self {
        match node {
            None => ByteVerdict::LedgerOnly(ledger.to_vec()),
            Some(n) => match first_difference(ledger, n) {
                None => ByteVerdict::Equal,
                Some(at) => ByteVerdict::Differ { at },
            },
        }
    }

    /// Whether both sides hold the same bytes.
    pub fn agrees(&self) -> bool {
        matches!(self, ByteVerdict::Equal)
    }

    /// The cell the comparison touches, which side holds bytes and whether they
    /// are equal.
    pub fn cell(&self) -> CellKey {
        let member = match self {
            ByteVerdict::Equal => "equal",
            ByteVerdict::Differ { .. } => "differ",
            ByteVerdict::LedgerOnly(_) => "ledger_only",
            ByteVerdict::NodeOnly(_) => "dolos_only",
        };
        CellKey::new(Axis::Bytes, member)
    }
}

impl fmt::Display for ByteVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ByteVerdict::Equal => write!(f, "equal"),
            ByteVerdict::Differ { at } => write!(f, "differ at byte {at}"),
            ByteVerdict::LedgerOnly(l) => write!(f, "ledger={} dolos=none", hex::encode(l)),
            ByteVerdict::NodeOnly(n) => write!(f, "ledger=none dolos={}", hex::encode(n)),
        }
    }
}

/// The index of the first byte where two byte strings differ.
pub fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    match a.iter().zip(b).position(|(x, y)| x != y) {
        Some(at) => Some(at),
        None if a.len() == b.len() => None,
        None => Some(a.len().min(b.len())),
    }
}

/// The number of items one comparison checked and the finding of each that
/// disagreed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tally<F = Finding> {
    pub compared: usize,
    pub disagree: Vec<F>,
}

impl<F> Default for Tally<F> {
    fn default() -> Self {
        Tally {
            compared: 0,
            disagree: Vec::new(),
        }
    }
}

impl<F> Tally<F> {
    /// Counts one compared item, keeping its finding when it disagrees.
    pub fn add(&mut self, agrees: bool, describe: impl FnOnce() -> F) {
        self.compared += 1;
        if !agrees {
            self.disagree.push(describe());
        }
    }

    /// Counts one compared item that agrees.
    pub fn agree(&mut self) {
        self.compared += 1;
    }

    /// Counts the items and keeps the disagreements of another tally.
    pub fn merge(&mut self, other: Tally<F>) {
        self.compared += other.compared;
        self.disagree.extend(other.disagree);
    }
}

impl Tally {
    /// Adds the verdicts of the tally as the judgement of `judge` at `entry`.
    pub fn report(&self, judge: Judge, entry: EntryPoint, out: &mut judgement::Tally) {
        let passed = judgement::Passed {
            judge,
            entry: Some(entry),
            cells: Vec::new(),
            pass: judgement::Pass::Agree,
        };
        out.pass(passed, self.compared - self.disagree.len());
        for finding in &self.disagree {
            out.add(finding.clone().verdict(judge, entry));
        }
    }
}

impl<F> Tally<F> {
    /// The tally without its findings.
    fn counted(&self) -> Self {
        Tally {
            compared: self.compared,
            disagree: Vec::new(),
        }
    }
}

/// The comparisons of one record that the known list settles.
pub trait Settled {
    /// The tally the comparisons of every record add to.
    type Total;

    /// Every entry point the comparisons drive.
    fn every() -> Vec<EntryPoint>;

    /// The tally of each entry point the comparisons drove.
    fn tallies(&self) -> Vec<(EntryPoint, &Tally)>;

    /// The comparisons without their findings.
    fn counted(&self) -> Self;

    /// Adds the comparisons to the tally of every record.
    fn add_to(self, total: &mut Self::Total);
}

/// The account comparisons of one record, which ReadAccounts answers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AccountChecks(pub Tally);

impl AccountChecks {
    /// The entry point that answers the account comparisons.
    pub const ENTRY: EntryPoint = EntryPoint::Instrument(Instrument::ReadAccounts);
}

impl Settled for AccountChecks {
    type Total = Tally;

    fn every() -> Vec<EntryPoint> {
        vec![AccountChecks::ENTRY]
    }

    fn tallies(&self) -> Vec<(EntryPoint, &Tally)> {
        vec![(AccountChecks::ENTRY, &self.0)]
    }

    fn counted(&self) -> Self {
        AccountChecks(self.0.counted())
    }

    fn add_to(self, total: &mut Tally) {
        total.merge(self.0);
    }
}

impl<R: ServedRoutes> Settled for ByRoute<R> {
    type Total = ByRoute<R>;

    fn every() -> Vec<EntryPoint> {
        Self::routes().map(|(route, version)| route.at(version)).collect()
    }

    fn tallies(&self) -> Vec<(EntryPoint, &Tally)> {
        self.0
            .iter()
            .map(|((route, version), tally)| (route.at(*version), tally))
            .collect()
    }

    fn counted(&self) -> Self {
        let counted = self.0.iter().map(|(at, t)| (*at, t.counted()));
        ByRoute(counted.collect())
    }

    fn add_to(self, total: &mut ByRoute<R>) {
        total.merge(self);
    }
}

/// A slot group whose block the node did not apply, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollFail {
    pub first: RecordId,
    pub last: RecordId,
    pub records: usize,
    pub slot: u64,
    pub cause: String,
}

impl fmt::Display for RollFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let RollFail {
            first,
            last,
            records,
            slot,
            cause,
        } = self;
        write!(
            f,
            "group={first}..{last} records={records} slot={slot} {cause}"
        )
    }
}

/// The routes that serve one kind of answer a judge compares.
pub trait ServedRoutes: Copy + Ord + fmt::Debug + 'static {
    /// Every route that serves the answer.
    const ALL: &'static [Self];

    /// The spec method of the route.
    fn route(self) -> Route;

    /// The entry point of the route at `version`.
    fn at(self, version: Version) -> EntryPoint {
        EntryPoint::Route(self.route(), version)
    }
}

macro_rules! served_routes {
    ($(#[$doc:meta])* $kind:ident { $($member:ident),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $kind {
            $($member),*
        }

        impl ServedRoutes for $kind {
            const ALL: &'static [Self] = &[$($kind::$member),*];

            fn route(self) -> Route {
                match self {
                    $($kind::$member => Route::$member),*
                }
            }
        }
    };
}

served_routes! {
    /// A route that serves the output each input of a transaction spends.
    OutputRoute { FetchBlock, FollowTip, DumpHistory, WatchTx, ReadTx }
}

served_routes! {
    /// A route that serves a transaction by its hash.
    TxRoute { ReadTx }
}

served_routes! {
    /// A route that serves an output the UTxO holds.
    UtxoRoute { ReadUtxos, SearchUtxos }
}

served_routes! {
    /// A route that serves the protocol parameters.
    ParamsRoute { ReadParams }
}

served_routes! {
    /// A route that answers for a transaction submitted to the mempool.
    SubmitRoute { SubmitTx, WaitForTx, WatchMempool }
}

/// The tallies of one judge, by the route and version each compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ByRoute<R>(pub BTreeMap<(R, Version), Tally>);

impl<R> Default for ByRoute<R> {
    fn default() -> Self {
        ByRoute(BTreeMap::new())
    }
}

impl<R: ServedRoutes> ByRoute<R> {
    /// Every route at each version the spec defines it.
    pub fn routes() -> impl Iterator<Item = (R, Version)> {
        R::ALL.iter().flat_map(|route| {
            let versions = Version::ALL.into_iter();
            let versions = versions.filter(|v| route.route().at(*v));
            versions.map(|version| (*route, version))
        })
    }

    /// The tally of `route` at `version`.
    pub fn at(&mut self, route: R, version: Version) -> &mut Tally {
        self.0.entry((route, version)).or_default()
    }

    /// Counts the items and keeps the disagreements of another set of tallies.
    pub fn merge(&mut self, other: ByRoute<R>) {
        for ((route, version), tally) in other.0 {
            self.at(route, version).merge(tally);
        }
    }

    /// The number of items compared at every route.
    pub fn compared(&self) -> usize {
        self.0.values().map(|t| t.compared).sum()
    }

    /// The findings of every route.
    pub fn disagree(&self) -> impl Iterator<Item = &Finding> {
        self.0.values().flat_map(|t| &t.disagree)
    }

    /// Adds the verdicts of each route as the judgement of `judge`, and the
    /// entry point of every route at each version as one it requires.
    fn report(
        &self,
        judge: Judge,
        out: &mut judgement::Tally,
        required: &mut Vec<(Judge, EntryPoint)>,
    ) {
        for ((route, version), tally) in &self.0 {
            tally.report(judge, route.at(*version), out);
        }
        required.extend(Self::routes().map(|(route, version)| (judge, route.at(version))));
    }
}

/// The state comparisons of one arm, one tally each.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StateTallies {
    pub roll_fail: Vec<RollFail>,
    pub utxo: ByRoute<UtxoRoute>,
    pub as_output: ByRoute<OutputRoute>,
    pub sub_only: usize,
    pub sub_unheld: usize,
    pub seeded_unjudged: usize,
    pub accounts: Tally,
    pub undo: Tally,
    pub revalidate: Tally,
    pub dependents: usize,
    pub params: ByRoute<ParamsRoute>,
    pub readtx: ByRoute<TxRoute>,
    pub script: Tally,
    pub datum: ByRoute<UtxoRoute>,
    pub plutus_data: Tally,
    pub served_tx: ByRoute<TxRoute>,
    pub submit: ByRoute<SubmitRoute>,
    pub known: Vec<String>,
    pub known_fired: KnownList,
    pub known_verdicts: Vec<Verdict>,
    pub known_stale: Vec<Verdict>,
}

impl StateTallies {
    /// Each tally a passing corpus compares at least once, by its judge and the
    /// entry point it drives.
    fn named(&self) -> [(Judge, EntryPoint, &Tally); 4] {
        let StateTallies {
            roll_fail: _,
            utxo: _,
            as_output: _,
            sub_only: _,
            sub_unheld: _,
            seeded_unjudged: _,
            accounts,
            undo,
            revalidate: _,
            dependents: _,
            params: _,
            readtx: _,
            script,
            datum: _,
            plutus_data,
            served_tx: _,
            submit: _,
            known: _,
            known_fired: _,
            known_verdicts: _,
            known_stale: _,
        } = self;
        [
            (Judge::Accounts, AccountChecks::ENTRY, accounts),
            (Judge::Undo, Instrument::ApplyBlock.into(), undo),
            (Judge::Script, Instrument::ScriptByHash.into(), script),
            (
                Judge::PlutusData,
                Instrument::PlutusData.into(),
                plutus_data,
            ),
        ]
    }

    /// Adds the verdicts of every tally, each block that did not roll forward
    /// and each known entry, and requires each named tally to have compared
    /// an item.
    pub fn report(&self, out: &mut judgement::Tally) {
        let revalidate = Instrument::Revalidate.into();
        let revalidate = (Judge::Revalidate, revalidate, &self.revalidate);
        let mut required = Vec::new();
        for (judge, entry, tally) in self.named().into_iter().chain([revalidate]) {
            tally.report(judge, entry, out);
            if judge != Judge::Revalidate {
                required.push((judge, entry));
            }
        }
        let routes = &mut required;
        self.readtx.report(Judge::ReadTx, out, routes);
        self.utxo.report(Judge::Utxo, out, routes);
        self.datum.report(Judge::Datum, out, routes);
        self.params.report(Judge::Params, out, routes);
        self.as_output.report(Judge::AsOutput, out, routes);
        self.served_tx.report(Judge::ServedTx, out, routes);
        self.submit.report(Judge::Submit, out, routes);
        for known in &self.known_verdicts {
            out.add(known.clone());
        }
        for fail in &self.roll_fail {
            out.add(Verdict::disagree(
                Judge::ApplyBlock,
                Instrument::ApplyBlock.into(),
                fail.first,
                vec![CellKey::new(Axis::Answer, "refused")],
                fail.to_string(),
            ));
        }
        for stale in &self.known_stale {
            out.add(stale.clone());
        }
        out.require(&required);
    }

    /// Adds the comparisons of another arm part after this one's.
    pub fn merge(&mut self, other: StateTallies) {
        let StateTallies {
            roll_fail,
            utxo,
            as_output,
            sub_only,
            sub_unheld,
            seeded_unjudged,
            accounts,
            undo,
            revalidate,
            dependents,
            params,
            readtx,
            script,
            datum,
            plutus_data,
            served_tx,
            submit,
            known,
            known_fired,
            known_verdicts,
            known_stale,
        } = other;
        self.roll_fail.extend(roll_fail);
        self.utxo.merge(utxo);
        self.as_output.merge(as_output);
        self.sub_only += sub_only;
        self.sub_unheld += sub_unheld;
        self.seeded_unjudged += seeded_unjudged;
        self.accounts.merge(accounts);
        self.undo.merge(undo);
        self.revalidate.merge(revalidate);
        self.dependents += dependents;
        self.params.merge(params);
        self.readtx.merge(readtx);
        self.script.merge(script);
        self.datum.merge(datum);
        self.plutus_data.merge(plutus_data);
        self.served_tx.merge(served_tx);
        self.submit.merge(submit);
        self.known.extend(known);
        self.known_fired.extend(known_fired);
        self.known_verdicts.extend(known_verdicts);
        self.known_stale.extend(known_stale);
    }

    /// The count of records in slot groups whose block the node did not apply.
    pub fn rolled_failed(&self) -> usize {
        self.roll_fail.iter().map(|r| r.records).sum()
    }

    /// Adds the served output comparisons of each record, passing a known record while it still disagrees.
    pub fn settle_as_output(
        &mut self,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, ByRoute<OutputRoute>>,
        known: &KnownList,
    ) {
        self.settle(Locus::AsOutput, |s| &mut s.as_output, kind, checks, known);
    }

    /// Adds the account comparisons of each record, passing a known record while it still disagrees.
    pub fn settle_accounts(
        &mut self,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, AccountChecks>,
        known: &KnownList,
    ) {
        self.settle(Locus::Accounts, |s| &mut s.accounts, kind, checks, known);
    }

    /// Adds the comparisons of each record to the tally of `locus`, passing a
    /// known record while it still disagrees, with a verdict at each entry
    /// point the known entry names a record of.
    fn settle<T: Settled>(
        &mut self,
        locus: Locus,
        tally_of: fn(&mut StateTallies) -> &mut T::Total,
        kind: CorpusKind,
        checks: BTreeMap<RecordId, T>,
        known: &KnownList,
    ) {
        let judge = locus.judge();
        let stale = |id: RecordId, entry: EntryPoint, row: &Row, detail: String| {
            let cells = vec![
                CellKey::new(Axis::Failure, "known_stale"),
                CellKey::new(Axis::Row, row),
            ];
            Verdict::disagree(judge, entry, id, cells, detail)
        };
        let ids: Vec<RecordId> = checks.keys().copied().collect();
        for (id, checked) in checks {
            let Some(row) = known.get(&(locus, id)) else {
                checked.add_to(tally_of(self));
                continue;
            };
            let tallies = checked.tallies();
            let disagree = tallies.iter().filter(|(_, t)| !t.disagree.is_empty());
            let disagree: Vec<&(EntryPoint, &Tally)> = disagree.collect();
            if disagree.is_empty() {
                let detail = format!("row={row} locus={locus}");
                let entries = tallies.iter().map(|(entry, _)| *entry);
                let entries: Vec<EntryPoint> = entries.collect();
                checked.add_to(tally_of(self));
                for entry in entries {
                    self.known_stale.push(stale(id, entry, row, detail.clone()));
                }
                continue;
            }
            for (entry, tally) in &disagree {
                let lines = tally.disagree.iter();
                let lines = lines.map(|d| format!("row={row} ids={id} {locus} {}", d.detail));
                self.known.extend(lines);
                self.known_verdicts.push(Verdict {
                    judge,
                    entry: Some(*entry),
                    subject: judgement::Subject::Record(id),
                    cells: vec![CellKey::new(Axis::Row, row)],
                    outcome: judgement::Outcome::Known(format!("row={row}")),
                });
            }
            self.known_fired.insert((locus, id), *row);
            checked.counted().add_to(tally_of(self));
        }
        for ((at, id), row) in known {
            if *at == locus && id.corpus == kind && !ids.contains(id) {
                let detail = format!("row={row} locus={locus} not_compared");
                for entry in T::every() {
                    self.known_stale.push(stale(*id, entry, row, detail.clone()));
                }
            }
        }
    }
}

impl fmt::Display for StateTallies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let StateTallies {
            roll_fail: _,
            utxo,
            as_output,
            sub_only,
            sub_unheld,
            seeded_unjudged,
            accounts,
            undo,
            revalidate,
            dependents,
            params,
            readtx,
            script,
            datum,
            plutus_data,
            served_tx,
            submit,
            known,
            known_fired: _,
            known_verdicts: _,
            known_stale: _,
        } = self;
        write!(
            f,
            "STATE compared utxo={} as_output={} of sub_only={sub_only} \
             sub_unheld={sub_unheld} \
             seeded_unjudged={seeded_unjudged} accounts={} undo={} revalidate={} \
             of dependents={dependents} params={} readtx={} script={} datum={} \
             plutus_data={} served_tx={} submit={} rolled_failed={}",
            utxo.compared(),
            as_output.compared(),
            accounts.compared,
            undo.compared,
            revalidate.compared,
            params.compared(),
            readtx.compared(),
            script.compared,
            datum.compared(),
            plutus_data.compared,
            served_tx.compared(),
            submit.compared(),
            self.rolled_failed()
        )?;
        for line in known {
            write!(f, "\nKNOWN {line}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row::Row;

    #[test]
    fn first_difference_names_the_byte_or_the_shorter_length() {
        assert_eq!(first_difference(b"abc", b"abc"), None);
        assert_eq!(first_difference(b"abc", b"abd"), Some(2));
        assert_eq!(first_difference(b"ab", b"abc"), Some(2));
        assert_eq!(first_difference(b"", b""), None);
    }

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    /// Comparisons of each record at FetchBlock v1, each agreeing once and disagreeing once per line.
    fn checks(entries: &[(u32, &[&str])]) -> BTreeMap<RecordId, ByRoute<OutputRoute>> {
        let mut checks = BTreeMap::new();
        for (n, disagree) in entries {
            let by_route: &mut ByRoute<OutputRoute> = checks.entry(supp(*n)).or_default();
            let tally = by_route.at(OutputRoute::FetchBlock, Version::V1);
            tally.agree();
            for d in *disagree {
                tally.add(false, || Finding::new(supp(*n), Vec::new(), *d));
            }
        }
        checks
    }

    fn account_checks(diff: &str) -> BTreeMap<RecordId, AccountChecks> {
        let mut tally = Tally::default();
        tally.agree();
        tally.add(false, || Finding::new(supp(9), Vec::new(), diff));
        BTreeMap::from([(supp(9), AccountChecks(tally))])
    }

    fn compared() -> StateTallies {
        let mut state = StateTallies {
            utxo: agreed_at(),
            params: agreed_at(),
            datum: agreed_at(),
            as_output: agreed_at(),
            served_tx: agreed_at(),
            readtx: agreed_at(),
            submit: agreed_at(),
            ..Default::default()
        };
        state.accounts.agree();
        state.undo.agree();
        state.revalidate.agree();
        state.script.agree();
        state.plutus_data.agree();
        state
    }

    /// One agreeing comparison at every route, at every version.
    fn agreed_at<R: ServedRoutes>() -> ByRoute<R> {
        let mut agreed = ByRoute::default();
        for (route, version) in ByRoute::<R>::routes() {
            agreed.at(route, version).agree();
        }
        agreed
    }

    /// The number of entry points the routes of `R` make.
    fn entries<R: ServedRoutes>() -> usize {
        ByRoute::<R>::routes().count()
    }

    #[test]
    fn a_served_route_judged_at_one_version_alone_fails_the_run() {
        let mut state = compared();
        assert!(passes(&state));
        let v1beta = (OutputRoute::FetchBlock, Version::V1beta);
        state.as_output.0.remove(&v1beta);
        let out = reported(&state);
        assert!(!passes(&state));
        let failing: Vec<String> = out.failing.iter().map(Verdict::to_string).collect();
        assert_eq!(failing.len(), 1, "{failing:?}");
        assert!(
            failing[0].contains("judge=as-output entry=fetch_block:v1beta"),
            "{failing:?}"
        );
    }

    /// The verdicts the state reports.
    fn reported(state: &StateTallies) -> judgement::Tally {
        let mut out = judgement::Tally::default();
        state.report(&mut out);
        out
    }

    /// Whether the verdicts the state reports pass a run.
    fn passes(state: &StateTallies) -> bool {
        matches!(
            reported(state).verdict(),
            judgement::RunVerdict::Pass { .. }
        )
    }

    fn tally(lines: &[&str]) -> Tally {
        Tally {
            compared: 2 * lines.len(),
            disagree: lines
                .iter()
                .map(|l| Finding::new(supp(0), Vec::new(), *l))
                .collect(),
        }
    }

    /// The tally of the lines at the first route of `R`, at v1.
    fn by_route<R: ServedRoutes>(lines: &[&str]) -> ByRoute<R> {
        ByRoute(BTreeMap::from([((R::ALL[0], Version::V1), tally(lines))]))
    }

    fn stale(line: &str) -> Verdict {
        Verdict::disagree(
            Judge::Accounts,
            Instrument::ReadAccounts.into(),
            supp(0),
            Vec::new(),
            line,
        )
    }

    fn roll_fail(n: u32) -> RollFail {
        RollFail {
            first: supp(n),
            last: supp(n),
            records: 1,
            slot: n.into(),
            cause: format!("cause{n}"),
        }
    }

    fn part(n: u32, row: Row) -> StateTallies {
        let line = format!("line{n}");
        let lines = [line.as_str()];
        StateTallies {
            roll_fail: vec![roll_fail(n)],
            utxo: by_route(&lines),
            as_output: by_route(&lines),
            sub_only: 1,
            sub_unheld: 4,
            seeded_unjudged: 2,
            accounts: tally(&lines),
            undo: tally(&lines),
            revalidate: tally(&lines),
            dependents: 3,
            params: by_route(&lines),
            readtx: by_route(&lines),
            script: tally(&lines),
            datum: by_route(&lines),
            plutus_data: tally(&lines),
            served_tx: by_route(&lines),
            submit: by_route(&lines),
            known: vec![line.clone()],
            known_fired: BTreeMap::from([((Locus::Accounts, supp(n)), row)]),
            known_verdicts: vec![stale(&format!("known{n}"))],
            known_stale: vec![stale(&line)],
        }
    }

    #[test]
    fn a_merged_part_adds_every_count_and_keeps_every_line_after_the_first_part() {
        let mut merged = part(0, Row::S1);
        merged.merge(part(1, Row::S2));
        let lines = ["line0", "line1"];
        let both = StateTallies {
            roll_fail: vec![roll_fail(0), roll_fail(1)],
            utxo: by_route(&lines),
            as_output: by_route(&lines),
            sub_only: 2,
            sub_unheld: 8,
            seeded_unjudged: 4,
            accounts: tally(&lines),
            undo: tally(&lines),
            revalidate: tally(&lines),
            dependents: 6,
            params: by_route(&lines),
            readtx: by_route(&lines),
            script: tally(&lines),
            datum: by_route(&lines),
            plutus_data: tally(&lines),
            served_tx: by_route(&lines),
            submit: by_route(&lines),
            known: vec!["line0".to_string(), "line1".to_string()],
            known_fired: BTreeMap::from([
                ((Locus::Accounts, supp(0)), Row::S1),
                ((Locus::Accounts, supp(1)), Row::S2),
            ]),
            known_verdicts: vec![stale("known0"), stale("known1")],
            known_stale: vec![stale("line0"), stale("line1")],
        };
        assert_eq!(merged, both);
    }

    #[test]
    fn merging_an_empty_part_changes_nothing() {
        let mut merged = part(0, Row::S1);
        merged.merge(StateTallies::default());
        assert_eq!(merged, part(0, Row::S1));
    }

    #[test]
    fn a_comparison_that_looked_at_nothing_fails() {
        let mut state = compared();
        state.as_output.at(OutputRoute::FetchBlock, Version::V1).agree();
        assert!(passes(&state), "{state}");
        type Emptied = (fn(&mut StateTallies), usize);
        let empties: [Emptied; 11] = [
            (|s| s.utxo = ByRoute::default(), entries::<UtxoRoute>()),
            (
                |s| s.as_output = ByRoute::default(),
                entries::<OutputRoute>(),
            ),
            (|s| s.accounts = Tally::default(), 1),
            (|s| s.undo = Tally::default(), 1),
            (|s| s.params = ByRoute::default(), entries::<ParamsRoute>()),
            (|s| s.readtx = ByRoute::default(), entries::<TxRoute>()),
            (|s| s.script = Tally::default(), 1),
            (|s| s.datum = ByRoute::default(), entries::<UtxoRoute>()),
            (|s| s.plutus_data = Tally::default(), 1),
            (|s| s.served_tx = ByRoute::default(), entries::<TxRoute>()),
            (|s| s.submit = ByRoute::default(), entries::<SubmitRoute>()),
        ];
        for (empty, failing) in empties {
            let mut state = state.clone();
            empty(&mut state);
            let out = reported(&state);
            assert_eq!(out.failing.len(), failing, "{out}");
            let judged = out.failing.iter().map(|v| v.to_string());
            assert!(judged.clone().all(|v| v.ends_with("judged=0")), "{out}");
        }
        let expected = format!(
            " seeded_unjudged=0 accounts=1 undo=1 revalidate=1 of dependents=0 params={} \
             readtx={} script=1 datum={} plutus_data=1 served_tx={}",
            entries::<ParamsRoute>(),
            entries::<TxRoute>(),
            entries::<UtxoRoute>(),
            entries::<TxRoute>()
        );
        assert!(state.to_string().contains(&expected), "{state}");
    }

    #[test]
    fn a_corpus_with_no_dependent_passes_and_a_kept_dependent_fails() {
        let mut state = compared();
        state.as_output.at(OutputRoute::FetchBlock, Version::V1).agree();
        state.revalidate = Tally::default();
        assert!(passes(&state), "{state}");
        state.revalidate.agree();
        assert!(passes(&state), "{state}");
        let cells = vec![CellKey::new(Axis::Rollback, "kept")];
        let line = "kept_after_rollback_to=Slot(40)";
        state
            .revalidate
            .add(false, || Finding::new(supp(2), cells, line));
        assert!(!passes(&state), "{state}");
        let text = reported(&state).to_string();
        assert!(
            text.contains(&format!(
                "VERDICT judge=revalidate entry=revalidate record=supp#2 \
                 cells=rollback:kept outcome=disagree {line}\n"
            )),
            "{text}"
        );
    }

    #[test]
    fn a_group_that_did_not_roll_prints_one_line_and_fails() {
        let mut state = compared();
        state.as_output.at(OutputRoute::FetchBlock, Version::V1).agree();
        assert!(passes(&state), "{state}");
        assert!(state.to_string().contains(" submit=9 rolled_failed=0"));
        assert!(reported(&state).failing.is_empty());
        state.roll_fail.push(RollFail {
            first: supp(3),
            last: supp(5),
            records: 3,
            slot: 40,
            cause: "panic at=a.rs:1:2 explicit panic".to_string(),
        });
        assert!(!passes(&state), "{state}");
        assert!(state.to_string().ends_with(" submit=9 rolled_failed=3"));
        let text = reported(&state).to_string();
        assert!(
            text.contains(
                "VERDICT judge=apply-block entry=apply_block record=supp#3 cells=answer:refused \
                 outcome=disagree group=supp#3..supp#5 records=3 slot=40 panic at=a.rs:1:2 explicit panic\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn agreeing_records_off_the_known_list_print_nothing_and_pass() {
        let mut state = compared();
        let before = state.as_output.compared();
        state.settle_as_output(
            CorpusKind::Supp,
            checks(&[(1, &[]), (2, &[])]),
            &KnownList::new(),
        );
        assert_eq!(state.as_output.compared(), before + 2);
        assert!(state.known.is_empty() && state.known_stale.is_empty());
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_known_record_passes_while_it_disagrees() {
        let mut state = compared();
        let before = state.as_output.compared();
        let known = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        let diff = "ledger=ab dolos=none input=00#1 services=FetchBlock";
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[diff])]), &known);
        assert_eq!(
            state.known,
            vec![format!("row=S3 ids=supp#9 as_output {diff}")]
        );
        assert_eq!(state.known_fired, known);
        assert_eq!(state.as_output.disagree().count(), 0);
        assert_eq!(state.as_output.compared(), before + 2);
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_known_record_that_agrees_or_was_not_compared_is_stale() {
        let mut state = StateTallies::default();
        let known = KnownList::from([
            ((Locus::AsOutput, supp(9)), Row::S3),
            ((Locus::AsOutput, supp(10)), Row::S3),
            (
                (
                    Locus::AsOutput,
                    RecordId {
                        corpus: CorpusKind::Ic,
                        n: 9,
                    },
                ),
                Row::S3,
            ),
            ((Locus::Validation, supp(11)), Row::V1),
        ]);
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[])]), &known);
        let lines: Vec<String> = state.known_stale.iter().map(Verdict::to_string).collect();
        let agreed = "VERDICT judge=as-output entry=fetch_block:v1 record=supp#9 \
                      cells=failure:known_stale,row:S3 outcome=disagree row=S3 locus=as_output";
        let unjudged = |entry: EntryPoint| {
            format!(
                "VERDICT judge=as-output entry={entry} record=supp#10 \
                 cells=failure:known_stale,row:S3 outcome=disagree row=S3 locus=as_output not_compared"
            )
        };
        let mut expected = vec![agreed.to_string()];
        expected.extend(ByRoute::<OutputRoute>::every().into_iter().map(unjudged));
        assert_eq!(lines, expected);
        assert_eq!(lines.len(), 1 + entries::<OutputRoute>());
        assert!(state.known_fired.is_empty() && state.known_verdicts.is_empty());
        assert!(!passes(&state));
    }

    #[test]
    fn a_known_record_is_reported_at_each_route_that_disagrees_and_no_other() {
        let mut state = compared();
        let known = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        let mut checked = ByRoute::default();
        for (route, version) in ByRoute::<OutputRoute>::routes() {
            checked.at(route, version).agree();
        }
        let diff = "ledger=ab dolos=none input=00#1 services=DumpHistory";
        let finding = || Finding::new(supp(9), Vec::new(), diff);
        checked
            .at(OutputRoute::DumpHistory, Version::V1alpha)
            .add(false, finding);
        let checks = BTreeMap::from([(supp(9), checked)]);
        state.settle_as_output(CorpusKind::Supp, checks, &known);
        let out = reported(&state);
        let reported_known: Vec<(Option<String>, usize)> = out
            .passed(Judge::AsOutput)
            .filter(|(passed, _)| passed.pass == judgement::Pass::Known)
            .map(|(passed, n)| (passed.entry.map(|e| e.to_string()), n))
            .collect();
        let dump_history = Some("dump_history:v1alpha".to_string());
        assert_eq!(reported_known, [(dump_history, 1)], "{state}");
        assert_eq!(state.known_fired, known);
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn an_account_disagreement_off_the_known_list_fails_and_a_known_one_passes() {
        let diff = "account=keyHash-00 ledger=balance=0,pool=none,drep=none dolos=balance=0";
        let mut state = compared();
        state.as_output.at(OutputRoute::FetchBlock, Version::V1).agree();
        let as_output = KnownList::from([((Locus::AsOutput, supp(9)), Row::S3)]);
        state.settle_accounts(CorpusKind::Supp, account_checks(diff), &as_output);
        assert_eq!(
            state.accounts.disagree,
            vec![Finding::new(supp(9), Vec::new(), diff)]
        );
        assert!(state.known.is_empty());
        assert!(!passes(&state), "{state}");

        let mut state = compared();
        state.as_output.at(OutputRoute::FetchBlock, Version::V1).agree();
        let known = KnownList::from([((Locus::Accounts, supp(9)), Row::S18)]);
        state.settle_accounts(CorpusKind::Supp, account_checks(diff), &known);
        assert_eq!(
            state.known,
            vec![format!("row=S18 ids=supp#9 accounts {diff}")]
        );
        assert_eq!(state.known_fired, known);
        assert!(state.accounts.disagree.is_empty());
        assert!(passes(&state), "{state}");
    }

    #[test]
    fn a_disagreement_off_the_known_list_fails_with_its_record() {
        let mut state = compared();
        let diff = "ledger=ab dolos=none input=00#1 services=FetchBlock";
        state.settle_as_output(CorpusKind::Supp, checks(&[(9, &[diff])]), &KnownList::new());
        let disagree: Vec<&Finding> = state.as_output.disagree().collect();
        assert_eq!(disagree, [&Finding::new(supp(9), Vec::new(), diff)]);
        let out = reported(&state);
        assert_eq!(out.failing.len(), 1, "{out}");
        assert_eq!(
            out.failing[0].to_string(),
            format!("VERDICT judge=as-output entry=fetch_block:v1 record=supp#9 cells= outcome=disagree {diff}")
        );
    }

    #[test]
    fn byte_verdict_prints_each_side() {
        assert_eq!(
            ByteVerdict::of(Some(b"\x01"), Some(b"\x01")),
            Some(ByteVerdict::Equal)
        );
        assert_eq!(
            ByteVerdict::of(Some(b"\x01\x02"), Some(b"\x01\x03"))
                .unwrap()
                .to_string(),
            "differ at byte 1"
        );
        assert_eq!(
            ByteVerdict::of(Some(b"\xab"), None).unwrap().to_string(),
            "ledger=ab dolos=none"
        );
        assert_eq!(ByteVerdict::of(None, None), None);
    }
}
