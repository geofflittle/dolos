#[allow(dead_code)]
#[path = "support/musashi_submission.rs"]
mod support;

use dolos_cardano::{
    model::{AccountState, EpochState, EpochValue, FixedNamespace, Stake},
    SingletonEntity,
};
use dolos_core::*;
use dolos_testing::toy_domain::ToyDomain;
use pallas::{
    codec::{minicbor, utils::MaybeIndefArray},
    crypto::{hash::Hasher, key::ed25519::SecretKey},
    ledger::{
        addresses::{Network, ShelleyAddress, ShelleyDelegationPart, ShelleyPaymentPart},
        primitives::dijkstra as n,
        traverse::{Era, MultiEraTx},
    },
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

// ToyDomain's mempool ignores lifecycle calls. Use the production Redb store
// with an in-memory database, keeping chain/state/genesis fixture setup unchanged.
#[derive(Clone)]
struct WithdrawalDomain {
    inner: ToyDomain,
    mempool: dolos_redb3::mempool::RedbMempool,
}
impl WithdrawalDomain {
    fn new(inner: ToyDomain) -> Self {
        Self {
            inner,
            mempool: dolos_redb3::mempool::RedbMempool::in_memory().unwrap(),
        }
    }
}
impl Domain for WithdrawalDomain {
    type Entity = <ToyDomain as Domain>::Entity;
    type EntityDelta = <ToyDomain as Domain>::EntityDelta;
    type Chain = <ToyDomain as Domain>::Chain;
    type WorkUnit = <ToyDomain as Domain>::WorkUnit;
    type Wal = <ToyDomain as Domain>::Wal;
    type State = <ToyDomain as Domain>::State;
    type Archive = <ToyDomain as Domain>::Archive;
    type TipSubscription = <ToyDomain as Domain>::TipSubscription;
    type Mempool = dolos_redb3::mempool::RedbMempool;
    fn storage_config(&self) -> &config::StorageConfig {
        self.inner.storage_config()
    }
    fn sync_config(&self) -> &config::SyncConfig {
        self.inner.sync_config()
    }
    fn genesis(&self) -> Arc<Genesis> {
        self.inner.genesis()
    }
    fn read_chain(&self) -> std::sync::RwLockReadGuard<'_, Self::Chain> {
        self.inner.read_chain()
    }
    fn write_chain(&self) -> std::sync::RwLockWriteGuard<'_, Self::Chain> {
        self.inner.write_chain()
    }
    fn wal(&self) -> &Self::Wal {
        self.inner.wal()
    }
    fn state(&self) -> &Self::State {
        self.inner.state()
    }
    fn archive(&self) -> &Self::Archive {
        self.inner.archive()
    }
    fn mempool(&self) -> &Self::Mempool {
        &self.mempool
    }
    fn watch_tip(&self, from: Option<ChainPoint>) -> Result<Self::TipSubscription, DomainError> {
        self.inner.watch_tip(from)
    }
    fn notify_tip(&self, tip: TipEvent) {
        self.inner.notify_tip(tip);
    }
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test_data/musashi-withdrawals")
        .join(name)
}
fn bytes(name: &str) -> Vec<u8> {
    hex::decode(std::fs::read_to_string(fixture_path(name)).unwrap().trim()).unwrap()
}
fn put_account(domain: &WithdrawalDomain, account: &AccountState) {
    let key = minicbor::to_vec(&account.credential).unwrap().into();
    let w = domain.state().start_writer().unwrap();
    w.write_entity_typed(&key, account).unwrap();
    w.commit().unwrap();
}
fn get_account(domain: &WithdrawalDomain, credential: &n::StakeCredential) -> Option<AccountState> {
    domain
        .state()
        .read_entity_typed(
            AccountState::NS,
            &minicbor::to_vec(credential).unwrap().into(),
        )
        .unwrap()
}
fn registered(credential: n::StakeCredential, balance: u64) -> AccountState {
    let mut a = AccountState::new(64, credential);
    a.registered_at = Some(1400000);
    a.deposit = Some(2_000_000);
    // Spendable rewards must exclude delegated UTxO stake and prior withdrawals.
    a.stake = EpochValue::with_live(
        64,
        Stake {
            rewards_sum: balance + 17,
            withdrawals_sum: 17,
            utxo_sum: 9_000_000_000,
            utxo_sum_at_pointer_addresses: 1_000_000,
        },
    );
    a
}
fn captured() -> (WithdrawalDomain, Vec<u8>, Vec<n::StakeCredential>) {
    let (inner, _) = support::fixture(false);
    let domain = WithdrawalDomain::new(inner);
    let raw = bytes("mint-batch.tx.hex");
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &raw).unwrap();
    assert_eq!(
        tx.hash().to_string(),
        "3a44fbaaf49822fa51d1b4f02818298a183e02d73ec1edb8915b48fd52a89a62"
    );
    let manifest: BTreeMap<String, Vec<String>> = serde_json::from_str(
        &std::fs::read_to_string(fixture_path("mint-batch-inputs.json")).unwrap(),
    )
    .unwrap();
    let mut delta = UtxoSetDelta::default();
    for name in manifest.keys() {
        let parts: Vec<_> = name.split('.').collect();
        delta.produced_utxo.insert(
            TxoRef(parts[0].parse().unwrap(), parts[1].parse().unwrap()),
            Arc::new(EraCbor(8, bytes(&format!("batch-inputs/{name}")))),
        );
    }
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture_path("account-state.json")).unwrap())
            .unwrap();
    let slot = state["validation_slot"].as_u64().unwrap();
    let era = dolos_cardano::eras::load_era_summary::<WithdrawalDomain>(domain.state()).unwrap();
    let epoch_number = era.slot_epoch(slot).0;
    assert_eq!(epoch_number, 65);
    let mut epoch = dolos_cardano::load_epoch::<WithdrawalDomain>(domain.state()).unwrap();
    epoch.number = epoch_number;
    // Replay with the unchanged historical epoch-64 parameter vector; this is
    // fixture assembly, not a claim to have replayed the epoch transition.
    epoch.pparams = EpochValue::with_live(epoch_number, epoch.pparams.unwrap_live().clone());
    let mut credentials = vec![];
    for (hash, account) in state["accounts"].as_object().unwrap() {
        assert_eq!(account["registered"], true);
        let credential = n::StakeCredential::ScriptHash(hash.parse().unwrap());
        assert_eq!(account["balance"], 0);
        let mut a = AccountState::new(epoch_number, credential.clone());
        a.registered_at = Some(1401321);
        a.deposit = Some(2_000_000);
        // Historical fixture has no prior reward or withdrawal value.
        put_account(&domain, &a);
        credentials.push(credential);
    }
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.write_entity_typed(&EpochState::singleton_key(), &epoch)
        .unwrap();
    w.set_cursor(ChainPoint::Slot(state["validation_slot"].as_u64().unwrap()))
        .unwrap();
    w.commit().unwrap();
    (domain, raw, credentials)
}

#[test]
fn captured_v3_withdrawals_validate_both_phases_without_changing_state() {
    let (domain, raw, credentials) = captured();
    let before: Vec<_> = credentials
        .iter()
        .map(|c| get_account(&domain, c))
        .collect();
    let validated = domain.validate_tx(&domain.read_chain(), &raw).unwrap();
    let report = validated.report.unwrap();
    assert_eq!(report.len(), 5);
    assert!(report.iter().all(|x| x.success));
    assert!(domain.mempool().peek_pending().is_empty());
    domain
        .receive_tx("offline-capture", &domain.read_chain(), &raw)
        .unwrap();
    assert_eq!(domain.mempool().peek_pending()[0].payload().1, raw);
    assert_eq!(
        credentials
            .iter()
            .map(|c| get_account(&domain, c))
            .collect::<Vec<_>>(),
        before
    );
}

fn assert_rejected(domain: &WithdrawalDomain, raw: &[u8], reason: &str) {
    let before = domain.mempool().peek_pending().len() + domain.mempool().peek_inflight().len();
    let error = domain
        .receive_tx("offline-negative", &domain.read_chain(), raw)
        .unwrap_err();
    assert!(
        format!("{error:?}").contains(reason),
        "expected {reason}: {error:?}"
    );
    assert_eq!(
        domain.mempool().peek_pending().len() + domain.mempool().peek_inflight().len(),
        before
    );
}

#[test]
fn captured_zero_withdrawal_requires_registered_zero_balance() {
    let (domain, raw, credentials) = captured();
    for (registered_at, balance, error) in [
        (None, 0, "unregistered account"),
        (
            Some(1401321),
            1,
            "legacy Plutus withdrawals must drain the account",
        ),
    ] {
        let mut a = AccountState::new(65, credentials[0].clone());
        a.registered_at = registered_at;
        a.deposit = Some(2_000_000);
        a.stake.unwrap_live_mut().rewards_sum = balance;
        put_account(&domain, &a);
        assert_rejected(&domain, &raw, error);
        assert_eq!(get_account(&domain, &credentials[0]), Some(a));
    }
}

// Independent spending inputs ensure account conflicts cannot be masked by UTxO conflicts.
fn synthetic(
    amounts: &[u64],
    balance: u64,
) -> (WithdrawalDomain, Vec<Vec<u8>>, n::StakeCredential) {
    let (inner, control) = support::fixture(false);
    let domain = WithdrawalDomain::new(inner);
    let key = SecretKey::from([73; 32]); // Public deterministic test-only key.
    let key_hash = Hasher::<224>::hash(key.public_key().as_ref());
    let credential = n::StakeCredential::AddrKeyhash(key_hash);
    let address = ShelleyAddress::new(
        Network::Testnet,
        ShelleyPaymentPart::Key(key_hash),
        ShelleyDelegationPart::Null,
    )
    .to_vec();
    let reward =
        dolos_cardano::pallas_extras::stake_credential_to_address(Network::Testnet, &credential)
            .to_vec();
    let output = |coin| {
        n::TransactionOutput::PostAlonzo(
            n::PostAlonzoTransactionOutput {
                address: address.clone().into(),
                value: n::Value::Coin(coin),
                datum_option: None,
                script_ref: None,
            }
            .into(),
        )
    };
    let template = MultiEraTx::decode_for_era(Era::Dijkstra, &control).unwrap();
    let mut delta = UtxoSetDelta::default();
    let transactions = amounts
        .iter()
        .enumerate()
        .map(|(index, amount)| {
            let input = n::TransactionInput {
                transaction_id: [90 + index as u8; 32].into(),
                index: 0,
            };
            delta.produced_utxo.insert(
                TxoRef(input.transaction_id, 0),
                Arc::new(EraCbor(8, minicbor::to_vec(output(20_000_000)).unwrap())),
            );
            let mut tx = template.as_dijkstra().unwrap().clone();
            tx.transaction_body.inputs = n::Set::from(vec![input]);
            tx.transaction_body.fee = 1_000_000;
            tx.transaction_body.outputs = MaybeIndefArray::Def(vec![output(19_000_000 + amount)]);
            tx.transaction_body.withdrawals =
                Some(BTreeMap::from([(reward.clone().into(), *amount)]));
            let hash = Hasher::<256>::hash(&minicbor::to_vec(&tx.transaction_body).unwrap());
            tx.transaction_witness_set.vkeywitness =
                n::NonEmptySet::from_vec(vec![n::VKeyWitness {
                    vkey: key.public_key().as_ref().to_vec().into(),
                    signature: key.sign(hash).as_ref().to_vec().into(),
                }]);
            minicbor::to_vec(tx.to_mempool_transaction()).unwrap()
        })
        .collect();
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.commit().unwrap();
    put_account(&domain, &registered(credential.clone(), balance));
    (domain, transactions, credential)
}

#[test]
fn key_withdrawals_allow_zero_partial_and_full_but_not_stake_or_overdraft() {
    let (domain, txs, credential) = synthetic(&[0, 4, 10, 11, 1_000_000], 10);
    let before = get_account(&domain, &credential);
    for tx in &txs[..3] {
        domain.validate_tx(&domain.read_chain(), tx).unwrap();
    }
    for tx in &txs[3..] {
        assert_rejected(&domain, tx, "amount exceeds the original balance");
    }
    assert_eq!(get_account(&domain, &credential), before);
}

#[test]
fn unavailable_or_inconsistent_account_state_rejects_without_defaults() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let mut account = registered(credential.clone(), 0);
    for (case, reason) in [
        "registered account has no live balance",
        "recorded withdrawals exceed rewards",
        "account snapshot does not match current epoch",
        "stored credential does not match account key",
    ]
    .into_iter()
    .enumerate()
    {
        account.stake = EpochValue::with_live(64, Stake::default());
        match case {
            0 => account.stake = EpochValue::new(64),
            1 => account.stake.unwrap_live_mut().withdrawals_sum = 1,
            2 => account.stake = EpochValue::with_live(63, Stake::default()),
            _ => account.credential = n::StakeCredential::AddrKeyhash([99; 28].into()),
        }
        // Also exercise corruption: entity contents need to match its lookup key.
        let w = domain.state().start_writer().unwrap();
        w.write_entity_typed(&minicbor::to_vec(&credential).unwrap().into(), &account)
            .unwrap();
        w.commit().unwrap();
        assert_rejected(&domain, &txs[0], reason);
    }
}

#[test]
fn pending_and_inflight_withdrawals_reserve_only_available_rewards() {
    for stage in 0..3 {
        let (domain, txs, credential) = synthetic(&[4, 6, 7, 1], 10);
        let before = get_account(&domain, &credential);
        for tx in &txs {
            domain.validate_tx(&domain.read_chain(), tx).unwrap();
        }
        let hash = domain
            .receive_tx("first", &domain.read_chain(), &txs[0])
            .unwrap();
        if stage >= 1 {
            domain.mempool().mark_inflight(&[hash]).unwrap();
        }
        if stage >= 2 {
            domain.mempool().mark_acknowledged(&[hash]).unwrap();
        }
        assert_rejected(&domain, &txs[2], "amount exceeds the original balance");
        domain
            .receive_tx("remaining", &domain.read_chain(), &txs[1])
            .unwrap();
        assert_rejected(&domain, &txs[3], "amount exceeds the original balance");
        assert_eq!(get_account(&domain, &credential), before);
    }
}

#[test]
fn the_chain_keeps_the_effects_of_the_unconfirmed_transactions_each_submit_read() {
    let (domain, txs, _) = synthetic(&[1, 1, 1, 100], 10);
    let mut received = vec![];
    for tx in &txs[..3] {
        let hash = domain.receive_tx("each", &domain.read_chain(), tx).unwrap();
        assert_eq!(domain.read_chain().unconfirmed_txs_held(), received);
        received.push(hash);
    }
    assert_rejected(&domain, &txs[3], "amount exceeds the original balance");
    assert_eq!(domain.read_chain().unconfirmed_txs_held(), received);
}

#[test]
fn confirmation_rollback_and_drop_update_withdrawal_reservations() {
    let (domain, txs, credential) = synthetic(&[4, 6, 7], 10);
    let hash = domain
        .receive_tx("first", &domain.read_chain(), &txs[0])
        .unwrap();
    domain.mempool().mark_inflight(&[hash]).unwrap();
    // Model the chain's committed balance update, then its mempool notification.
    let mut account = get_account(&domain, &credential);
    let original = account.clone();
    let mut withdrawal = dolos_cardano::model::WithdrawalInc::new(credential.clone(), 4);
    withdrawal.apply(&mut account);
    put_account(&domain, account.as_ref().unwrap());
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400010), &[hash], &[], 10, 10)
        .unwrap();
    assert_eq!(
        domain.mempool().check_status(&hash).stage,
        MempoolTxStage::Confirmed
    );
    domain.validate_tx(&domain.read_chain(), &txs[1]).unwrap(); // no double subtraction
    assert_rejected(&domain, &txs[2], "amount exceeds the original balance");
    withdrawal.undo(&mut account);
    assert_eq!(account, original);
    put_account(&domain, account.as_ref().unwrap());
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400007), &[], &[hash], 10, 10)
        .unwrap();
    assert_eq!(
        domain.mempool().check_status(&hash).stage,
        MempoolTxStage::Pending
    );
    assert_rejected(&domain, &txs[2], "amount exceeds the original balance");
    domain.mempool().mark_inflight(&[hash]).unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400011), &[], &[], 10, 1)
        .unwrap();
    assert!(domain.mempool().peek_inflight().is_empty());
    let finalized = domain.mempool().dump_finalized(0, 10);
    assert_eq!(finalized.items.len(), 1);
    assert_eq!(finalized.items[0].hash(), hash);
    assert_eq!(finalized.items[0].stage, MempoolTxStage::Dropped);
    domain.validate_tx(&domain.read_chain(), &txs[2]).unwrap();
}

// Unregisters the synthetic credential and returns a transaction that registers it.
fn registration(
    domain: &WithdrawalDomain,
    txs: &[Vec<u8>],
    credential: &n::StakeCredential,
) -> Vec<u8> {
    let key = minicbor::to_vec(credential).unwrap().into();
    let w = domain.state().start_writer().unwrap();
    w.delete_entity(AccountState::NS, &key).unwrap();
    w.commit().unwrap();
    let deposit = dolos_cardano::load_effective_pparams::<WithdrawalDomain>(domain.state())
        .unwrap()
        .key_deposit()
        .unwrap();
    let template = MultiEraTx::decode_for_era(Era::Dijkstra, &txs[0]).unwrap();
    let mut tx = template.as_dijkstra().unwrap().clone();
    let n::TransactionOutput::PostAlonzo(output) = &tx.transaction_body.outputs[0] else {
        unreachable!()
    };
    let address = output.address.clone();
    let output = |coin| {
        n::TransactionOutput::PostAlonzo(
            n::PostAlonzoTransactionOutput {
                address: address.clone(),
                value: n::Value::Coin(coin),
                datum_option: None,
                script_ref: None,
            }
            .into(),
        )
    };
    let input = n::TransactionInput {
        transaction_id: [120; 32].into(),
        index: 0,
    };
    let mut delta = UtxoSetDelta::default();
    delta.produced_utxo.insert(
        TxoRef(input.transaction_id, 0),
        Arc::new(EraCbor(8, minicbor::to_vec(output(20_000_000)).unwrap())),
    );
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.commit().unwrap();
    tx.transaction_body.outputs =
        MaybeIndefArray::Def(vec![output(20_000_000 - 1_000_000 - deposit)]);
    tx.transaction_body.inputs = n::Set::from(vec![input]);
    tx.transaction_body.withdrawals = None;
    tx.transaction_body.fee = 1_000_000;
    tx.transaction_body.certificates =
        n::NonEmptySet::from_vec(vec![n::Certificate::Reg(credential.clone(), deposit)]);
    let key = SecretKey::from([73; 32]);
    let hash = Hasher::<256>::hash(&minicbor::to_vec(&tx.transaction_body).unwrap());
    tx.transaction_witness_set.vkeywitness = n::NonEmptySet::from_vec(vec![n::VKeyWitness {
        vkey: key.public_key().as_ref().to_vec().into(),
        signature: key.sign(hash).as_ref().to_vec().into(),
    }]);
    minicbor::to_vec(tx.to_mempool_transaction()).unwrap()
}

#[test]
fn revalidation_keeps_pending_transactions_the_ledger_still_accepts() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let register = registration(&domain, &txs, &credential);
    assert_rejected(&domain, &txs[0], "account missing from the original state");
    let a = domain
        .receive_tx("register", &domain.read_chain(), &register)
        .unwrap();
    let t = domain
        .receive_tx("withdraw", &domain.read_chain(), &txs[0])
        .unwrap();
    assert_eq!(
        domain.revalidate_pending(&domain.read_chain()).unwrap(),
        Vec::<TxHash>::new()
    );
    let pending: Vec<_> = domain
        .mempool()
        .peek_pending()
        .iter()
        .map(|x| x.hash())
        .collect();
    assert_eq!(pending, vec![a, t]);
    domain.mempool().mark_inflight(&[a]).unwrap();
    assert_eq!(
        domain.revalidate_pending(&domain.read_chain()).unwrap(),
        Vec::<TxHash>::new()
    );
    assert_eq!(
        domain.mempool().check_status(&t).stage,
        MempoolTxStage::Pending
    );
}

#[test]
fn a_pending_withdrawal_is_dropped_once_its_registration_is_dropped() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let register = registration(&domain, &txs, &credential);
    let a = domain
        .receive_tx("register", &domain.read_chain(), &register)
        .unwrap();
    domain.mempool().mark_inflight(&[a]).unwrap();
    let t = domain
        .receive_tx("withdraw", &domain.read_chain(), &txs[0])
        .unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400011), &[], &[], 10, 1)
        .unwrap();
    assert_eq!(
        domain.revalidate_pending(&domain.read_chain()).unwrap(),
        vec![t]
    );
    assert!(domain.mempool().peek_pending().is_empty());
    let finalized = domain.mempool().dump_finalized(0, 10);
    let stages: Vec<_> = finalized
        .items
        .iter()
        .map(|x| (x.hash(), x.stage.clone()))
        .collect();
    assert_eq!(
        stages,
        vec![(a, MempoolTxStage::Dropped), (t, MempoolTxStage::Dropped)]
    );
}

#[test]
fn a_rolled_back_registration_stays_ahead_of_its_pending_withdrawal() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let register = registration(&domain, &txs, &credential);
    let a = domain
        .receive_tx("register", &domain.read_chain(), &register)
        .unwrap();
    domain.mempool().mark_inflight(&[a]).unwrap();
    let t = domain
        .receive_tx("withdraw", &domain.read_chain(), &txs[0])
        .unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400011), &[a], &[], 10, 10)
        .unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400007), &[], &[a], 10, 10)
        .unwrap();
    let pending: Vec<_> = domain
        .mempool()
        .peek_pending()
        .iter()
        .map(|x| x.hash())
        .collect();
    assert_eq!(pending, vec![a, t]);
    assert_eq!(
        domain.revalidate_pending(&domain.read_chain()).unwrap(),
        Vec::<TxHash>::new()
    );
}

#[test]
fn a_rolled_back_registration_whose_input_is_spent_drops_its_withdrawal() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let register = registration(&domain, &txs, &credential);
    let a = domain
        .receive_tx("register", &domain.read_chain(), &register)
        .unwrap();
    domain.mempool().mark_inflight(&[a]).unwrap();
    let t = domain
        .receive_tx("withdraw", &domain.read_chain(), &txs[0])
        .unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400011), &[a], &[], 10, 10)
        .unwrap();
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400007), &[], &[a], 10, 10)
        .unwrap();
    // Model a block on the new branch spending the registration's input.
    let mut delta = UtxoSetDelta::default();
    delta
        .consumed_utxo
        .insert(TxoRef([120; 32].into(), 0), Arc::new(EraCbor(8, vec![])));
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.commit().unwrap();
    let dropped = domain.revalidate_pending(&domain.read_chain()).unwrap();
    assert!(dropped.contains(&a) && dropped.contains(&t), "{dropped:?}");
    assert!(domain.mempool().peek_pending().is_empty());
}

#[test]
fn a_pending_registration_a_block_includes_is_confirmed() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let register = registration(&domain, &txs, &credential);
    let a = domain
        .receive_tx("register", &domain.read_chain(), &register)
        .unwrap();
    // Model the block's committed state: the input spent and the account registered.
    let mut delta = UtxoSetDelta::default();
    delta
        .consumed_utxo
        .insert(TxoRef([120; 32].into(), 0), Arc::new(EraCbor(8, vec![])));
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.commit().unwrap();
    put_account(&domain, &registered(credential.clone(), 0));
    domain
        .mempool()
        .confirm(&ChainPoint::Slot(1400011), &[a], &[], 10, 10)
        .unwrap();
    assert_eq!(
        domain.revalidate_pending(&domain.read_chain()).unwrap(),
        Vec::<TxHash>::new()
    );
    assert_eq!(
        domain.mempool().check_status(&a).stage,
        MempoolTxStage::Confirmed
    );
    assert!(domain.mempool().dump_finalized(0, 10).items.is_empty());
}

#[test]
fn absent_deregistered_and_unreadable_accounts_do_not_become_zero_balances() {
    let (domain, txs, credential) = synthetic(&[0], 0);
    let key = minicbor::to_vec(&credential).unwrap().into();
    let w = domain.state().start_writer().unwrap();
    w.delete_entity(AccountState::NS, &key).unwrap();
    w.commit().unwrap();
    assert_rejected(&domain, &txs[0], "account missing from the original state");
    let mut a = registered(credential, 0);
    a.deregistered_at = Some(1400001);
    put_account(&domain, &a);
    assert_rejected(&domain, &txs[0], "account missing from the original state");
    let w = domain.state().start_writer().unwrap();
    w.write_entity(AccountState::NS, &key, &vec![0xff]).unwrap();
    w.commit().unwrap();
    assert_rejected(&domain, &txs[0], "entity decoding error");
}

#[test]
fn rejected_signature_does_not_reserve_rewards_and_zero_withdrawals_can_repeat() {
    let (domain, txs, credential) = synthetic(&[10, 10], 10);
    let before = get_account(&domain, &credential);
    let mut bad = txs[0].clone();
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &bad).unwrap();
    let signature = tx
        .as_dijkstra()
        .unwrap()
        .transaction_witness_set
        .vkeywitness
        .as_ref()
        .unwrap()[0]
        .signature
        .to_vec();
    let offset = bad
        .windows(signature.len())
        .position(|x| x == signature)
        .unwrap();
    bad[offset] ^= 1;
    assert_rejected(&domain, &bad, "VKWrongSignature");
    domain
        .receive_tx("valid-after-rejection", &domain.read_chain(), &txs[1])
        .unwrap();
    assert_eq!(get_account(&domain, &credential), before);
    let (domain, txs, _) = synthetic(&[0, 0], 0);
    for tx in txs {
        domain
            .receive_tx("zero", &domain.read_chain(), &tx)
            .unwrap();
    }
    assert_eq!(domain.mempool().peek_pending().len(), 2);
}

#[test]
fn simultaneous_submissions_cannot_overdraw_one_account() {
    let (domain, txs, credential) = synthetic(&[7, 7], 10);
    let before = get_account(&domain, &credential);
    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = txs
            .iter()
            .map(|tx| {
                let domain = &domain;
                scope.spawn(move || domain.receive_tx("concurrent", &domain.read_chain(), tx))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|x| x.is_ok()).count(), 1);
    let error = results.into_iter().find_map(Result::err).unwrap();
    assert!(
        format!("{error:?}").contains("amount exceeds the original balance"),
        "{error:?}"
    );
    assert_eq!(domain.mempool().peek_pending().len(), 1);
    assert_eq!(get_account(&domain, &credential), before);
}

#[test]
fn failed_v3_execution_leaves_accounts_and_mempool_unchanged() {
    let (domain, raw, credentials) = captured();
    let before: Vec<_> = credentials
        .iter()
        .map(|c| get_account(&domain, c))
        .collect();
    let tx = MultiEraTx::decode_for_era(Era::Dijkstra, &raw).unwrap();
    let mut delta = UtxoSetDelta::default();
    // Synthetic state mutation: replace the settings reference datum with an
    // invalid shape. Keep the transaction, scripts and all commitments intact.
    for input in tx.reference_inputs() {
        let key = TxoRef::from(&input);
        let encoded = domain
            .state()
            .get_utxos(vec![key.clone()])
            .unwrap()
            .remove(&key)
            .unwrap();
        let mut output: n::TransactionOutput = minicbor::decode(&encoded.1).unwrap();
        if let n::TransactionOutput::PostAlonzo(inner) = &mut output {
            if inner.datum_option.is_some() {
                inner.datum_option = Some(
                    n::DatumOption::Data(pallas::codec::utils::CborWrap(
                        n::PlutusData::BigInt(n::BigInt::Int(0.into())).into(),
                    ))
                    .into(),
                );
                delta.produced_utxo.insert(
                    key,
                    Arc::new(EraCbor(8, minicbor::to_vec(&output).unwrap())),
                );
            }
        }
    }
    assert!(!delta.produced_utxo.is_empty());
    let w = domain.state().start_writer().unwrap();
    w.apply_utxoset(&delta).unwrap();
    w.commit().unwrap();
    assert_rejected(&domain, &raw, "Phase2ValidationRejected");
    assert_eq!(
        credentials
            .iter()
            .map(|c| get_account(&domain, c))
            .collect::<Vec<_>>(),
        before
    );
}
