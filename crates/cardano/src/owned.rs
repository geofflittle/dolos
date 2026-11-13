use dolos_core::{
    BlockBody, BlockHash, BlockSlot, EraCbor, RawBlock, RawUtxoMap, TxRange, TxoRef,
};
use pallas::codec::minicbor;
use pallas::ledger::traverse::{Era, MultiEraBlock, MultiEraOutput};
use self_cell::self_cell;
use std::sync::Arc;

self_cell!(
    pub struct OwnedMultiEraBlock {
        owner: Arc<BlockBody>,

        #[covariant]
        dependent: MultiEraBlock,
    }
);

impl OwnedMultiEraBlock {
    pub fn decode(buf: Arc<BlockBody>) -> Result<Self, pallas::ledger::traverse::Error> {
        Self::try_new(buf, |x| MultiEraBlock::decode(x))
    }

    pub fn view(&self) -> &MultiEraBlock<'_> {
        self.borrow_dependent()
    }

    /// The bytes each top level transaction spans in a Dijkstra block, in
    /// block order.
    pub fn tx_ranges(&self) -> Option<Vec<TxRange>> {
        if self.view().era() != Era::Dijkstra {
            return None;
        }
        dijkstra_tx_ranges(self.borrow_owner()).ok()
    }
}

/// The bytes each element of the transaction array of the Dijkstra block
/// `block` spans.
fn dijkstra_tx_ranges(block: &[u8]) -> Result<Vec<TxRange>, minicbor::decode::Error> {
    let offset = |position: usize| {
        u32::try_from(position).map_err(|_| minicbor::decode::Error::message("offset over u32"))
    };

    // `[era, [header, [transactions, leios certificate, peras certificate]]]`
    let mut d = minicbor::Decoder::new(block);
    d.array()?;
    d.skip()?;
    d.array()?;
    d.skip()?;
    d.array()?;
    let count = d.array()?;

    let mut ranges = Vec::new();
    loop {
        let more = match count {
            Some(n) => (ranges.len() as u64) < n,
            None => d.datatype()? != minicbor::data::Type::Break,
        };
        if !more {
            return Ok(ranges);
        }
        let start = offset(d.position())?;
        d.skip()?;
        ranges.push(start..offset(d.position())?);
    }
}

impl dolos_core::Block for OwnedMultiEraBlock {
    fn depends_on(&self, loaded: &mut RawUtxoMap) -> Vec<TxoRef> {
        crate::utxoset::compute_block_dependencies(self.view(), loaded)
    }

    fn slot(&self) -> BlockSlot {
        self.view().slot()
    }

    fn hash(&self) -> BlockHash {
        self.view().hash()
    }

    fn raw(&self) -> RawBlock {
        self.borrow_owner().clone()
    }
}

self_cell!(
    pub struct OwnedMultiEraOutput {
        owner: Arc<EraCbor>,

        #[not_covariant]
        dependent: MultiEraOutput,
    }
);

impl OwnedMultiEraOutput {
    pub fn decode(buf: Arc<EraCbor>) -> Result<Self, pallas::ledger::traverse::Error> {
        Self::try_new(buf, |x| {
            let EraCbor(era, cbor) = x.as_ref();

            let era = pallas::ledger::traverse::Era::try_from(*era)?;

            let dec = MultiEraOutput::decode(era, cbor)
                .map_err(|x| pallas::ledger::traverse::Error::InvalidCbor(x.to_string()))?;

            Ok(dec)
        })
    }
}

#[cfg(test)]
mod tests {
    use pallas::ledger::traverse::MultiEraTx;

    use super::*;

    fn test_block(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test_data")
            .join(name);
        hex::decode(std::fs::read_to_string(path).unwrap().trim()).unwrap()
    }

    /// The block with its transaction array in the other CBOR length form,
    /// definite for indefinite and indefinite for definite.
    fn other_array_form(block: &[u8]) -> Vec<u8> {
        let mut d = minicbor::Decoder::new(block);
        d.array().unwrap();
        d.skip().unwrap();
        d.array().unwrap();
        d.skip().unwrap();
        d.array().unwrap();
        let head = d.position();
        let count = d.array().unwrap();
        let first = d.position();
        let mut n = 0;
        while count.map_or_else(
            || d.datatype().unwrap() != minicbor::data::Type::Break,
            |count| n < count,
        ) {
            d.skip().unwrap();
            n += 1;
        }
        let last = d.position();

        let mut header = minicbor::Encoder::new(Vec::new());
        let (open, close, rest) = match count {
            Some(_) => (vec![0x9f], vec![0xff], last),
            None => (header.array(n).unwrap().writer().clone(), vec![], last + 1),
        };
        [&block[..head], &open, &block[first..last], &close, &block[rest..]].concat()
    }

    /// The hashes of the transactions `ranges` span in `block`.
    fn hashes_spanned(block: &[u8], ranges: &[TxRange]) -> Vec<Vec<u8>> {
        ranges
            .iter()
            .map(|range| {
                let bytes = &block[range.start as usize..range.end as usize];
                MultiEraTx::decode_for_era(Era::Dijkstra, bytes)
                    .unwrap()
                    .hash()
                    .to_vec()
            })
            .collect()
    }

    #[test]
    fn a_block_before_dijkstra_has_no_tx_ranges() {
        let (_, empty) = dolos_testing::blocks::make_conway_block(100);
        let blocks = [
            Arc::new(test_block("byron1.block")),
            Arc::new(test_block("conway.block")),
            empty,
        ];

        for raw in blocks {
            let block = OwnedMultiEraBlock::decode(raw).unwrap();

            assert_ne!(block.view().era(), Era::Dijkstra);
            assert_eq!(block.tx_ranges(), None, "{:?}", block.view().era());
        }
    }

    #[test]
    fn tx_ranges_span_each_transaction_in_block_order() {
        let stored = test_block("musashi-w36/ranking-sub-transaction.block");
        let other = other_array_form(&stored);
        assert_ne!(stored, other);

        for cbor in [stored, other] {
            let block = OwnedMultiEraBlock::decode(Arc::new(cbor.clone())).unwrap();
            let expected: Vec<_> = block.view().txs().iter().map(|x| x.hash().to_vec()).collect();
            let ranges = block.tx_ranges().unwrap();

            assert!(expected.len() > 1);
            assert_eq!(hashes_spanned(&cbor, &ranges), expected);
        }
    }
}
