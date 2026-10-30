//! The committed list of transaction constructs, each needing a judged record the ledger accepted.

use std::collections::{BTreeMap, BTreeSet};

use ledger_oracle_judge::record::RecordId;
use pallas::{
    codec::minicbor,
    crypto::hash::{Hash, Hasher},
    ledger::{
        addresses::byron::AddressPayload,
        primitives::dijkstra::{Guards, NativeScript, StakeCredential, WitnessSet},
        traverse::{Era, MultiEraTx},
    },
};

/// The address type and spending data header that precede a bootstrap key and chain code in an address root preimage.
const BOOTSTRAP_ROOT_HEADER: [u8; 6] = [0x83, 0x00, 0x82, 0x00, 0x58, 0x40];

/// Whether a level is the top level body or a sub transaction body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Depth {
    Top,
    Sub,
}

impl Depth {
    fn name(self) -> &'static str {
        match self {
            Depth::Top => "top",
            Depth::Sub => "sub",
        }
    }
}

/// The guards and witnesses of the top level body or of one sub transaction body.
struct Level<'a> {
    guards: Option<&'a Guards>,
    required_top_level_guards: bool,
    witnesses: &'a WitnessSet<'a>,
}

impl Level<'_> {
    fn vkey_hashes(&self) -> BTreeSet<Hash<28>> {
        let witnesses = self.witnesses.vkeywitness.iter().flat_map(|s| s.iter());
        witnesses.map(|w| Hasher::<224>::hash(&w.vkey)).collect()
    }

    fn bootstrap_hashes(&self) -> BTreeSet<Hash<28>> {
        let witnesses = self
            .witnesses
            .bootstrap_witness
            .iter()
            .flat_map(|s| s.iter());
        witnesses
            .map(|w| {
                let preimage = [
                    &BOOTSTRAP_ROOT_HEADER[..],
                    &w.public_key,
                    &w.chain_code,
                    &w.attributes,
                ];
                AddressPayload::hash_address_id_preimage(&preimage.concat())
            })
            .collect()
    }

    fn guard_keys(&self) -> BTreeSet<Hash<28>> {
        match self.guards {
            Some(Guards::AddrKeyhashes(keys)) => keys.iter().copied().collect(),
            Some(Guards::Credentials(credentials)) => credentials
                .iter()
                .filter_map(|c| match c {
                    StakeCredential::AddrKeyhash(key) => Some(*key),
                    StakeCredential::ScriptHash(_) => None,
                })
                .collect(),
            None => BTreeSet::new(),
        }
    }

    fn guard_script(&self) -> bool {
        match self.guards {
            Some(Guards::Credentials(credentials)) => credentials
                .iter()
                .any(|c| matches!(c, StakeCredential::ScriptHash(_))),
            Some(Guards::AddrKeyhashes(_)) | None => false,
        }
    }

    fn guard_met_only_by_bootstrap(&self) -> bool {
        let vkeys = self.vkey_hashes();
        let only = self.bootstrap_hashes();
        self.guard_keys()
            .iter()
            .any(|k| only.contains(k) && !vkeys.contains(k))
    }

    fn native_signature(&self) -> bool {
        let scripts = self.witnesses.native_script.iter().flat_map(|s| s.iter());
        scripts.into_iter().any(|s| signs(s))
    }

    fn bootstrap_attributes(&self) -> bool {
        let witnesses = self
            .witnesses
            .bootstrap_witness
            .iter()
            .flat_map(|s| s.iter());
        witnesses
            .into_iter()
            .any(|w| w.attributes.as_slice() != [0xa0])
    }
}

/// Whether a native script requires a signature anywhere in its tree.
fn signs(script: &NativeScript) -> bool {
    match script {
        NativeScript::ScriptPubkey(_) => true,
        NativeScript::ScriptAll(scripts)
        | NativeScript::ScriptAny(scripts)
        | NativeScript::ScriptNOfK(_, scripts) => scripts.iter().any(signs),
        NativeScript::InvalidBefore(_)
        | NativeScript::InvalidHereafter(_)
        | NativeScript::ScriptRequireGuard(_) => false,
    }
}

macro_rules! constructs {
    ($($construct:ident = $name:literal at $($depth:ident),+ when |$level:ident| $holds:expr),* $(,)?) => {
        /// A construct one level of a transaction can hold.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub(super) enum Construct {
            $($construct),*
        }

        impl Construct {
            /// Every construct of the list at each depth where the ledger accepts it.
            const ENTRIES: &'static [(Depth, Construct)] =
                &[$($((Depth::$depth, Construct::$construct)),+),*];

            fn name(self) -> &'static str {
                match self {
                    $(Construct::$construct => $name),*
                }
            }

            fn holds(self, level: &Level) -> bool {
                match self {
                    $(Construct::$construct => {
                        let $level = level;
                        $holds
                    }),*
                }
            }
        }
    };
}

// The ledger refuses a PlutusV1, V2 or V3 script in a sub transaction, so those witnesses are top level entries only.
constructs! {
    VKeyWitness = "witness_vkey" at Top, Sub when |l| l.witnesses.vkeywitness.is_some(),
    NativeScriptWitness = "witness_native_script" at Top, Sub
        when |l| l.witnesses.native_script.is_some(),
    BootstrapWitness = "witness_bootstrap" at Top, Sub
        when |l| l.witnesses.bootstrap_witness.is_some(),
    PlutusV1 = "witness_plutus_v1" at Top when |l| l.witnesses.plutus_v1_script.is_some(),
    Datum = "witness_datum" at Top, Sub when |l| l.witnesses.plutus_data.is_some(),
    Redeemer = "witness_redeemer" at Top, Sub when |l| l.witnesses.redeemer.is_some(),
    PlutusV2 = "witness_plutus_v2" at Top when |l| l.witnesses.plutus_v2_script.is_some(),
    PlutusV3 = "witness_plutus_v3" at Top when |l| l.witnesses.plutus_v3_script.is_some(),
    PlutusV4 = "witness_plutus_v4" at Top, Sub when |l| l.witnesses.plutus_v4_script.is_some(),
    BootstrapAttributes = "bootstrap_attributes_not_empty" at Top, Sub
        when |l| l.bootstrap_attributes(),
    GuardKey = "guard_key" at Top, Sub when |l| !l.guard_keys().is_empty(),
    GuardScript = "guard_script" at Top, Sub when |l| l.guard_script(),
    GuardByBootstrap = "guard_key_only_bootstrap" at Top, Sub
        when |l| l.guard_met_only_by_bootstrap(),
    RequiredTopLevelGuards = "required_top_level_guards" at Top, Sub
        when |l| l.required_top_level_guards,
    NativeSignature = "native_script_signature" at Top, Sub when |l| l.native_signature(),
}

/// The levels and constructs one transaction holds.
pub(super) fn of_tx(tx: &[u8]) -> Result<BTreeSet<(Depth, Construct)>, minicbor::decode::Error> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, tx)?;
    let Some(tx) = tx.as_dijkstra() else {
        return Err(minicbor::decode::Error::message(
            "not a Dijkstra transaction",
        ));
    };
    let body = &tx.transaction_body;
    let mut levels = vec![(
        Depth::Top,
        Level {
            guards: body.guards.as_ref(),
            required_top_level_guards: body.required_top_level_guards.is_some(),
            witnesses: &tx.transaction_witness_set,
        },
    )];
    for sub in body.sub_transactions.iter().flat_map(|s| s.iter()) {
        let sub_body = &sub.sub_transaction_body;
        levels.push((
            Depth::Sub,
            Level {
                guards: sub_body.guards.as_ref(),
                required_top_level_guards: sub_body.required_top_level_guards.is_some(),
                witnesses: &sub.transaction_witness_set,
            },
        ));
    }
    let held = levels.iter().flat_map(|(depth, level)| {
        let entries = Construct::ENTRIES.iter().filter(move |(d, _)| d == depth);
        entries.filter(|(_, c)| c.holds(level)).copied()
    });
    Ok(held.collect())
}

/// The answer Dolos gave a record the ledger accepted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Answer {
    Agree,
    Declared { label: String },
}

/// For each construct at each depth, a judged record the ledger accepted that holds it, one Dolos agreed on when there is one.
#[derive(Debug, Default)]
pub(super) struct Coverage(BTreeMap<(Depth, Construct), (Answer, RecordId)>);

impl Coverage {
    pub(super) fn add(&mut self, id: RecordId, answer: Answer, held: BTreeSet<(Depth, Construct)>) {
        for entry in held {
            self.insert(entry, (answer.clone(), id));
        }
    }

    pub(super) fn extend(&mut self, other: Coverage) {
        for (entry, evidence) in other.0 {
            self.insert(entry, evidence);
        }
    }

    fn insert(&mut self, entry: (Depth, Construct), evidence: (Answer, RecordId)) {
        let kept = self.0.entry(entry).or_insert_with(|| evidence.clone());
        if evidence.0 < kept.0 {
            *kept = evidence;
        }
    }

    /// The line of each construct at each depth, a count line, and whether every one has a record.
    pub(super) fn bar(&self) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let (mut covered, mut agreed) = (0, 0);
        let entries = Construct::ENTRIES;
        for (depth, construct) in entries {
            let name = format!("{}_{}", depth.name(), construct.name());
            lines.push(match self.0.get(&(*depth, *construct)) {
                Some((Answer::Agree, id)) => {
                    covered += 1;
                    agreed += 1;
                    format!("CONSTRUCT {name} record={id} dolos=agree")
                }
                Some((Answer::Declared { label }, id)) => {
                    covered += 1;
                    format!("CONSTRUCT {name} record={id} dolos=declared label={label}")
                }
                None => format!("CONSTRUCT_UNCOVERED {name}"),
            });
        }
        let pass = covered == entries.len();
        lines.push(format!(
            "CONSTRUCT covered={covered}/{} agreed={agreed} pass={pass}",
            entries.len()
        ));
        (lines, pass)
    }
}

#[cfg(test)]
mod tests {
    use ledger_oracle_judge::record::CorpusKind;
    use pallas::ledger::primitives::dijkstra::{BootstrapWitness, NonEmptySet, VKeyWitness};

    use super::*;

    fn every_entry() -> BTreeSet<(Depth, Construct)> {
        Construct::ENTRIES.iter().copied().collect()
    }

    fn supp(n: u32) -> RecordId {
        RecordId {
            corpus: CorpusKind::Supp,
            n,
        }
    }

    fn declared() -> Answer {
        Answer::Declared {
            label: "witness fields".to_string(),
        }
    }

    #[test]
    fn a_bar_with_a_record_for_every_entry_passes() {
        let mut coverage = Coverage::default();
        coverage.add(supp(3), Answer::Agree, every_entry());
        let (lines, pass) = coverage.bar();
        assert!(pass, "{lines:?}");
        assert_eq!(
            lines[0],
            "CONSTRUCT top_witness_vkey record=supp#3 dolos=agree"
        );
        let entries = every_entry().len();
        let last = format!("CONSTRUCT covered={entries}/{entries} agreed={entries} pass=true");
        assert_eq!(lines.last(), Some(&last));
    }

    #[test]
    fn an_agreed_record_is_kept_over_a_declared_one_in_either_order() {
        let entry = BTreeSet::from([(Depth::Top, Construct::VKeyWitness)]);
        for agreed_first in [true, false] {
            let mut coverage = Coverage::default();
            let answers = [(supp(1), Answer::Agree), (supp(2), declared())];
            let mut answers = answers.to_vec();
            if !agreed_first {
                answers.reverse();
            }
            for (id, answer) in answers {
                coverage.add(id, answer, entry.clone());
            }
            let (lines, _) = coverage.bar();
            assert_eq!(
                lines[0],
                "CONSTRUCT top_witness_vkey record=supp#1 dolos=agree"
            );
        }
    }

    #[test]
    fn a_declared_record_covers_an_entry_and_names_its_label() {
        let mut coverage = Coverage::default();
        coverage.add(supp(4), declared(), every_entry());
        let (lines, pass) = coverage.bar();
        assert!(pass);
        let first = "CONSTRUCT top_witness_vkey record=supp#4 dolos=declared label=witness fields";
        assert_eq!(lines[0], first);
        let entries = every_entry().len();
        let last = format!("CONSTRUCT covered={entries}/{entries} agreed=0 pass=true");
        assert_eq!(lines.last(), Some(&last));
    }

    #[test]
    fn a_bar_missing_one_entry_fails_and_names_it() {
        let mut held = every_entry();
        held.remove(&(Depth::Sub, Construct::GuardByBootstrap));
        let mut coverage = Coverage::default();
        coverage.add(supp(3), Answer::Agree, held);
        let (lines, pass) = coverage.bar();
        assert!(!pass);
        let uncovered: Vec<_> = lines.iter().filter(|l| l.contains("UNCOVERED")).collect();
        assert_eq!(
            uncovered,
            ["CONSTRUCT_UNCOVERED sub_guard_key_only_bootstrap"]
        );
    }

    /// The constructs each kept record of a corpus that pallas decodes holds, by record number.
    fn held(kind: CorpusKind) -> Vec<(u32, BTreeSet<(Depth, Construct)>)> {
        let corpus = super::super::load(kind);
        let records = corpus.kept().flat_map(|run| run.records());
        let held = records.filter_map(|r| Some((r.n, of_tx(&r.tx.0).ok()?)));
        held.collect()
    }

    fn witnesses(vkeys: &[&[u8]], attributes: &[u8]) -> WitnessSet<'static> {
        let vkeys = vkeys.iter().map(|k| VKeyWitness {
            vkey: k.to_vec().into(),
            signature: vec![0; 64].into(),
        });
        let bootstrap = BootstrapWitness {
            public_key: vec![1; 32].into(),
            signature: vec![0; 64].into(),
            chain_code: vec![2; 32].into(),
            attributes: attributes.to_vec().into(),
        };
        WitnessSet {
            vkeywitness: NonEmptySet::from_vec(vkeys.collect()),
            native_script: None,
            bootstrap_witness: NonEmptySet::from_vec(vec![bootstrap]),
            plutus_v1_script: None,
            plutus_data: None,
            redeemer: None,
            plutus_v2_script: None,
            plutus_v3_script: None,
            plutus_v4_script: None,
        }
    }

    fn guarded<'a>(witnesses: &'a WitnessSet<'a>, guards: &'a Guards) -> Level<'a> {
        Level {
            guards: Some(guards),
            required_top_level_guards: false,
            witnesses,
        }
    }

    fn key_guard(key: Hash<28>) -> Guards {
        Guards::AddrKeyhashes(NonEmptySet::from_vec(vec![key]).unwrap())
    }

    #[test]
    fn a_guard_key_a_vkey_provides_is_not_met_only_by_bootstrap() {
        let vkey = [9; 32];
        let witnesses = witnesses(&[&vkey], &[0xa0]);
        let guards = key_guard(Hasher::<224>::hash(&vkey));
        let level = guarded(&witnesses, &guards);
        assert!(Construct::GuardKey.holds(&level));
        assert!(!Construct::GuardByBootstrap.holds(&level));
    }

    #[test]
    fn a_guard_key_only_a_bootstrap_witness_provides_is_met_only_by_bootstrap() {
        let witnesses = witnesses(&[&[9; 32]], &[0xa0]);
        let unmet = key_guard(Hash::new([0; 28]));
        let roots = guarded(&witnesses, &unmet).bootstrap_hashes();
        let guards = key_guard(*roots.first().unwrap());
        assert!(Construct::GuardByBootstrap.holds(&guarded(&witnesses, &guards)));
        assert!(!Construct::GuardByBootstrap.holds(&guarded(&witnesses, &unmet)));
    }

    #[test]
    fn only_attribute_bytes_other_than_the_empty_map_are_not_empty() {
        let guards = key_guard(Hash::new([0; 28]));
        let empty = witnesses(&[], &[0xa0]);
        assert!(!Construct::BootstrapAttributes.holds(&guarded(&empty, &guards)));
        let held = witnesses(&[], &[0xa1, 0x03, 0x41, 0x00]);
        assert!(Construct::BootstrapAttributes.holds(&guarded(&held, &guards)));
    }

    #[test]
    fn the_supplement_holds_a_guard_met_only_by_bootstrap_at_both_depths() {
        let held = held(CorpusKind::Supp);
        for depth in [Depth::Top, Depth::Sub] {
            let entry = (depth, Construct::GuardByBootstrap);
            assert!(held.iter().any(|(_, h)| h.contains(&entry)), "{depth:?}");
        }
    }
}
