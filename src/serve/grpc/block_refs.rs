//! Shared helpers for resolving `block_ref` metadata for UTxOs returned by
//! `read_utxos` / `search_utxos` across the v1alpha and v1beta query services.
//!
//! Loads the era summary once per request and reads each block that holds a
//! source transaction once.

use std::collections::HashMap;

use dolos_core::{
    async_query::{AsyncQueryFacade, BlockPoint},
    BlockHeight, BlockSlot, Domain, TxHash, TxoRef,
};
use tonic::Status;
use tracing::debug;

/// Chain-agnostic block reference data assembled from a block's point and the
/// request-scoped era summary.
#[derive(Clone, Debug)]
pub struct BlockRefData {
    pub slot: BlockSlot,
    pub hash: [u8; 32],
    pub height: BlockHeight,
    pub timestamp: u64,
}

/// Fetch a `BlockRefData` for every UTxO in `txo_refs`, deduplicating by
/// source transaction and reading each block that holds one once. The
/// returned map is keyed by `TxHash`, with no entry for transactions that are
/// not in the archive (e.g. genesis UTxOs, custom UTxOs, or transactions
/// ahead of the current indexer cursor).
///
/// Storage errors surface as `Status::internal`. Block-decode errors for
/// archive entries that did exist degrade to a `debug!` log and a missing map
/// entry — these indicate node-internal corruption that callers cannot fix.
pub async fn fetch_block_refs<D>(
    domain: &D,
    txo_refs: impl IntoIterator<Item = &TxoRef>,
) -> Result<HashMap<TxHash, BlockRefData>, Status>
where
    D: Domain,
{
    let unique_hashes: Vec<TxHash> = txo_refs
        .into_iter()
        .map(|r| r.0)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if unique_hashes.is_empty() {
        return Ok(HashMap::new());
    }

    let chain_summary = dolos_cardano::eras::load_era_summary::<D>(domain.state())
        .map_err(|e| Status::internal(format!("failed to load era summary: {e}")))?;

    let query = AsyncQueryFacade::new(domain.clone());
    let lookup_failed =
        |e: dolos_core::DomainError| Status::internal(format!("block_ref lookup failed: {e}"));
    let answers = query
        .block_points_by_tx_hashes(unique_hashes)
        .await
        .map_err(lookup_failed)?;
    let mut out = HashMap::new();

    for (tx_hashes, answer) in answers {
        match answer {
            Ok((points, _)) => {
                out.extend(points.into_iter().map(|(tx_hash, point)| {
                    let BlockPoint { slot, hash, height } = point;
                    let data = BlockRefData {
                        slot,
                        hash: *hash,
                        height,
                        timestamp: chain_summary.slot_time(slot) * 1000,
                    };
                    (tx_hash, data)
                }));
            }
            Err(e @ dolos_core::DomainError::ChainError(_)) => {
                for tx_hash in tx_hashes {
                    debug!(
                        tx_hash = %hex::encode(tx_hash),
                        error = %e,
                        "failed to decode block while resolving UTxO block_ref",
                    );
                }
            }
            Err(e) => return Err(lookup_failed(e)),
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dolos_core::{
        indexes::ArchiveIndexDelta, ArchiveStore as _, ArchiveWriter as _, ChainPoint,
    };
    use pallas::{codec::minicbor, ledger::traverse::MultiEraBlock};

    use super::*;

    const BLOCK: &str =
        include_str!("../../../test_data/musashi-w36/ranking-sub-transaction.block");

    /// The block's header in a block whose body no decoder accepts.
    fn header_over_garbage(raw: &[u8]) -> Vec<u8> {
        let mut decoder = minicbor::Decoder::new(raw);
        decoder.array().unwrap();
        let tag = decoder.position();
        decoder.u16().unwrap();
        let tag = &raw[tag..decoder.position()];
        decoder.array().unwrap();
        let start = decoder.position();
        decoder.skip().unwrap();
        let header = &raw[start..decoder.position()];
        [&[0x82][..], tag, &[0x82], header, &[0x40]].concat()
    }

    /// The slot, hash and height of the block that answers each hash.
    fn points(refs: &HashMap<TxHash, BlockRefData>) -> Vec<(TxHash, u64, [u8; 32], u64)> {
        let mut points: Vec<_> = refs
            .iter()
            .map(|(tx, r)| (*tx, r.slot, r.hash, r.height))
            .collect();
        points.sort();
        points
    }

    #[tokio::test]
    async fn a_hash_no_block_holds_has_no_block_ref() {
        let fixture = crate::tests::musashi::sub_transaction_block();
        let raw = hex::decode(BLOCK.trim()).unwrap();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let absent = TxHash::from([0xaa; 32]);
        let refs = [
            TxoRef(fixture.parent, 0),
            TxoRef(fixture.sub, 1),
            TxoRef(absent, 0),
        ];

        let found = fetch_block_refs(&fixture.domain, &refs).await.unwrap();

        let expected = |tx| (tx, block.slot(), *block.hash(), block.number());
        let mut wanted = vec![expected(fixture.parent), expected(fixture.sub)];
        wanted.sort();
        assert_eq!(points(&found), wanted);
    }

    #[tokio::test]
    async fn a_block_ref_is_answered_from_the_header_alone() {
        let fixture = crate::tests::musashi::sub_transaction_block();
        let raw = hex::decode(BLOCK.trim()).unwrap();
        let block = MultiEraBlock::decode(&raw).unwrap();
        let garbage = header_over_garbage(&raw);
        assert!(MultiEraBlock::decode(&garbage).is_err());
        let slot = fixture.slot + 1;
        let tx = TxHash::from([0xee; 32]);
        let writer = fixture.domain.archive().start_writer().unwrap();
        writer
            .apply(
                &ChainPoint::Specific(slot, [0xee; 32].into()),
                &Arc::new(garbage),
            )
            .unwrap();
        writer
            .apply_index(&[ArchiveIndexDelta {
                slot,
                block_hash: vec![0xee; 32],
                block_number: Some(block.number()),
                txs: vec![dolos_core::IndexedTx::unplaced(tx.to_vec())],
                tags: Vec::new(),
            }])
            .unwrap();
        writer.commit().unwrap();

        let found = fetch_block_refs(&fixture.domain, &[TxoRef(tx, 0)])
            .await
            .unwrap();

        assert_eq!(
            points(&found),
            vec![(tx, block.slot(), *block.hash(), block.number())]
        );
    }
}
