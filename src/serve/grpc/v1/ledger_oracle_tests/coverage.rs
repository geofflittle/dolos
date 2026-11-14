//! The members of the coverage table and the cells a transaction the ledger
//! accepted touches on its transaction axes.

use std::collections::BTreeSet;

use ledger_oracle_judge::{
    context::{language_members, Language},
    coverage::{Member, Reach},
    judgement::{Axis, CellKey, EntryPoint, RequestShape, Route},
    refusal::leaf_members,
    rules::Rules,
};
use pallas::{
    codec::minicbor,
    crypto::hash::{Hash, Hasher},
    ledger::{
        addresses::byron::AddressPayload,
        primitives::dijkstra::{
            Certificate, GovAction, Guards, NativeScript, NonEmptySet, ProposalProcedure,
            ProtocolParamUpdate, RedeemerTag, StakeCredential, SubTransactionBody, TransactionBody,
            WitnessSet,
        },
        traverse::{Era, MultiEraTx},
        validate::phase2::Builtin,
    },
};

/// The address type and spending data header that precede a bootstrap key and
/// chain code in an address root preimage.
const BOOTSTRAP_ROOT_HEADER: [u8; 6] = [0x83, 0x00, 0x82, 0x00, 0x58, 0x40];

/// Defines the name of every field of a struct and the names of the fields one
/// value holds, from a destructure that names every field.
macro_rules! keys {
    ($every:ident, $held:ident, $ty:ident { $($required:ident),* ; $($optional:ident),* $(,)? }) => {
        /// The name of every field.
        const $every: &[&str] = &[$(stringify!($required),)* $(stringify!($optional)),*];

        /// The names of the fields `value` holds.
        fn $held(value: &$ty) -> Vec<&'static str> {
            let $ty { $($required: _,)* $($optional),* } = value;
            let mut held: Vec<&'static str> = vec![$(stringify!($required)),*];
            $(
                if $optional.is_some() {
                    held.push(stringify!($optional));
                }
            )*
            held
        }
    };
}

/// Defines the name of every variant of an enum and the name of one value's
/// variant, from a match that names every variant.
macro_rules! kinds {
    ($every:ident, $kind:ident, $ty:ty { $($pattern:pat => $name:literal),* $(,)? }) => {
        /// The name of every variant.
        const $every: &[&str] = &[$($name),*];

        /// The name of the variant of `value`.
        fn $kind(value: &$ty) -> &'static str {
            match value {
                $($pattern => $name),*
            }
        }
    };
}

keys!(BODY_KEYS, body_keys, TransactionBody {
    inputs, outputs, fee;
    ttl, certificates, withdrawals, auxiliary_data_hash, validity_interval_start, mint,
    script_data_hash, collateral, guards, network_id, collateral_return, total_collateral,
    reference_inputs, voting_procedures, proposal_procedures, treasury_value, donation,
    sub_transactions, required_top_level_guards, direct_deposits, account_balance_intervals,
    starting_account_balance_intervals,
});

keys!(SUB_BODY_KEYS, sub_body_keys, SubTransactionBody {
    inputs, outputs;
    ttl, certificates, withdrawals, auxiliary_data_hash, validity_interval_start, mint,
    script_data_hash, guards, network_id, reference_inputs, voting_procedures,
    proposal_procedures, treasury_value, donation, required_top_level_guards, direct_deposits,
    account_balance_intervals,
});

keys!(WITNESS_KEYS, witness_keys, WitnessSet {
    ;
    vkeywitness, native_script, bootstrap_witness, plutus_v1_script, plutus_data, redeemer,
    plutus_v2_script, plutus_v3_script, plutus_v4_script,
});

keys!(PARAM_KEYS, param_keys, ProtocolParamUpdate {
    ;
    minfee_a, minfee_b, max_block_body_size, max_transaction_size, max_block_header_size,
    key_deposit, pool_deposit, maximum_epoch, desired_number_of_stake_pools,
    pool_pledge_influence, expansion_rate, treasury_growth_rate, min_pool_cost,
    ada_per_utxo_byte, cost_models_for_script_languages, execution_costs, max_tx_ex_units,
    max_block_ex_units, max_value_size, collateral_percentage, max_collateral_inputs,
    pool_voting_thresholds, drep_voting_thresholds, min_committee_size, committee_term_limit,
    governance_action_validity_period, governance_action_deposit, drep_deposit,
    drep_inactivity_period, minfee_refscript_cost_per_byte, max_ref_script_size_per_block,
    max_ref_script_size_per_tx, ref_script_cost_stride, ref_script_cost_multiplier,
    max_pledge_leverage, min_pool_margin, leios_announcement_period_length,
    leios_vote_period_length, leios_diffusion_period_length, leios_committee_size,
    leios_quorum_stake_threshold, max_endorser_block_references_size,
    max_endorser_block_txs_size, max_endorser_block_execution_units,
    max_ref_script_size_per_endorser_block, peras_min_candidate_block_age, peras_healing_factor,
    peras_cert_boost, peras_target_committee_size, peras_bootstrap_round,
    peras_quorum_threshold_safety_margin, ref_inputs_cost_per_multi_asset_policy,
    ref_inputs_cost_per_datum_byte,
});

kinds!(CERTIFICATES, certificate, Certificate {
    Certificate::StakeDelegation(..) => "stake_delegation",
    Certificate::PoolRegistration { .. } => "pool_registration",
    Certificate::PoolRetirement(..) => "pool_retirement",
    Certificate::Reg(..) => "reg",
    Certificate::UnReg(..) => "unreg",
    Certificate::VoteDeleg(..) => "vote_deleg",
    Certificate::StakeVoteDeleg(..) => "stake_vote_deleg",
    Certificate::StakeRegDeleg(..) => "stake_reg_deleg",
    Certificate::VoteRegDeleg(..) => "vote_reg_deleg",
    Certificate::StakeVoteRegDeleg(..) => "stake_vote_reg_deleg",
    Certificate::AuthCommitteeHot(..) => "auth_committee_hot",
    Certificate::ResignCommitteeCold(..) => "resign_committee_cold",
    Certificate::RegDRepCert(..) => "reg_drep",
    Certificate::UnRegDRepCert(..) => "unreg_drep",
    Certificate::UpdateDRepCert(..) => "update_drep",
});

kinds!(GOV_ACTIONS, gov_action, GovAction {
    GovAction::ParameterChange(..) => "parameter_change",
    GovAction::HardForkInitiation(..) => "hard_fork_initiation",
    GovAction::TreasuryWithdrawals(..) => "treasury_withdrawals",
    GovAction::NoConfidence(..) => "no_confidence",
    GovAction::UpdateCommittee(..) => "update_committee",
    GovAction::NewConstitution(..) => "new_constitution",
    GovAction::Information => "information",
});

kinds!(GUARDS, guard, Guards {
    Guards::AddrKeyhashes(_) => "addr_keyhashes",
    Guards::Credentials(_) => "credentials",
});

kinds!(CREDENTIALS, credential, StakeCredential {
    StakeCredential::AddrKeyhash(_) => "credential_key",
    StakeCredential::ScriptHash(_) => "credential_script",
});

kinds!(REDEEMERS, redeemer, RedeemerTag {
    RedeemerTag::Spend => "spend",
    RedeemerTag::Mint => "mint",
    RedeemerTag::Cert => "cert",
    RedeemerTag::Reward => "reward",
    RedeemerTag::Vote => "vote",
    RedeemerTag::Propose => "propose",
    RedeemerTag::Guarding => "guarding",
});

/// The members of the witness axis that no field of the witness set names.
const WITNESS_CONSTRUCTS: &[&str] = &["bootstrap_attributes", "native_signature"];

/// The member of the guard axis that no variant names.
const KEY_ONLY_BOOTSTRAP: &str = "key_only_bootstrap";

/// Whether a level is the top level body or a sub transaction body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Depth {
    Top,
    Sub,
}

impl Depth {
    const ALL: [Depth; 2] = [Depth::Top, Depth::Sub];

    fn name(self) -> &'static str {
        match self {
            Depth::Top => "top",
            Depth::Sub => "sub",
        }
    }

    fn body_keys(self) -> &'static [&'static str] {
        match self {
            Depth::Top => BODY_KEYS,
            Depth::Sub => SUB_BODY_KEYS,
        }
    }

    /// How the oracle reaches the witness member `key` at this depth.
    fn witness_reach(self, key: &str) -> Reach {
        match (self, key) {
            (Depth::Sub, "plutus_v1_script" | "plutus_v2_script" | "plutus_v3_script") => {
                Reach::Excluded {
                    reason: "the ledger refuses a PlutusV1, V2 or V3 script in a sub transaction"
                        .to_string(),
                    command: "git -C cardano-ledger show 9df9f98:eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxInfo.hs | grep -n -A1 'Attempt to use PlutusV1-V3 in a sub'".to_string(),
                }
            }
            _ => Reach::Counted,
        }
    }
}

/// The parts of the top level body or of one sub transaction body that the
/// transaction axes read.
struct Level<'a> {
    depth: Depth,
    keys: Vec<&'static str>,
    guards: Option<&'a Guards>,
    certificates: Vec<&'a Certificate>,
    actions: Vec<&'a GovAction>,
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

    fn guard_credentials(&self) -> Vec<&StakeCredential> {
        match self.guards {
            Some(Guards::Credentials(credentials)) => credentials.iter().collect(),
            Some(Guards::AddrKeyhashes(_)) | None => Vec::new(),
        }
    }

    fn guard_keys(&self) -> BTreeSet<Hash<28>> {
        let credentials = self
            .guard_credentials()
            .into_iter()
            .filter_map(|c| match c {
                StakeCredential::AddrKeyhash(key) => Some(*key),
                StakeCredential::ScriptHash(_) => None,
            });
        match self.guards {
            Some(Guards::AddrKeyhashes(keys)) => keys.iter().copied().collect(),
            Some(Guards::Credentials(_)) | None => credentials.collect(),
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

    fn signed(&self) -> bool {
        self.witnesses.vkeywitness.is_some() || self.witnesses.bootstrap_witness.is_some()
    }

    fn zero_budget(&self) -> bool {
        let values = self.witnesses.redeemer.iter().flat_map(|r| r.0.values());
        values
            .into_iter()
            .any(|v| v.ex_units.mem == 0 && v.ex_units.steps == 0)
    }

    fn guard_without_redeemer(&self) -> bool {
        let script_guard = self
            .guard_credentials()
            .into_iter()
            .any(|c| matches!(c, StakeCredential::ScriptHash(_)));
        let keys = self.witnesses.redeemer.iter().flat_map(|r| r.0.keys());
        let guarding = keys.into_iter().any(|k| k.tag == RedeemerTag::Guarding);
        script_guard && !guarding
    }

    fn reference_script_only(&self) -> bool {
        let w = self.witnesses;
        let redeemed = w.redeemer.as_ref().is_some_and(|r| !r.0.is_empty());
        let witnessed = w.plutus_v1_script.is_some()
            || w.plutus_v2_script.is_some()
            || w.plutus_v3_script.is_some()
            || w.plutus_v4_script.is_some();
        redeemed && !witnessed
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

    /// The cells of the transaction axes this level touches.
    fn cells(&self) -> Vec<CellKey> {
        let depth = self.depth.name();
        let at = |axis, name: &str| CellKey::new(axis, format!("{depth}.{name}"));
        let mut cells: Vec<CellKey> = self.keys.iter().map(|k| at(Axis::Body, k)).collect();
        let witnesses = witness_keys(self.witnesses).into_iter();
        cells.extend(witnesses.map(|k| at(Axis::Witness, k)));
        let constructs = [self.bootstrap_attributes(), self.native_signature()];
        let constructs = WITNESS_CONSTRUCTS.iter().zip(constructs);
        cells.extend(
            constructs
                .filter(|(_, held)| *held)
                .map(|(k, _)| at(Axis::Witness, k)),
        );
        cells.extend(self.guards.map(|g| at(Axis::Guard, guard(g))));
        let credentials = self.guard_credentials().into_iter();
        cells.extend(credentials.map(|c| at(Axis::Guard, credential(c))));
        if self.guard_met_only_by_bootstrap() {
            cells.push(at(Axis::Guard, KEY_ONLY_BOOTSTRAP));
        }
        let certificates = self.certificates.iter();
        cells.extend(certificates.map(|c| at(Axis::Certificate, certificate(c))));
        for action in &self.actions {
            cells.push(at(Axis::GovAction, gov_action(action)));
            if let GovAction::ParameterChange(_, update, _) = action {
                let keys = param_keys(update).into_iter();
                cells.extend(keys.map(|k| CellKey::new(Axis::ParamKey, k)));
            }
        }
        let tags = self.witnesses.redeemer.iter().flat_map(|r| r.0.keys());
        cells.extend(tags.map(|k| at(Axis::Redeemer, redeemer(&k.tag))));
        cells
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

/// The governance action of each proposal procedure of one body.
fn actions(proposals: &Option<NonEmptySet<ProposalProcedure>>) -> Vec<&GovAction> {
    let proposals = proposals.iter().flat_map(|s| s.iter());
    proposals.map(|p| &p.gov_action).collect()
}

/// Whether the levels of one transaction show the request shape by their
/// content, never for a shape a judge gives by deriving its request.
fn shows(shape: RequestShape, levels: &[Level]) -> bool {
    let mut levels = levels.iter();
    match shape {
        RequestShape::Draft | RequestShape::RewardOutputDraft => false,
        RequestShape::Signed => levels.any(Level::signed),
        RequestShape::Unsigned => !levels.any(Level::signed),
        RequestShape::ZeroBudget => levels.any(Level::zero_budget),
        RequestShape::GuardWithoutRedeemer => levels.any(Level::guard_without_redeemer),
        RequestShape::ReferenceScriptOnly => levels.any(Level::reference_script_only),
    }
}

/// The request shape cells the content of the Dijkstra transaction `tx` shows.
pub(super) fn shape_cells(tx: &[u8]) -> Result<Vec<CellKey>, minicbor::decode::Error> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, tx)?;
    let levels = levels(&tx)?;
    let shown = RequestShape::ALL.into_iter().filter(|s| shows(*s, &levels));
    Ok(shown.map(|s| CellKey::new(Axis::RequestShape, s)).collect())
}

/// The cells of the transaction axes the Dijkstra transaction `tx` touches.
pub(super) fn cells_of_tx(tx: &[u8]) -> Result<BTreeSet<CellKey>, minicbor::decode::Error> {
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, tx)?;
    Ok(levels(&tx)?.iter().flat_map(Level::cells).collect())
}

/// The top level body and each sub transaction body of the Dijkstra transaction
/// `tx`.
fn levels<'a>(tx: &'a MultiEraTx) -> Result<Vec<Level<'a>>, minicbor::decode::Error> {
    let Some(tx) = tx.as_dijkstra() else {
        return Err(minicbor::decode::Error::message(
            "not a Dijkstra transaction",
        ));
    };
    let body = &tx.transaction_body;
    let mut levels = vec![Level {
        depth: Depth::Top,
        keys: body_keys(body),
        guards: body.guards.as_ref(),
        certificates: body.certificates.iter().flat_map(|s| s.iter()).collect(),
        actions: actions(&body.proposal_procedures),
        witnesses: &tx.transaction_witness_set,
    }];
    for sub in body.sub_transactions.iter().flat_map(|s| s.iter()) {
        let sub_body = &sub.sub_transaction_body;
        levels.push(Level {
            depth: Depth::Sub,
            keys: sub_body_keys(sub_body),
            guards: sub_body.guards.as_ref(),
            certificates: sub_body
                .certificates
                .iter()
                .flat_map(|s| s.iter())
                .collect(),
            actions: actions(&sub_body.proposal_procedures),
            witnesses: &sub.transaction_witness_set,
        });
    }
    Ok(levels)
}

/// The builtin axis member for `builtin` in a script of `language`.
pub(super) fn builtin_member(language: Language, builtin: Builtin) -> String {
    format!("{language}.{builtin:?}")
}

/// The records whose verdicts a coverage axis counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Records {
    /// The records of the ledger corpora.
    Recorded,
    /// The mutants, each judged in a run of its own.
    Mutants,
}

/// How `records` reach an entry point, with the reason and command for each
/// route they do not reach.
pub(super) fn entry_reach(entry: EntryPoint, records: Records) -> Reach {
    let excluded = |reason: &str, command: &str| Reach::Excluded {
        reason: reason.to_string(),
        command: command.to_string(),
    };
    let route = match entry {
        EntryPoint::Instrument(_) => return Reach::Counted,
        EntryPoint::Route(route, _) => route,
    };
    match route {
        Route::FetchBlock | Route::FollowTip | Route::DumpHistory | Route::WatchTx
            if records == Records::Mutants =>
        {
            excluded(
                "a block serving route is judged only on an input an earlier record of the run made, and a mutant runs alone",
                "grep -n -e 'that an earlier record made' -e 'MUTATION a run judged' src/serve/grpc/v1/ledger_oracle_tests/mod.rs",
            )
        }
        Route::SubmitTx
        | Route::EvalTx
        | Route::WaitForTx
        | Route::WatchMempool
        | Route::FetchBlock
        | Route::FollowTip
        | Route::DumpHistory
        | Route::WatchTx
        | Route::ReadParams
        | Route::ReadUtxos
        | Route::SearchUtxos
        | Route::ReadTx => Reach::Counted,
        Route::ReadMempool => excluded(
            "row S23, ReadMempool is unimplemented",
            "grep -n 'read_mempool is not yet available' src/serve/grpc/v1/submit.rs",
        ),
        Route::ReadData => excluded(
            "row S14, ReadData panics on any call and serves nothing from chain state",
            "grep -n -A8 'async fn read_data' src/serve/grpc/v1/query.rs",
        ),
        Route::ReadTip => excluded(
            "row S24, the tip is judged only against a node",
            "grep -n -A3 'S24 =>' crates/ledger-oracle-judge/src/row.rs",
        ),
        Route::ReadGenesis | Route::ReadEraSummary | Route::ReadState => excluded(
            "a record holds no genesis file, era summary or stake distribution snapshot",
            "grep -n -A16 'pub struct Record ' crates/ledger-oracle-judge/src/record.rs",
        ),
    }
}

/// Every member of the coverage table, with the rule members `rules` gives.
pub(super) fn members(rules: &Rules) -> Vec<Member> {
    let mut members = Vec::new();
    for depth in Depth::ALL {
        let name = depth.name();
        let at = |axis: Axis, names: &'static [&'static str]| {
            names
                .iter()
                .map(move |n| Member::counted(axis, format!("{name}.{n}")))
        };
        members.extend(at(Axis::Body, depth.body_keys()));
        let witnesses = WITNESS_KEYS.iter().chain(WITNESS_CONSTRUCTS);
        members.extend(witnesses.map(|k| Member {
            cell: CellKey::new(Axis::Witness, format!("{name}.{k}")),
            reach: depth.witness_reach(k),
        }));
        members.extend(at(Axis::Guard, GUARDS));
        members.extend(at(Axis::Guard, CREDENTIALS));
        members.extend(at(Axis::Guard, &[KEY_ONLY_BOOTSTRAP]));
        members.extend(at(Axis::Certificate, CERTIFICATES));
        members.extend(at(Axis::GovAction, GOV_ACTIONS));
        members.extend(at(Axis::Redeemer, REDEEMERS));
    }
    members.extend(
        PARAM_KEYS
            .iter()
            .map(|k| Member::counted(Axis::ParamKey, k)),
    );
    for language in Language::ALL {
        let builtins = (0..=u8::MAX).filter_map(|tag| Builtin::try_from(tag).ok());
        let member = |builtin| Member::counted(Axis::Builtin, builtin_member(language, builtin));
        members.extend(builtins.map(member));
    }
    members.extend(leaf_members());
    members.extend(rules.members());
    let entries = EntryPoint::every().into_iter();
    members.extend(entries.map(|e| Member {
        cell: CellKey::new(Axis::Entry, e),
        reach: entry_reach(e, Records::Recorded),
    }));
    let shapes = RequestShape::ALL.into_iter();
    members.extend(shapes.map(|s| Member::counted(Axis::RequestShape, s)));
    members.extend(language_members());
    members
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ledger_oracle_judge::{record::CorpusKind, rules};
    use pallas::ledger::primitives::dijkstra::{BootstrapWitness, VKeyWitness};

    use super::*;

    /// The method names, in snake case, of every service the descriptor sets of
    /// one version declare.
    fn spec_methods(sets: [&[u8]; 4]) -> BTreeSet<String> {
        use prost::Message as _;
        let decoded = sets.into_iter().map(prost_types::FileDescriptorSet::decode);
        let files = decoded.flat_map(|set| set.expect("a spec descriptor set decodes").file);
        let methods = files.flat_map(|f| f.service).flat_map(|s| s.method);
        let snake = |name: &str| {
            let mut snake = String::new();
            for c in name.chars() {
                if c.is_uppercase() && !snake.is_empty() {
                    snake.push('_');
                }
                snake.push(c.to_ascii_lowercase());
            }
            snake
        };
        methods.map(|m| snake(m.name())).collect()
    }

    #[test]
    fn every_method_the_spec_declares_is_a_route_at_its_version() {
        use ledger_oracle_judge::judgement::Version;
        use pallas::interop::utxorpc::{v1, v1alpha, v1beta};
        let sets = [
            (
                Version::V1,
                [
                    v1::spec::query::FILE_DESCRIPTOR_SET,
                    v1::spec::submit::FILE_DESCRIPTOR_SET,
                    v1::spec::sync::FILE_DESCRIPTOR_SET,
                    v1::spec::watch::FILE_DESCRIPTOR_SET,
                ],
            ),
            (
                Version::V1beta,
                [
                    v1beta::spec::query::FILE_DESCRIPTOR_SET,
                    v1beta::spec::submit::FILE_DESCRIPTOR_SET,
                    v1beta::spec::sync::FILE_DESCRIPTOR_SET,
                    v1beta::spec::watch::FILE_DESCRIPTOR_SET,
                ],
            ),
            (
                Version::V1alpha,
                [
                    v1alpha::spec::query::FILE_DESCRIPTOR_SET,
                    v1alpha::spec::submit::FILE_DESCRIPTOR_SET,
                    v1alpha::spec::sync::FILE_DESCRIPTOR_SET,
                    v1alpha::spec::watch::FILE_DESCRIPTOR_SET,
                ],
            ),
        ];
        for (version, sets) in sets {
            let routes = Route::ALL.into_iter().filter(|r| r.at(version));
            let routes: BTreeSet<String> = routes.map(|r| r.name().to_string()).collect();
            assert_eq!(routes, spec_methods(sets), "{version}");
        }
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
            depth: Depth::Top,
            keys: Vec::new(),
            guards: Some(guards),
            certificates: Vec::new(),
            actions: Vec::new(),
            witnesses,
        }
    }

    fn key_guard(key: Hash<28>) -> Guards {
        Guards::AddrKeyhashes(NonEmptySet::from_vec(vec![key]).unwrap())
    }

    fn touches(level: &Level, axis: Axis, member: &str) -> bool {
        level.cells().contains(&CellKey::new(axis, member))
    }

    #[test]
    fn a_guard_key_a_vkey_provides_is_not_met_only_by_bootstrap() {
        let vkey = [9; 32];
        let witnesses = witnesses(&[&vkey], &[0xa0]);
        let guards = key_guard(Hasher::<224>::hash(&vkey));
        let level = guarded(&witnesses, &guards);
        assert!(touches(&level, Axis::Guard, "top.addr_keyhashes"));
        assert!(!touches(&level, Axis::Guard, "top.key_only_bootstrap"));
    }

    #[test]
    fn a_guard_key_only_a_bootstrap_witness_provides_is_met_only_by_bootstrap() {
        let witnesses = witnesses(&[&[9; 32]], &[0xa0]);
        let unmet = key_guard(Hash::new([0; 28]));
        let roots = guarded(&witnesses, &unmet).bootstrap_hashes();
        let guards = key_guard(*roots.first().unwrap());
        let bootstrap = "top.key_only_bootstrap";
        assert!(touches(
            &guarded(&witnesses, &guards),
            Axis::Guard,
            bootstrap
        ));
        assert!(!touches(
            &guarded(&witnesses, &unmet),
            Axis::Guard,
            bootstrap
        ));
    }

    #[test]
    fn a_credential_guard_touches_the_kind_of_each_credential() {
        let witnesses = witnesses(&[], &[0xa0]);
        let script = StakeCredential::ScriptHash(Hash::new([1; 28]));
        let guards = Guards::Credentials(NonEmptySet::from_vec(vec![script]).unwrap());
        let level = guarded(&witnesses, &guards);
        assert!(touches(&level, Axis::Guard, "top.credentials"));
        assert!(touches(&level, Axis::Guard, "top.credential_script"));
        assert!(!touches(&level, Axis::Guard, "top.credential_key"));
    }

    #[test]
    fn only_attribute_bytes_other_than_the_empty_map_are_not_empty() {
        let guards = key_guard(Hash::new([0; 28]));
        let attributes = "top.bootstrap_attributes";
        let empty = witnesses(&[], &[0xa0]);
        assert!(!touches(
            &guarded(&empty, &guards),
            Axis::Witness,
            attributes
        ));
        let held = witnesses(&[], &[0xa1, 0x03, 0x41, 0x00]);
        assert!(touches(&guarded(&held, &guards), Axis::Witness, attributes));
    }

    /// A witness set with one entry under each of the keys 0 to 8.
    fn every_witness_field() -> Result<Vec<u8>, minicbor::encode::Error<std::convert::Infallible>> {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.map(9)?;
        e.u8(0)?
            .array(1)?
            .array(2)?
            .bytes(&[9; 32])?
            .bytes(&[0; 64])?;
        e.u8(1)?.array(1)?.array(2)?.u8(4)?.u8(0)?;
        e.u8(2)?
            .array(1)?
            .array(4)?
            .bytes(&[1; 32])?
            .bytes(&[0; 64])?;
        e.bytes(&[2; 32])?.bytes(&[0xa0])?;
        e.u8(3)?.array(1)?.bytes(&[0])?;
        e.u8(4)?.array(1)?.u8(0)?;
        e.u8(5)?.map(1)?.array(2)?.u8(0)?.u8(0)?;
        e.array(2)?.u8(0)?.array(2)?.u8(0)?.u8(0)?;
        for key in 6..=8 {
            e.u8(key)?.array(1)?.bytes(&[0])?;
        }
        Ok(e.into_writer())
    }

    #[test]
    fn each_witness_set_field_is_the_one_witness_key_lost_when_it_is_cleared() {
        let bytes = every_witness_field().unwrap();
        let full: WitnessSet = minicbor::decode(&bytes).unwrap();
        assert_eq!(witness_keys(&full), WITNESS_KEYS);
        macro_rules! each_field {
            ($($field:ident),* $(,)?) => {{
                let WitnessSet { $($field: _),* } = &full;
                $(
                    let mut cleared = full.clone();
                    cleared.$field = None;
                    let held = witness_keys(&cleared);
                    let lost: Vec<&str> = WITNESS_KEYS.iter().copied().filter(|k| !held.contains(k)).collect();
                    assert_eq!(lost, [stringify!($field)]);
                )*
            }};
        }
        each_field!(
            vkeywitness,
            native_script,
            bootstrap_witness,
            plutus_v1_script,
            plutus_data,
            redeemer,
            plutus_v2_script,
            plutus_v3_script,
            plutus_v4_script,
        );
    }

    /// A witness set with a key witness when `signed`, one redeemer of the tag
    /// `tag` with `units` memory and steps when `tag` is some, and one V3
    /// script when `scripted`.
    fn redeemed(
        signed: bool,
        tag: Option<u8>,
        units: u64,
        scripted: bool,
    ) -> Result<Vec<u8>, minicbor::encode::Error<std::convert::Infallible>> {
        let mut e = minicbor::Encoder::new(Vec::new());
        let entries = [signed, tag.is_some(), scripted];
        e.map(entries.iter().filter(|held| **held).count() as u64)?;
        if signed {
            e.u8(0)?
                .array(1)?
                .array(2)?
                .bytes(&[9; 32])?
                .bytes(&[0; 64])?;
        }
        if let Some(tag) = tag {
            e.u8(5)?.map(1)?.array(2)?.u8(tag)?.u8(0)?;
            e.array(2)?.u8(0)?.array(2)?.u64(units)?.u64(units)?;
        }
        if scripted {
            e.u8(7)?.array(1)?.bytes(&[0])?;
        }
        Ok(e.into_writer())
    }

    /// The request shapes the content of one top level body shows.
    fn shown(witnesses: &WitnessSet, guards: Option<&Guards>) -> Vec<RequestShape> {
        let levels = [Level {
            depth: Depth::Top,
            keys: Vec::new(),
            guards,
            certificates: Vec::new(),
            actions: Vec::new(),
            witnesses,
        }];
        let shapes = RequestShape::ALL.into_iter();
        shapes.filter(|s| shows(*s, &levels)).collect()
    }

    const SPEND: Option<u8> = Some(0);
    const GUARDING: Option<u8> = Some(6);

    #[test]
    fn no_content_shows_a_shape_a_judge_derives_by_its_request() {
        for bytes in [
            redeemed(false, None, 0, false),
            redeemed(true, SPEND, 0, false),
        ] {
            let bytes = bytes.unwrap();
            let set: WitnessSet = minicbor::decode(&bytes).unwrap();
            let shapes = shown(&set, None);
            assert!(!shapes.contains(&RequestShape::Draft), "{shapes:?}");
            assert!(
                !shapes.contains(&RequestShape::RewardOutputDraft),
                "{shapes:?}"
            );
        }
    }

    #[test]
    fn a_body_with_a_key_witness_shows_signed_and_one_with_none_shows_unsigned() {
        let signed = redeemed(true, None, 0, false).unwrap();
        let signed: WitnessSet = minicbor::decode(&signed).unwrap();
        assert_eq!(shown(&signed, None), [RequestShape::Signed]);
        let unsigned = redeemed(false, None, 0, false).unwrap();
        let unsigned: WitnessSet = minicbor::decode(&unsigned).unwrap();
        assert_eq!(shown(&unsigned, None), [RequestShape::Unsigned]);
    }

    #[test]
    fn only_a_redeemer_of_zero_memory_and_zero_steps_shows_zero_budget() {
        let budgeted = redeemed(true, SPEND, 1, true).unwrap();
        let budgeted: WitnessSet = minicbor::decode(&budgeted).unwrap();
        assert!(!shown(&budgeted, None).contains(&RequestShape::ZeroBudget));
        let zero = redeemed(true, SPEND, 0, true).unwrap();
        let zero: WitnessSet = minicbor::decode(&zero).unwrap();
        assert!(shown(&zero, None).contains(&RequestShape::ZeroBudget));
    }

    #[test]
    fn only_a_script_guard_with_no_guarding_redeemer_shows_guard_without_redeemer() {
        let script = StakeCredential::ScriptHash(Hash::new([1; 28]));
        let scripted = Guards::Credentials(NonEmptySet::from_vec(vec![script]).unwrap());
        let keyed = key_guard(Hash::new([2; 28]));
        let shape = RequestShape::GuardWithoutRedeemer;
        let guarding = redeemed(true, GUARDING, 1, true).unwrap();
        let guarding: WitnessSet = minicbor::decode(&guarding).unwrap();
        assert!(!shown(&guarding, Some(&scripted)).contains(&shape));
        let spending = redeemed(true, SPEND, 1, true).unwrap();
        let spending: WitnessSet = minicbor::decode(&spending).unwrap();
        assert!(!shown(&spending, Some(&keyed)).contains(&shape));
        assert!(shown(&spending, Some(&scripted)).contains(&shape));
    }

    #[test]
    fn only_redeemers_with_no_witnessed_plutus_script_show_reference_script_only() {
        let shape = RequestShape::ReferenceScriptOnly;
        let witnessed = redeemed(true, SPEND, 1, true).unwrap();
        let witnessed: WitnessSet = minicbor::decode(&witnessed).unwrap();
        assert!(!shown(&witnessed, None).contains(&shape));
        let bare = redeemed(true, None, 0, false).unwrap();
        let bare: WitnessSet = minicbor::decode(&bare).unwrap();
        assert!(!shown(&bare, None).contains(&shape));
        let referenced = redeemed(true, SPEND, 1, false).unwrap();
        let referenced: WitnessSet = minicbor::decode(&referenced).unwrap();
        assert!(shown(&referenced, None).contains(&shape));
    }

    #[test]
    fn each_member_is_listed_once_and_only_the_sub_plutus_v1_to_v3_witnesses_are_excluded_here() {
        let rules = rules::committed().unwrap_or_else(|e| panic!("{e}"));
        let members = members(&rules);
        let mut named = BTreeMap::new();
        for member in &members {
            *named.entry(member.cell.to_string()).or_insert(0) += 1;
        }
        let twice: Vec<_> = named.iter().filter(|(_, n)| **n > 1).collect();
        assert!(twice.is_empty(), "{twice:?}");
        let tx_axes = [Axis::Body, Axis::Witness, Axis::Guard, Axis::Certificate];
        let excluded: Vec<String> = members
            .iter()
            .filter(|m| tx_axes.contains(&m.cell.axis) && m.reach != Reach::Counted)
            .map(|m| m.cell.to_string())
            .collect();
        assert_eq!(
            excluded,
            [
                "witness:sub.plutus_v1_script",
                "witness:sub.plutus_v2_script",
                "witness:sub.plutus_v3_script",
            ]
        );
        for language in Language::ALL {
            let prefix = format!("{language}.");
            let builtins: Vec<&Member> = members
                .iter()
                .filter(|m| m.cell.axis == Axis::Builtin && m.cell.member.starts_with(&prefix))
                .collect();
            let counted = builtins.iter().filter(|m| m.reach == Reach::Counted);
            assert_eq!((builtins.len(), counted.count()), (101, 101));
        }
    }

    #[test]
    fn mutants_reach_no_block_serving_route_and_no_route_a_recorded_record_cannot_reach() {
        use ledger_oracle_judge::judgement::{Instrument, Version};
        let v1 = |route| EntryPoint::Route(route, Version::V1);
        assert_eq!(
            entry_reach(v1(Route::FetchBlock), Records::Recorded),
            Reach::Counted
        );
        assert_eq!(
            entry_reach(v1(Route::ReadTx), Records::Mutants),
            Reach::Counted
        );
        let apply = Instrument::ApplyBlock.into();
        assert_eq!(entry_reach(apply, Records::Mutants), Reach::Counted);
        let blocks = [
            Route::FetchBlock,
            Route::FollowTip,
            Route::DumpHistory,
            Route::WatchTx,
        ];
        let mut excluded = [0, 0];
        for entry in EntryPoint::every() {
            let recorded = entry_reach(entry, Records::Recorded);
            let mutants = entry_reach(entry, Records::Mutants);
            let block = matches!(entry, EntryPoint::Route(route, _) if blocks.contains(&route));
            match (&recorded, block) {
                (Reach::Excluded { .. }, _) => assert_eq!(mutants, recorded, "{entry}"),
                (Reach::Counted, true) => {
                    assert!(matches!(mutants, Reach::Excluded { .. }), "{entry}")
                }
                (Reach::Counted, false) => assert_eq!(mutants, Reach::Counted, "{entry}"),
            }
            for (n, reach) in excluded.iter_mut().zip([&recorded, &mutants]) {
                *n += usize::from(*reach != Reach::Counted);
            }
        }
        assert_eq!(excluded, [17, 29]);
    }

    /// The cells of each kept record of a corpus that pallas decodes.
    fn held(kind: CorpusKind) -> Vec<BTreeSet<CellKey>> {
        let corpus = super::super::load(kind);
        let records = corpus.kept().flat_map(|run| run.records());
        records.filter_map(|r| cells_of_tx(&r.tx.0).ok()).collect()
    }

    #[test]
    fn the_supplement_holds_a_guard_met_only_by_bootstrap_at_both_depths() {
        let held = held(CorpusKind::Supp);
        for depth in Depth::ALL {
            let cell = CellKey::new(
                Axis::Guard,
                format!("{}.{KEY_ONLY_BOOTSTRAP}", depth.name()),
            );
            assert!(held.iter().any(|h| h.contains(&cell)), "{depth:?}");
        }
    }
}
