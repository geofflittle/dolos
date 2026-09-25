use std::{collections::HashMap, sync::Arc};

use dolos_core::{
    ChainError, Domain, EntityKey, Genesis, InvariantViolation, StateError, StateStore as _,
    TxOrder, TxoRef,
};
use pallas::{
    codec::utils::KeepRaw,
    ledger::{
        primitives::{Epoch, PlutusData},
        traverse::{
            MultiEraBlock, MultiEraCert, MultiEraInput, MultiEraOutput, MultiEraPolicyAssets,
            MultiEraProposal, MultiEraRedeemer, MultiEraTx, MultiEraUpdate,
        },
    },
};
use tracing::{debug, instrument, warn};

use crate::{
    load_effective_pparams, load_gov, owned::OwnedMultiEraOutput, roll::proposals::ProposalVisitor,
    utxoset, Cache, DRepState, FixedNamespace as _, PParamsSet,
};

// Sub-modules
pub mod accounts;
pub mod assets;
pub mod batch;
pub mod datums;
pub mod dreps;
pub mod epochs;
pub mod pools;
pub mod proposals;
pub mod txs;
pub mod work_unit;

// Re-exports
pub use batch::{WorkBatch, WorkBlock, WorkDeltas};
pub use work_unit::RollWorkUnit;

use accounts::AccountVisitor;
use assets::AssetStateVisitor;
use datums::DatumVisitor;
use dreps::{DRepStateVisitor, DormancyContext};
use epochs::EpochStateVisitor;
use pools::PoolStateVisitor;
use txs::TxLogVisitor;

pub trait BlockVisitor {
    #[allow(unused_variables)]
    #[allow(clippy::too_many_arguments)]
    fn visit_root(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        genesis: &Genesis,
        pparams: &PParamsSet,
        epoch: Epoch,
        epoch_start: u64,
        protocol: u16,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a transaction. IMPORTANT: the crawl calls this for *every*
    /// transaction in the block, phase-2-invalid ones included, so that fees
    /// and collateral can still be priced. An implementation that consumes
    /// transaction-body content (certificates, mints, withdrawals, proposals,
    /// votes) owes its own `tx.is_valid()` check.
    #[allow(unused_variables)]
    fn visit_tx(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        utxos: &HashMap<TxoRef, OwnedMultiEraOutput>,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a consumed input. IMPORTANT: for a phase-2-invalid transaction
    /// pallas resolves `consumes()` to the collateral inputs, which is exactly
    /// what the ledger spends — this must not be gated on validity.
    #[allow(unused_variables)]
    fn visit_input(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        input: &MultiEraInput,
        resolved: &MultiEraOutput,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a produced output. IMPORTANT: for a phase-2-invalid transaction
    /// pallas resolves `produces()` to the collateral-return output, which is
    /// exactly what the ledger creates — this must not be gated on validity.
    #[allow(unused_variables)]
    fn visit_output(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        index: u32,
        output: &MultiEraOutput,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a mint. The crawl calls this only for valid transactions: the
    /// Conway LEDGER rule applies no body effects for a phase-2 failure.
    #[allow(unused_variables)]
    fn visit_mint(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        mint: &MultiEraPolicyAssets,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a certificate. The crawl calls this only for valid transactions:
    /// CERTS runs only under `IsValid True`.
    #[allow(unused_variables)]
    fn visit_cert(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        order: &TxOrder,
        cert: &MultiEraCert,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a withdrawal. The crawl calls this only for valid transactions:
    /// the withdrawal drain runs only under `IsValid True`.
    #[allow(unused_variables)]
    fn visit_withdrawal(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        account: &[u8],
        amount: u64,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a protocol-parameter update. The crawl calls the transaction-
    /// carried form (`tx: Some(_)`) only for valid transactions — UTXOS's
    /// invalid branch returns the PPUP state untouched. The block-level form
    /// (`tx: None`) is the Byron/Shelley header update, which has no
    /// transaction and no validity.
    #[allow(unused_variables)]
    fn visit_update(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: Option<&MultiEraTx>,
        update: &MultiEraUpdate,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit plutus data available in the tx witness set. IMPORTANT: this does
    /// not include inline-plutus data (visit the outputs for that).
    #[allow(unused_variables)]
    fn visit_datums(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        data: &KeepRaw<'_, PlutusData>,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    /// Visit a governance proposal. The crawl calls this only for valid
    /// transactions: GOV runs only under `IsValid True`.
    #[allow(unused_variables)]
    fn visit_proposal(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        proposal: &MultiEraProposal,
        idx: usize,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn visit_redeemers(
        &mut self,
        deltas: &mut WorkDeltas,
        block: &MultiEraBlock,
        tx: &MultiEraTx,
        proposal: &MultiEraRedeemer,
    ) -> Result<(), ChainError> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn flush(&mut self, deltas: &mut WorkDeltas) -> Result<(), ChainError> {
        Ok(())
    }
}

pub struct DeltaBuilder<'a> {
    genesis: Arc<Genesis>,
    work: &'a mut WorkBlock,
    active_params: &'a PParamsSet,
    epoch: Epoch,
    epoch_start: u64,
    protocol: u16,
    utxos: &'a HashMap<TxoRef, OwnedMultiEraOutput>,

    /// Exactly the inputs the lenient utxo walk consumed, when that rule is on.
    ///
    /// `None` is the strict rule, where every input a transaction names is
    /// consumed and one that cannot be resolved is a refusal. `Some` is a
    /// positive list rather than a licence to skip: an input outside it was not
    /// consumed by the ledger either, so showing it to a visitor would report a
    /// spend the network did not make.
    consumed: Option<std::collections::HashSet<TxoRef>>,

    account_state: AccountVisitor,
    asset_state: AssetStateVisitor,
    datum_state: DatumVisitor,
    drep_state: DRepStateVisitor,
    epoch_state: EpochStateVisitor,
    pool_state: PoolStateVisitor,
    tx_logs: TxLogVisitor,
    proposal_logs: ProposalVisitor,
}

impl<'a> DeltaBuilder<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        genesis: Arc<Genesis>,
        protocol: u16,
        active_params: &'a PParamsSet,
        epoch: Epoch,
        epoch_start: u64,
        work: &'a mut WorkBlock,
        utxos: &'a HashMap<TxoRef, OwnedMultiEraOutput>,
        dormancy: DormancyContext,
        consumed: Option<std::collections::HashSet<TxoRef>>,
    ) -> Self {
        Self {
            genesis,
            work,
            active_params,
            epoch,
            epoch_start,
            protocol,
            utxos,
            consumed,
            account_state: Default::default(),
            asset_state: Default::default(),
            datum_state: Default::default(),
            drep_state: DRepStateVisitor::new(dormancy),
            epoch_state: Default::default(),
            pool_state: Default::default(),
            tx_logs: Default::default(),
            proposal_logs: Default::default(),
        }
    }

    /// The dormancy context after this block's deltas — a release inside
    /// the block zeroes the counter; registrations seen while the counter
    /// was non-zero extend the fan-out key set. `compute_delta` threads
    /// the context into the next block's builder.
    pub fn take_dormancy(&mut self) -> DormancyContext {
        self.drep_state.take_dormancy()
    }

    pub fn crawl(&mut self) -> Result<(), ChainError> {
        let block = self.work.decoded();
        let block = block.view();
        let mut deltas = WorkDeltas::default();

        self.account_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.asset_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.datum_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.drep_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.epoch_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.pool_state.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.tx_logs.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;
        self.proposal_logs.visit_root(
            &mut deltas,
            block,
            &self.genesis,
            self.active_params,
            self.epoch,
            self.epoch_start,
            self.protocol,
        )?;

        for (order, tx) in block.txs().iter().enumerate() {
            self.account_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.asset_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.datum_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.drep_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.epoch_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.pool_state
                .visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.tx_logs.visit_tx(&mut deltas, block, tx, self.utxos)?;
            self.proposal_logs
                .visit_tx(&mut deltas, block, tx, self.utxos)?;

            for input in tx.consumes() {
                let txoref = TxoRef::from(&input);

                // Under the lenient rule the utxo walk has already decided
                // which inputs were available at their own transaction's
                // position. An input it did not consume was not consumed by the
                // ledger either, so showing it here would report a spend the
                // network did not make.
                if let Some(consumed) = &self.consumed {
                    if !consumed.contains(&txoref) {
                        continue;
                    }
                }

                let resolved = self.utxos.get(&txoref).ok_or_else(|| {
                    StateError::InvariantViolation(InvariantViolation::InputNotFound(txoref))
                })?;

                resolved.with_dependent(|_, resolved| {
                    self.account_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.asset_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.datum_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.drep_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.epoch_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.pool_state
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.tx_logs
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    self.proposal_logs
                        .visit_input(&mut deltas, block, tx, &input, resolved)?;
                    Result::<_, ChainError>::Ok(())
                })?;
            }

            for (index, output) in tx.produces() {
                self.account_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.asset_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.datum_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.drep_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.epoch_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.pool_state
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.tx_logs
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
                self.proposal_logs
                    .visit_output(&mut deltas, block, tx, index as u32, &output)?;
            }

            // The Conway LEDGER rule runs CERTS, GOV, the withdrawal drain and
            // the PPUP/update registration only under `IsValid True`; a
            // phase-2-invalid tx moves collateral and nothing else. Two
            // carve-outs stay outside this guard: the input/output fan-outs,
            // where pallas already resolves `consumes()`/`produces()` to the
            // collateral pair, and `visit_tx`, which every visitor still sees
            // so fees and collateral can be priced.
            if tx.is_valid() {
                for mint in tx.mints() {
                    self.account_state
                        .visit_mint(&mut deltas, block, tx, &mint)?;
                    self.asset_state.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.datum_state.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.drep_state.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.epoch_state.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.pool_state.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.tx_logs.visit_mint(&mut deltas, block, tx, &mint)?;
                    self.proposal_logs
                        .visit_mint(&mut deltas, block, tx, &mint)?;
                }

                for cert in tx.certs() {
                    self.account_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.asset_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.datum_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.drep_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.epoch_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.pool_state
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.tx_logs
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                    self.proposal_logs
                        .visit_cert(&mut deltas, block, tx, &order, &cert)?;
                }

                for (account, amount) in tx.withdrawals().collect::<Vec<_>>() {
                    self.account_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.asset_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.datum_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.drep_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.epoch_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.pool_state
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.tx_logs
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                    self.proposal_logs
                        .visit_withdrawal(&mut deltas, block, tx, account, amount)?;
                }

                if let Some(update) = tx.update() {
                    self.account_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.asset_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.datum_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.drep_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.epoch_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.pool_state
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.tx_logs
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                    self.proposal_logs
                        .visit_update(&mut deltas, block, Some(tx), &update)?;
                }
            }

            for datum in tx.plutus_data() {
                self.account_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.asset_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.datum_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.drep_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.epoch_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.pool_state
                    .visit_datums(&mut deltas, block, tx, datum)?;
                self.tx_logs.visit_datums(&mut deltas, block, tx, datum)?;
                self.proposal_logs
                    .visit_datums(&mut deltas, block, tx, datum)?;
            }

            // Same LEDGER gate as above: GOV never registers a proposal
            // carried by a phase-2-invalid transaction.
            if tx.is_valid() {
                for (idx, proposal) in tx.gov_proposals().iter().enumerate() {
                    self.account_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.asset_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.datum_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.drep_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.epoch_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.pool_state
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.tx_logs
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                    self.proposal_logs
                        .visit_proposal(&mut deltas, block, tx, proposal, idx)?;
                }
            }

            for redeemer in tx.redeemers() {
                self.account_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.asset_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.datum_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.drep_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.epoch_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.pool_state
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.tx_logs
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
                self.proposal_logs
                    .visit_redeemers(&mut deltas, block, tx, &redeemer)?;
            }
        }

        if let Some(update) = block.update() {
            self.account_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.asset_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.datum_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.drep_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.epoch_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.pool_state
                .visit_update(&mut deltas, block, None, &update)?;
            self.tx_logs
                .visit_update(&mut deltas, block, None, &update)?;
            self.proposal_logs
                .visit_update(&mut deltas, block, None, &update)?;
        }

        self.account_state.flush(&mut deltas)?;
        self.asset_state.flush(&mut deltas)?;
        self.datum_state.flush(&mut deltas)?;
        self.drep_state.flush(&mut deltas)?;
        self.epoch_state.flush(&mut deltas)?;
        self.pool_state.flush(&mut deltas)?;
        self.tx_logs.flush(&mut deltas)?;
        self.proposal_logs.flush(&mut deltas)?;

        self.work.deltas = deltas;

        Ok(())
    }
}

#[instrument(name = "roll", skip_all)]
pub(crate) fn compute_delta<D: Domain>(
    genesis: Arc<Genesis>,
    cache: &Cache,
    state: &D::State,
    batch: &mut WorkBatch,
) -> Result<(), ChainError> {
    let (epoch, _) = cache.eras.slot_epoch(batch.first_slot());

    let (protocol, _) = cache.eras.protocol_and_era_for_epoch(epoch);
    let epoch_start = cache.eras.epoch_start(epoch);

    debug!(
        from = batch.first_slot(),
        to = batch.last_slot(),
        epoch,
        "computing delta"
    );

    let active_params = load_effective_pparams::<D>(state)?;

    // Governance dormancy context for the DRep visitor: the dormant-epoch
    // counter and — only when it's non-zero, which is rare — the dreps key
    // set for the release fan-out. The context evolves across blocks of
    // the batch (a release zeroes the counter, registrations extend the
    // key set), so it's taken back after each crawl.
    let mut dormancy = DormancyContext {
        dormant_epochs: load_gov::<D>(state)?.num_dormant_epochs,
        drep_keys: Default::default(),
        batch_registrations: Default::default(),
    };

    if dormancy.dormant_epochs > 0 {
        let mut keys = Vec::new();

        // raw iteration: only the keys matter, skip the CBOR decode
        for record in state.iter_entities(DRepState::NS, EntityKey::full_range())? {
            let (key, _) = record?;
            keys.push(key);
        }

        dormancy.drep_keys = Arc::new(keys);
    }

    let lenient = batch.lenient_apply;

    for block in batch.blocks.iter_mut() {
        // Under the lenient rule the utxo walk runs first, because it is the
        // one place that decides whether an input was available at its own
        // transaction's position, and the visitors have to be shown exactly the
        // consumptions it made and no others.
        let lenient_delta = if lenient {
            let blockd = block.decoded();
            let blockd = blockd.view();

            let (delta, stats) = utxoset::compute_apply_delta_lenient(
                blockd,
                &batch.utxos_decoded,
                &batch.store_utxos,
            )?;

            // A block applied this way leaves a UTxO set the ledger rules do
            // not give, so the disclosure is a warning and not an entry. Each
            // left input is named against its own transaction, because the
            // block and a count leave nobody able to say which transaction to
            // go and look at.
            if stats.is_lenient() {
                warn!(
                    slot = blockd.slot(),
                    block = %blockd.hash(),
                    txs = blockd.tx_count(),
                    skipped_inputs = stats.skipped_inputs(),
                    recreated_outputs = stats.recreated_outputs,
                    "applied a block the way the node does, so this utxo set is not the one the rules give"
                );

                for left in stats.skipped.iter() {
                    warn!(
                        slot = blockd.slot(),
                        block = %blockd.hash(),
                        tx = %left.tx,
                        input_tx = %left.input.0,
                        input_index = left.input.1,
                        "left an input unconsumed because nothing held it"
                    );
                }
            }

            Some(delta)
        } else {
            None
        };

        let consumed: Option<std::collections::HashSet<TxoRef>> = lenient_delta
            .as_ref()
            .map(|d| d.consumed_utxo.keys().cloned().collect());

        let mut builder = DeltaBuilder::new(
            genesis.clone(),
            *protocol,
            &active_params,
            epoch,
            epoch_start,
            block,
            &batch.utxos_decoded,
            std::mem::take(&mut dormancy),
            consumed,
        );

        builder.crawl()?;

        dormancy = builder.take_dormancy();

        // TODO: we treat the UTxO set differently due to tech-debt. We should migrate
        // this into the entity system. (#1042)
        let utxos = match lenient_delta {
            Some(delta) => delta,
            None => {
                let blockd = block.decoded();
                let blockd = blockd.view();
                utxoset::compute_apply_delta(blockd, &batch.utxos_decoded)?
            }
        };

        block.utxo_delta = Some(utxos);
    }

    Ok(())
}

/// Dijkstra blocks carrying a governance proposal at body key 20 and a DRep
/// vote at body key 19. Every carrier of either key on the Musashi chain is
/// Conway, thirteen of them at slots 780 to 45736, so a Dijkstra one is built
/// here rather than harvested.
#[cfg(test)]
pub(crate) mod dijkstra_fixture {
    use std::sync::Arc;

    use pallas::codec::{
        minicbor,
        utils::{Bytes, KeepRaw, MaybeIndefArray, Nullable},
    };
    use pallas::crypto::hash::Hash;
    use pallas::ledger::primitives::{
        conway::{Anchor, GovActionId, Vote, Voter, VotingProcedure, VotingProcedures},
        dijkstra, VrfCert,
    };

    use crate::OwnedMultiEraBlock;

    /// The era the block wrapper names for Dijkstra.
    const DIJKSTRA_BLOCK_WRAPPER: u16 = 8;

    pub const SLOT: u64 = 1_000;

    /// The reward account every proposal in these blocks pays its deposit
    /// refund to, a stake address on the test network.
    pub fn reward_account() -> Bytes {
        Bytes::from(vec![0xe0; 29])
    }

    pub fn anchor() -> Anchor {
        Anchor {
            url: "https://example.com".to_string(),
            content_hash: Hash::<32>::from([0x44u8; 32]),
        }
    }

    /// The DRep key hash of the voter in [`votes`].
    pub fn voter_hash() -> Hash<28> {
        Hash::<28>::from([0x66u8; 28])
    }

    /// The proposal the vote in [`votes`] is cast on.
    pub fn voted_proposal() -> GovActionId {
        GovActionId {
            transaction_id: Hash::<32>::from([0x55u8; 32]),
            action_index: 0,
        }
    }

    fn votes() -> VotingProcedures {
        let mut votes = std::collections::BTreeMap::new();
        votes.insert(
            voted_proposal(),
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );

        let mut procedures = std::collections::BTreeMap::new();
        procedures.insert(Voter::DRepKey(voter_hash()), votes);

        procedures
    }

    /// A body carrying one proposal of the action given at key 20 and one
    /// DRep vote at key 19. It consumes and produces nothing, so a crawl of
    /// the block it goes in needs no resolved UTxOs.
    pub fn body_with_governance(action: dijkstra::GovAction) -> dijkstra::TransactionBody<'static> {
        let proposal = dijkstra::ProposalProcedure {
            deposit: 100_000_000,
            reward_account: reward_account(),
            gov_action: action,
            anchor: anchor(),
        };

        dijkstra::TransactionBody {
            inputs: dijkstra::Set::from(vec![]),
            outputs: MaybeIndefArray::Def(vec![]),
            fee: 170_000,
            ttl: None,
            certificates: None,
            withdrawals: None,
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: None,
            script_data_hash: None,
            collateral: None,
            guards: None,
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: None,
            voting_procedures: Some(votes()),
            proposal_procedures: Some(dijkstra::NonEmptySet::try_from(vec![proposal]).unwrap()),
            treasury_value: None,
            donation: None,
            sub_transactions: None,
            required_top_level_guards: None,
            direct_deposits: None,
            account_balance_intervals: None,
            starting_account_balance_intervals: None,
        }
    }

    /// A body that spends and creates nothing, donates the amount given, and
    /// carries one sub transaction whose body holds the proposal and the vote of
    /// [`body_with_governance`] and donates the other amount given.
    pub fn body_with_sub_transaction(
        donation: u64,
        sub_donation: u64,
    ) -> dijkstra::TransactionBody<'static> {
        let governance = body_with_governance(dijkstra::GovAction::Information);

        let sub_body = dijkstra::SubTransactionBody {
            inputs: dijkstra::Set::from(vec![]),
            outputs: MaybeIndefArray::Def(vec![]),
            ttl: None,
            certificates: None,
            withdrawals: None,
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: None,
            script_data_hash: None,
            guards: None,
            network_id: None,
            reference_inputs: None,
            voting_procedures: governance.voting_procedures,
            proposal_procedures: governance.proposal_procedures,
            treasury_value: None,
            donation: Some(dijkstra::PositiveCoin::try_from(sub_donation).unwrap()),
            required_top_level_guards: None,
            direct_deposits: None,
            account_balance_intervals: None,
        };

        let sub = dijkstra::SubTransaction {
            sub_transaction_body: KeepRaw::from(sub_body),
            transaction_witness_set: KeepRaw::from(witness_set()),
            auxiliary_data: Nullable::Null,
        };

        dijkstra::TransactionBody {
            voting_procedures: None,
            proposal_procedures: None,
            donation: Some(dijkstra::PositiveCoin::try_from(donation).unwrap()),
            sub_transactions: Some(dijkstra::NonEmptySet::try_from(vec![sub]).unwrap()),
            ..body_with_governance(dijkstra::GovAction::Information)
        }
    }

    fn witness_set() -> dijkstra::WitnessSet<'static> {
        dijkstra::WitnessSet {
            vkeywitness: None,
            native_script: None,
            bootstrap_witness: None,
            plutus_v1_script: None,
            plutus_data: None,
            redeemer: None,
            plutus_v2_script: None,
            plutus_v3_script: None,
        }
    }

    /// A one transaction ranking block at [`SLOT`], with the transaction's
    /// producer verdict set to the value given.
    pub fn block(body: dijkstra::TransactionBody<'static>, success: bool) -> OwnedMultiEraBlock {
        let header_body = dijkstra::HeaderBody {
            block_number: 1,
            slot: SLOT,
            prev_hash: Some(Hash::from([9u8; 32])),
            issuer_vkey: Bytes::from(vec![0x10, 0x11]),
            vrf_vkey: Bytes::from(vec![0x12, 0x13]),
            vrf_result: VrfCert(Bytes::from(vec![0x14]), Bytes::from(vec![0x15])),
            block_body_size: 0,
            block_body_hash: Hash::from([0u8; 32]),
            operational_cert: pallas::ledger::primitives::babbage::OperationalCert {
                operational_cert_hot_vkey: Bytes::from(vec![0x16]),
                operational_cert_sequence_number: 1,
                operational_cert_kes_period: 0,
                operational_cert_sigma: Bytes::from(vec![0x17]),
            },
            protocol_version: (12, 0),
            block_body_contains_leios_cert: false,
            eb_announcement: Nullable::Null,
        };

        let header = dijkstra::Header {
            header_body,
            body_signature: Bytes::from(vec![0x18]),
        };

        let tx = dijkstra::BlockTransaction {
            transaction_body: KeepRaw::from(body),
            transaction_witness_set: KeepRaw::from(witness_set()),
            auxiliary_data: Nullable::Null,
            success,
        };

        let block = dijkstra::Block {
            header: KeepRaw::from(header),
            block_body: dijkstra::BlockBody {
                transactions: MaybeIndefArray::Def(vec![tx]),
                leios_certificate: Nullable::Null,
                peras_certificate: Nullable::Null,
            },
        };

        let raw = minicbor::to_vec((DIJKSTRA_BLOCK_WRAPPER, block)).unwrap();

        OwnedMultiEraBlock::decode(Arc::new(raw)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dolos_core::NsKey;
    use pallas::codec::utils::{NonEmptySet, NonZeroInt, Set};
    use pallas::crypto::hash::Hash;
    use pallas::ledger::primitives::conway::{
        Anchor, Certificate, GovAction, GovActionId, ProposalProcedure, TransactionBody, Vote,
        Voter, VotingProcedure,
    };
    use pallas::ledger::primitives::{StakeCredential, UnitInterval};

    use crate::model::{EpochStatsUpdate, PParamValue as Val};
    use crate::{CardanoDelta, OwnedMultiEraBlock};

    use super::*;

    const SLOT: u64 = 1_000;
    const EPOCH: Epoch = 42;
    const EPOCH_START: u64 = 900;

    /// A transaction body with no inputs and no outputs — `consumes()` and
    /// `produces()` are empty under either validity, so the crawl needs no
    /// resolved UTxOs — carrying one of every body element the LEDGER rule
    /// gates: a stake registration, a DRep registration, a pool registration,
    /// a withdrawal, a mint, a proposal and a vote.
    fn loaded_tx_body() -> TransactionBody<'static> {
        let mut mint = BTreeMap::new();
        let mut assets = BTreeMap::new();
        assets.insert(
            pallas::ledger::primitives::Bytes::from(vec![0xaa, 0xbb]),
            NonZeroInt::try_from(7i64).unwrap(),
        );
        mint.insert(Hash::<28>::from([0x33u8; 28]), assets);

        let mut withdrawals = BTreeMap::new();
        withdrawals.insert(
            pallas::codec::utils::Bytes::from(vec![0xe0; 29]),
            1_000_000u64,
        );

        let anchor = Anchor {
            url: "https://example.com".to_string(),
            content_hash: Hash::<32>::from([0x44u8; 32]),
        };

        let proposal = ProposalProcedure {
            deposit: 100_000_000,
            reward_account: pallas::codec::utils::Bytes::from(vec![0xe0; 29]),
            gov_action: GovAction::Information,
            anchor: anchor.clone(),
        };

        let mut votes = BTreeMap::new();
        votes.insert(
            GovActionId {
                transaction_id: Hash::<32>::from([0x55u8; 32]),
                action_index: 0,
            },
            VotingProcedure {
                vote: Vote::Yes,
                anchor: None,
            },
        );
        let mut voting_procedures = BTreeMap::new();
        voting_procedures.insert(Voter::DRepKey(Hash::<28>::from([0x66u8; 28])), votes);

        let certs = vec![
            Certificate::StakeRegistration(StakeCredential::AddrKeyhash(Hash::<28>::from(
                [0x11u8; 28],
            ))),
            Certificate::RegDRepCert(
                StakeCredential::AddrKeyhash(Hash::<28>::from([0x66u8; 28])),
                500_000_000,
                Some(anchor.clone()),
            ),
            // Operator differs from the block issuer, so a `pools` row for it
            // is unambiguously the certificate's doing and not `visit_root`'s.
            Certificate::PoolRegistration {
                operator: Hash::<28>::from([0x77u8; 28]),
                vrf_keyhash: Hash::<32>::from([0x88u8; 32]),
                pledge: 1,
                cost: 2,
                margin: UnitInterval {
                    numerator: 1,
                    denominator: 2,
                },
                reward_account: pallas::codec::utils::Bytes::from(vec![0xe0; 29]),
                pool_owners: Set::from(vec![]),
                relays: vec![],
                pool_metadata: None,
            },
        ];

        TransactionBody {
            inputs: Set::from(vec![]),
            outputs: vec![],
            fee: 170_000,
            ttl: None,
            certificates: Some(NonEmptySet::try_from(certs).unwrap()),
            withdrawals: Some(withdrawals),
            auxiliary_data_hash: None,
            validity_interval_start: None,
            mint: Some(mint),
            script_data_hash: None,
            collateral: None,
            required_signers: None,
            network_id: None,
            collateral_return: None,
            total_collateral: None,
            reference_inputs: None,
            voting_procedures: Some(voting_procedures),
            proposal_procedures: Some(NonEmptySet::try_from(vec![proposal]).unwrap()),
            treasury_value: None,
            donation: None,
        }
    }

    fn test_pparams() -> PParamsSet {
        PParamsSet::default()
            .with(Val::ProtocolVersion((10, 0)))
            .with(Val::KeyDeposit(2_000_000))
            .with(Val::PoolDeposit(500_000_000))
            .with(Val::DrepDeposit(500_000_000))
            .with(Val::DrepInactivityPeriod(20))
            .with(Val::GovernanceActionValidityPeriod(6))
    }

    /// Crawl a one-transaction Conway block whose transaction is valid or
    /// phase-2-invalid, and hand back the deltas it produced.
    fn crawl_single_tx_block(valid: bool) -> WorkDeltas {
        let (_, raw) =
            dolos_testing::blocks::make_conway_block_with_tx(SLOT, loaded_tx_body(), None, valid);

        crawl_block(OwnedMultiEraBlock::decode(raw).unwrap(), 10)
    }

    fn crawl_block(block: OwnedMultiEraBlock, protocol: u16) -> WorkDeltas {
        let mut work = WorkBlock::new(block);

        let genesis = Arc::new(crate::load_test_genesis("preview"));
        let pparams = test_pparams();
        let utxos = HashMap::new();

        let mut builder = DeltaBuilder::new(
            genesis,
            protocol,
            &pparams,
            EPOCH,
            EPOCH_START,
            &mut work,
            &utxos,
            DormancyContext::default(),
            None,
        );

        builder.crawl().unwrap();

        work.deltas
    }

    fn keys_in(deltas: &WorkDeltas, ns: &str) -> Vec<EntityKey> {
        deltas
            .entities
            .keys()
            .filter(|NsKey(namespace, _)| *namespace == ns)
            .map(|NsKey(_, key)| key.clone())
            .collect()
    }

    fn epoch_stats(deltas: &WorkDeltas) -> EpochStatsUpdate {
        deltas
            .entities
            .iter()
            .filter(|(NsKey(ns, _), _)| *ns == "epochs")
            .flat_map(|(_, group)| group.iter())
            .find_map(|delta| match delta {
                CardanoDelta::EpochStatsUpdate(stats) => Some((**stats).clone()),
                _ => None,
            })
            .expect("epoch stats delta")
    }

    /// The operator hash of the fixture's `PoolRegistration` certificate,
    /// which `PoolRegistration::key` uses as the `pools` entity key.
    fn cert_pool_key() -> EntityKey {
        EntityKey::from([0x77u8; 28].as_slice())
    }

    /// The block issuer's operator hash, which `PoolStateVisitor::visit_root`
    /// emits a `MintedBlocksInc` for on every block regardless of validity.
    fn block_issuer_key() -> EntityKey {
        let issuer =
            dolos_testing::blocks::make_conway_block_with_tx(SLOT, loaded_tx_body(), None, true);
        let block = OwnedMultiEraBlock::decode(issuer.1).unwrap();
        let key = block.view().header().issuer_vkey().unwrap().to_vec();
        let operator = pallas::crypto::hash::Hasher::<224>::hash(&key);
        EntityKey::from(operator.as_slice())
    }

    #[test]
    fn invalid_tx_contributes_no_entity_state() {
        let deltas = crawl_single_tx_block(false);

        for ns in ["accounts", "dreps", "proposals", "assets"] {
            assert!(
                keys_in(&deltas, ns).is_empty(),
                "phase-2-invalid tx wrote to the `{ns}` namespace"
            );
        }

        assert_eq!(
            keys_in(&deltas, "pools"),
            vec![block_issuer_key()],
            "the only pools row must be the block issuer's minted-blocks counter"
        );

        let stats = epoch_stats(&deltas);
        assert_eq!(stats.new_accounts, 0);
        assert_eq!(stats.removed_accounts, 0);
        assert_eq!(stats.drep_deposits, 0);
        assert_eq!(stats.drep_refunds, 0);
        assert_eq!(stats.proposal_deposits, 0);
        assert_eq!(stats.withdrawals, 0);
        assert_eq!(stats.reserve_mirs, 0);
        assert_eq!(stats.treasury_mirs, 0);
        assert!(stats.registered_pools.is_empty());

        // The transaction is still counted and its collateral still priced.
        assert_eq!(stats.tx_count, 1);
    }

    #[test]
    fn valid_tx_contributes_entity_state() {
        let deltas = crawl_single_tx_block(true);

        for ns in ["accounts", "dreps", "proposals", "assets", "pools"] {
            assert!(
                !keys_in(&deltas, ns).is_empty(),
                "valid tx wrote nothing to the `{ns}` namespace — the fixture is wrong"
            );
        }

        // `visit_root` alone fills `pools` with the block issuer, so only the
        // certificate's own operator key proves the cert path ran.
        assert!(
            keys_in(&deltas, "pools").contains(&cert_pool_key()),
            "valid tx did not register the certificate's pool — the fixture is wrong"
        );

        let stats = epoch_stats(&deltas);
        assert!(stats.new_accounts > 0);
        assert!(stats.drep_deposits > 0);
        assert!(stats.proposal_deposits > 0);
        assert!(stats.withdrawals > 0);
        assert!(!stats.registered_pools.is_empty());
        assert_eq!(stats.tx_count, 1);
    }

    fn crawl_dijkstra_governance_block(valid: bool) -> WorkDeltas {
        let block = dijkstra_fixture::block(
            dijkstra_fixture::body_with_governance(
                pallas::ledger::primitives::dijkstra::GovAction::Information,
            ),
            valid,
        );

        crawl_block(block, 12)
    }

    fn proposals_recorded(deltas: &WorkDeltas) -> Vec<&crate::NewProposalV2> {
        deltas
            .entities
            .values()
            .flatten()
            .filter_map(|delta| match delta {
                CardanoDelta::NewProposalV2(x) => Some(x.as_ref()),
                _ => None,
            })
            .collect()
    }

    fn votes_recorded(deltas: &WorkDeltas) -> Vec<&crate::VoteCast> {
        deltas
            .entities
            .values()
            .flatten()
            .filter_map(|delta| match delta {
                CardanoDelta::VoteCast(x) => Some(x.as_ref()),
                _ => None,
            })
            .collect()
    }

    /// The must-fire case. Musashi is a Dijkstra chain, so the proposal and
    /// the vote a Dijkstra transaction carries are the ones a follower of it
    /// has to record.
    #[test]
    fn a_dijkstra_transaction_records_its_proposal_and_its_vote() {
        let deltas = crawl_dijkstra_governance_block(true);

        let proposals = proposals_recorded(&deltas);
        assert_eq!(proposals.len(), 1, "the proposal was not recorded");
        assert_eq!(proposals[0].slot, dijkstra_fixture::SLOT);
        assert_eq!(proposals[0].deposit, Some(100_000_000));
        assert_eq!(proposals[0].anchor, Some(dijkstra_fixture::anchor()));

        let votes = votes_recorded(&deltas);
        assert_eq!(votes.len(), 1, "the vote was not recorded");
        assert_eq!(
            votes[0].proposal_tx,
            dijkstra_fixture::voted_proposal().transaction_id
        );

        // A DRep that votes has its expiry refreshed, so the vote writes a
        // row under the voter's key.
        let voter = crate::drep_to_entity_key(&pallas::ledger::primitives::conway::DRep::Key(
            dijkstra_fixture::voter_hash(),
        ));
        assert!(
            keys_in(&deltas, "dreps").contains(&voter),
            "the voting DRep got no row"
        );
    }

    /// The must-not case. A phase-2-invalid transaction runs neither GOV nor
    /// CERTS, so neither its proposal nor its vote may be recorded.
    #[test]
    fn an_invalid_dijkstra_transaction_records_no_governance() {
        let deltas = crawl_dijkstra_governance_block(false);

        assert!(proposals_recorded(&deltas).is_empty());
        assert!(votes_recorded(&deltas).is_empty());
        assert!(keys_in(&deltas, "dreps").is_empty());
    }

    const DONATION: u64 = 3_000_000;
    const SUB_DONATION: u64 = 5_000_000;

    fn crawl_block_with_sub_transaction(valid: bool) -> WorkDeltas {
        let block = dijkstra_fixture::block(
            dijkstra_fixture::body_with_sub_transaction(DONATION, SUB_DONATION),
            valid,
        );

        crawl_block(block, 12)
    }

    /// The must-fire case. A sub transaction of a valid transaction records its
    /// proposal and its vote and donates to the treasury, and the block still
    /// counts one transaction.
    #[test]
    fn a_sub_transaction_of_a_valid_transaction_is_applied() {
        let deltas = crawl_block_with_sub_transaction(true);
        let stats = epoch_stats(&deltas);

        assert_eq!(
            (
                proposals_recorded(&deltas).len(),
                votes_recorded(&deltas).len(),
                stats.treasury_donations,
                stats.tx_count,
            ),
            (1, 1, DONATION + SUB_DONATION, 1)
        );
    }

    /// The must-not case. Neither a phase 2 invalid transaction nor the sub
    /// transaction it carries records governance or donates to the treasury,
    /// and the block still counts one transaction.
    #[test]
    fn an_invalid_transaction_and_its_sub_transaction_donate_nothing() {
        let deltas = crawl_block_with_sub_transaction(false);
        let stats = epoch_stats(&deltas);

        assert_eq!(
            (
                proposals_recorded(&deltas).len(),
                votes_recorded(&deltas).len(),
                stats.treasury_donations,
                stats.tx_count,
            ),
            (0, 0, 0, 1)
        );
    }
}
