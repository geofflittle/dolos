use pallas::interop::utxorpc::{LedgerContext, TxoRef, UtxoMap};

/// The chain reads a served response takes, which map a transaction only
/// through a block context.
pub trait ChainReads: Clone {
    /// The outputs the chain holds for the given references.
    fn utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap>;
    /// The wall clock time of a slot, in UNIX seconds.
    fn slot_timestamp(&self, slot: u64) -> Option<u64>;
}

impl<T: LedgerContext> ChainReads for T {
    fn utxos(&self, refs: &[TxoRef]) -> Option<UtxoMap> {
        self.get_utxos(refs)
    }
    fn slot_timestamp(&self, slot: u64) -> Option<u64> {
        self.get_slot_timestamp(slot)
    }
}
