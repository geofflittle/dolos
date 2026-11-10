//!
//! Cardano-specific async query helpers for `AsyncQueryFacade`.

use pallas::{
    crypto::hash::Hash,
    ledger::{
        primitives::conway::{DatumOption, PlutusData},
        traverse::{ComputeHash, MultiEraBlock, OriginalHash},
    },
};

use dolos_core::{
    archive::ArchiveStore, AsyncQueryFacade, BlockBody, BlockSlot, ChainError, Domain, DomainError,
    EntityKey, StateStore as _, TagDimension, TxHash, TxoRef,
};

use crate::indexes::dimensions::archive;
use crate::model::{DatumState, DATUM_NS};

use futures_core::Stream;

/// Number of slots to fetch per chunk when iterating over tagged blocks.
const SLOT_CHUNK_SIZE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOrder {
    Asc,
    Desc,
}

pub use crate::pallas_extras::ScriptLanguage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptData {
    pub language: ScriptLanguage,
    pub script: Vec<u8>,
}

#[async_trait::async_trait]
pub trait AsyncCardanoQueryExt<D: Domain> {
    fn blocks_by_address_stream(
        &self,
        address: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_payment_stream(
        &self,
        payment: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_stake_stream(
        &self,
        stake: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_asset_stream(
        &self,
        asset: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_policy_stream(
        &self,
        policy: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_account_certs_stream(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_pool_certs_stream(
        &self,
        pool: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_account_withdrawals_stream(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    fn blocks_by_metadata_stream(
        &self,
        label: u64,
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static;

    async fn blocks_by_address(
        &self,
        address: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_payment(
        &self,
        payment: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_stake(
        &self,
        stake: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_asset(
        &self,
        asset: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_account_certs(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_pool_certs(
        &self,
        pool: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_account_withdrawals(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn blocks_by_metadata(
        &self,
        label: u64,
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>;

    async fn plutus_data(&self, datum_hash: &Hash<32>) -> Result<Option<PlutusData>, DomainError>;

    async fn get_datum(&self, datum_hash: &Hash<32>) -> Result<Option<Vec<u8>>, DomainError>;

    async fn script_by_hash(
        &self,
        script_hash: &Hash<28>,
    ) -> Result<Option<ScriptData>, DomainError>;

    async fn tx_by_spent_txo(&self, spent_txo: &[u8]) -> Result<Option<TxHash>, DomainError>;
}

#[async_trait::async_trait]
impl<D: Domain> AsyncCardanoQueryExt<D> for AsyncQueryFacade<D>
where
    D: Clone + Send + Sync + 'static,
{
    fn blocks_by_address_stream(
        &self,
        address: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::ADDRESS,
            address.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_payment_stream(
        &self,
        payment: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::PAYMENT,
            payment.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_stake_stream(
        &self,
        stake: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::STAKE,
            stake.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_asset_stream(
        &self,
        asset: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::ASSET,
            asset.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_policy_stream(
        &self,
        policy: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::POLICY,
            policy.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_account_certs_stream(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::ACCOUNT_CERTS,
            account.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_pool_certs_stream(
        &self,
        pool: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::POOL_CERTS,
            pool.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_account_withdrawals_stream(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::ACCOUNT_WITHDRAWALS,
            account.to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    fn blocks_by_metadata_stream(
        &self,
        label: u64,
        start_slot: BlockSlot,
        end_slot: BlockSlot,
        order: SlotOrder,
    ) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
    {
        blocks_by_tag_stream(
            (*self).clone(),
            archive::METADATA,
            label.to_be_bytes().to_vec(),
            start_slot,
            end_slot,
            order,
        )
    }

    async fn blocks_by_address(
        &self,
        address: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::ADDRESS, address, start_slot, end_slot).await
    }

    async fn blocks_by_payment(
        &self,
        payment: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::PAYMENT, payment, start_slot, end_slot).await
    }

    async fn blocks_by_stake(
        &self,
        stake: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::STAKE, stake, start_slot, end_slot).await
    }

    async fn blocks_by_asset(
        &self,
        asset: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::ASSET, asset, start_slot, end_slot).await
    }

    async fn blocks_by_account_certs(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::ACCOUNT_CERTS, account, start_slot, end_slot).await
    }

    async fn blocks_by_pool_certs(
        &self,
        pool: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(self, archive::POOL_CERTS, pool, start_slot, end_slot).await
    }

    async fn blocks_by_account_withdrawals(
        &self,
        account: &[u8],
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(
            self,
            archive::ACCOUNT_WITHDRAWALS,
            account,
            start_slot,
            end_slot,
        )
        .await
    }

    async fn blocks_by_metadata(
        &self,
        label: u64,
        start_slot: BlockSlot,
        end_slot: BlockSlot,
    ) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError> {
        blocks_by_tag(
            self,
            archive::METADATA,
            &label.to_be_bytes(),
            start_slot,
            end_slot,
        )
        .await
    }

    async fn plutus_data(&self, datum_hash: &Hash<32>) -> Result<Option<PlutusData>, DomainError> {
        let end_slot = self
            .run_blocking(move |domain| {
                Ok(domain
                    .archive()
                    .get_tip()?
                    .map(|(slot, _)| slot)
                    .unwrap_or_default())
            })
            .await?;

        let datum_hash = *datum_hash;
        find_first_by_tag(
            self,
            archive::DATUM,
            datum_hash.as_slice().to_vec(),
            0,
            end_slot,
            |block| datum_in_block(block, datum_hash),
        )
        .await
    }

    async fn get_datum(&self, datum_hash: &Hash<32>) -> Result<Option<Vec<u8>>, DomainError> {
        let key = EntityKey::from(*datum_hash);
        self.run_blocking(move |domain| {
            let datum_state: Option<DatumState> =
                domain.state().read_entity_typed(DATUM_NS, &key)?;
            Ok(datum_state.map(|s| s.bytes))
        })
        .await
    }

    async fn script_by_hash(
        &self,
        script_hash: &Hash<28>,
    ) -> Result<Option<ScriptData>, DomainError> {
        let end_slot = self
            .run_blocking(move |domain| {
                Ok(domain
                    .archive()
                    .get_tip()?
                    .map(|(slot, _)| slot)
                    .unwrap_or_default())
            })
            .await?;

        let script_hash = *script_hash;
        find_first_by_tag(
            self,
            archive::SCRIPT,
            script_hash.as_slice().to_vec(),
            0,
            end_slot,
            |block| script_in_block(block, script_hash),
        )
        .await
    }

    async fn tx_by_spent_txo(&self, spent_txo: &[u8]) -> Result<Option<TxHash>, DomainError> {
        let spent = spent_txo.to_vec();

        let end_slot = self
            .run_blocking(move |domain| {
                Ok(domain
                    .archive()
                    .get_tip()?
                    .map(|(slot, _)| slot)
                    .unwrap_or_default())
            })
            .await?;

        find_first_by_tag(
            self,
            archive::SPENT_TXO,
            spent.clone(),
            0,
            end_slot,
            |block| {
                for (_, tx) in dolos_core::applied_txs(block) {
                    for input in tx.inputs() {
                        let bytes: Vec<u8> = TxoRef::from(&input).into();
                        if bytes.as_slice() == spent.as_slice() {
                            return Some(tx.hash());
                        }
                    }
                }
                None
            },
        )
        .await
    }
}

/// The datum with this hash in the witnesses, on the outputs or in the
/// redeemers of a transaction of the block.
fn datum_in_block(block: &MultiEraBlock, datum_hash: Hash<32>) -> Option<PlutusData> {
    for tx in block.txs() {
        let searched = crate::pallas_extras::for_each_applied_tx(&tx, |tx| {
            // Check witnesses
            if let Some(plutus_data) = tx.find_plutus_data(&datum_hash) {
                return Err(plutus_data.clone().unwrap());
            }
            // Check inline datums
            for (_, output) in tx.produces() {
                if let Some(DatumOption::Data(data)) = output.datum() {
                    if data.original_hash() == datum_hash {
                        return Err(data.clone().unwrap().unwrap());
                    }
                }
            }
            // Check redeemer data
            for redeemer in tx.redeemers() {
                if redeemer.data().compute_hash() == datum_hash {
                    return Err(redeemer.data().clone());
                }
            }
            Ok(())
        });

        if let Err(found) = searched {
            return Some(found);
        }
    }
    None
}

/// The script with this hash in the witnesses or on the outputs of a
/// transaction of the block.
fn script_in_block(block: &MultiEraBlock, script_hash: Hash<28>) -> Option<ScriptData> {
    for tx in block.txs() {
        let searched = crate::pallas_extras::for_each_applied_tx(&tx, |tx| {
            match crate::pallas_extras::tx_scripts(tx)
                .into_iter()
                .find(|parts| parts.hash == script_hash)
            {
                Some(parts) => Err(ScriptData {
                    language: parts.language,
                    script: parts.bytes,
                }),
                None => Ok(()),
            }
        });

        if let Err(found) = searched {
            return Some(found);
        }
    }
    None
}

async fn blocks_by_tag<D>(
    facade: &AsyncQueryFacade<D>,
    dimension: TagDimension,
    key: &[u8],
    start_slot: BlockSlot,
    end_slot: BlockSlot,
) -> Result<Vec<(BlockSlot, Option<BlockBody>)>, DomainError>
where
    D: Domain + Clone + Send + Sync + 'static,
{
    let slots = facade
        .slots_by_tag(dimension, key.to_vec(), start_slot, end_slot)
        .await?;

    let mut out = Vec::with_capacity(slots.len());
    for slot in slots {
        let block = facade.block_by_slot(slot).await?;
        out.push((slot, block));
    }

    Ok(out)
}

/// Find the first match in blocks tagged with a specific dimension/key.
/// Uses chunked slot iteration (512 at a time) to avoid memory spikes.
async fn find_first_by_tag<D, F, T>(
    facade: &AsyncQueryFacade<D>,
    dimension: TagDimension,
    key: Vec<u8>,
    start_slot: BlockSlot,
    end_slot: BlockSlot,
    mut predicate: F,
) -> Result<Option<T>, DomainError>
where
    D: Domain + Clone + Send + Sync + 'static,
    F: FnMut(&MultiEraBlock) -> Option<T>,
{
    let mut current_start = start_slot;

    loop {
        let key_clone = key.clone();
        let chunk_start = current_start;

        let slots: Vec<BlockSlot> = facade
            .run_blocking(move |domain| {
                Ok(domain
                    .archive()
                    .slots_by_tag(dimension, &key_clone, chunk_start, end_slot)?
                    .take(SLOT_CHUNK_SIZE)
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .await?;

        if slots.is_empty() {
            return Ok(None);
        }

        for slot in &slots {
            let Some(raw) = facade.block_by_slot(*slot).await? else {
                continue;
            };

            let block = MultiEraBlock::decode(raw.as_slice())
                .map_err(|e| DomainError::ChainError(ChainError::DecodingError(e)))?;

            if let Some(result) = predicate(&block) {
                return Ok(Some(result));
            }
        }

        // Update start for next chunk
        if let Some(&last) = slots.last() {
            current_start = last + 1;
            if current_start > end_slot {
                break;
            }
        }
    }

    Ok(None)
}

fn blocks_by_tag_stream<D>(
    facade: AsyncQueryFacade<D>,
    dimension: TagDimension,
    key: Vec<u8>,
    mut start_slot: BlockSlot,
    mut end_slot: BlockSlot,
    order: SlotOrder,
) -> impl Stream<Item = Result<(BlockSlot, Option<BlockBody>), DomainError>> + Send + 'static
where
    D: Domain + Clone + Send + Sync + 'static,
{
    async_stream::try_stream! {
        loop {
            // we fetch slots in chunks to avoid holding the index read transaction for too long
            // and to avoid loading all slots into memory at once
            let slots: Vec<BlockSlot> = facade
                .run_blocking({
                    let key = key.clone();
                    move |domain| {
                        let iter = domain
                            .archive()
                            .slots_by_tag(dimension, &key, start_slot, end_slot)?;

                        let slots = match order {
                            SlotOrder::Asc => iter.take(SLOT_CHUNK_SIZE).collect::<Result<Vec<_>, _>>()?,
                            SlotOrder::Desc => iter.rev().take(SLOT_CHUNK_SIZE).collect::<Result<Vec<_>, _>>()?,
                        };

                        Ok(slots)
                    }
                })
                .await?;

            if slots.is_empty() {
                break;
            }

            for slot in slots {
                let block = facade.block_by_slot(slot).await?;
                yield (slot, block);

                // update bounds to avoid re-fetching same slots in next iteration
                match order {
                    SlotOrder::Asc => start_slot = slot + 1,
                    SlotOrder::Desc => {
                        if slot == 0 {
                            return;
                        }
                        end_slot = slot - 1;
                    }
                }
            }

            match order {
                SlotOrder::Asc if start_slot > end_slot => break,
                SlotOrder::Desc if end_slot < start_slot => break,
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pallas::codec::minicbor;
    use pallas::codec::utils::{KeepRaw, MaybeIndefArray};
    use pallas::crypto::hash::Hasher;
    use pallas::ledger::primitives::dijkstra;

    use super::*;
    use crate::indexes::CardanoIndexDeltaBuilder;

    /// A block whose transaction lists a sub transaction, with a native script
    /// added to the witnesses of the sub transaction and a Plutus V4 script
    /// added to the witnesses of the transaction that lists it.
    fn block_with_scripts() -> (Vec<u8>, Hash<28>, Hash<28>) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data/musashi-w36/ranking-sub-transaction.block");
        let text = std::fs::read_to_string(&path).unwrap();
        let cbor = hex::decode(text.trim()).unwrap();
        let (era, mut block): (u16, dijkstra::Block) = minicbor::decode(&cbor).unwrap();

        let native = dijkstra::NativeScript::ScriptPubkey(Hash::new([9; 28]));
        let native_hash = Hasher::<224>::hash_tagged(&minicbor::to_vec(&native).unwrap(), 0);
        let v4 = dijkstra::PlutusScript::<4>(vec![1, 2, 3].into());
        let v4_hash = v4.compute_hash();

        let mut txs: Vec<_> = block.block_body.transactions.to_vec();
        let carrier = txs
            .iter_mut()
            .find(|tx| tx.transaction_body.sub_transactions.is_some())
            .expect("a transaction lists a sub transaction");
        let mut body = (*carrier.transaction_body).clone();
        let mut subs = body.sub_transactions.take().unwrap().into_vec();
        let mut witnesses = (*subs[0].transaction_witness_set).clone();
        witnesses.native_script = dijkstra::NonEmptySet::from_vec(vec![KeepRaw::from(native)]);
        subs[0].transaction_witness_set = witnesses.into();
        body.sub_transactions = dijkstra::NonEmptySet::from_vec(subs);
        carrier.transaction_body = body.into();
        let mut witnesses = (*carrier.transaction_witness_set).clone();
        witnesses.plutus_v4_script = dijkstra::NonEmptySet::from_vec(vec![v4]);
        carrier.transaction_witness_set = witnesses.into();
        block.block_body.transactions = MaybeIndefArray::Def(txs);

        (
            minicbor::to_vec((era, block)).unwrap(),
            native_hash,
            v4_hash,
        )
    }

    /// A block whose transaction lists a sub transaction, with a datum added to
    /// the witnesses of the sub transaction only.
    fn block_with_sub_datum() -> (Vec<u8>, Hash<32>, PlutusData) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data/musashi-w36/ranking-sub-transaction.block");
        let text = std::fs::read_to_string(&path).unwrap();
        let cbor = hex::decode(text.trim()).unwrap();
        let (era, mut block): (u16, dijkstra::Block) = minicbor::decode(&cbor).unwrap();

        let datum = PlutusData::BoundedBytes(vec![5, 6, 7].into());
        let datum_hash = Hasher::<256>::hash(&minicbor::to_vec(&datum).unwrap());

        let mut txs: Vec<_> = block.block_body.transactions.to_vec();
        let carrier = txs
            .iter_mut()
            .find(|tx| tx.transaction_body.sub_transactions.is_some())
            .expect("a transaction lists a sub transaction");
        let mut body = (*carrier.transaction_body).clone();
        let mut subs = body.sub_transactions.take().unwrap().into_vec();
        let mut witnesses = (*subs[0].transaction_witness_set).clone();
        witnesses.plutus_data =
            dijkstra::NonEmptySet::from_vec(vec![KeepRaw::from(datum.clone())]).map(KeepRaw::from);
        subs[0].transaction_witness_set = witnesses.into();
        body.sub_transactions = dijkstra::NonEmptySet::from_vec(subs);
        carrier.transaction_body = body.into();
        block.block_body.transactions = MaybeIndefArray::Def(txs);

        (minicbor::to_vec((era, block)).unwrap(), datum_hash, datum)
    }

    /// The keys the archive index tags the block with in one dimension.
    fn tagged(block: &MultiEraBlock, dimension: TagDimension) -> Vec<Vec<u8>> {
        let mut builder = CardanoIndexDeltaBuilder::new();
        builder.index_block(block, &HashMap::new());
        builder
            .build()
            .into_iter()
            .flat_map(|delta| delta.tags)
            .filter(|tag| tag.dimension == dimension)
            .map(|tag| tag.key)
            .collect()
    }

    #[test]
    fn a_datum_the_block_does_not_carry_is_neither_found_nor_tagged() {
        let (cbor, _, _) = block_with_sub_datum();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let absent = Hash::new([7; 32]);

        assert_eq!(datum_in_block(&block, absent), None);
        assert!(!tagged(&block, archive::DATUM).contains(&absent.to_vec()));
    }

    #[test]
    fn a_sub_transaction_datum_is_found_and_tagged() {
        let (cbor, datum_hash, datum) = block_with_sub_datum();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let found = datum_in_block(&block, datum_hash);
        let tagged = tagged(&block, archive::DATUM).contains(&datum_hash.to_vec());

        assert_eq!((found, tagged), (Some(datum), true));
    }

    #[test]
    fn a_script_the_block_does_not_carry_is_neither_found_nor_tagged() {
        let (cbor, _, _) = block_with_scripts();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let absent = Hash::new([7; 28]);

        assert_eq!(script_in_block(&block, absent), None);
        assert!(!tagged(&block, archive::SCRIPT).contains(&absent.to_vec()));
    }

    #[test]
    fn a_sub_transaction_witness_is_found_and_tagged() {
        let (cbor, native, _) = block_with_scripts();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let found = script_in_block(&block, native).map(|s| s.language);
        let tagged = tagged(&block, archive::SCRIPT).contains(&native.to_vec());

        assert_eq!((found, tagged), (Some(ScriptLanguage::Native), true));
    }

    #[test]
    fn a_v4_witness_is_found_and_tagged() {
        let (cbor, _, v4) = block_with_scripts();
        let block = MultiEraBlock::decode(&cbor).unwrap();
        let found = script_in_block(&block, v4);
        let tagged = tagged(&block, archive::SCRIPT).contains(&v4.to_vec());
        let expected = ScriptData {
            language: ScriptLanguage::PlutusV4,
            script: vec![1, 2, 3],
        };

        assert_eq!((found, tagged), (Some(expected), true));
    }
}
