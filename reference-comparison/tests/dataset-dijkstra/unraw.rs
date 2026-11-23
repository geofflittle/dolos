//! The Dijkstra values that keep bytes as read, written without them.

use pallas_primitives::dijkstra;

use crate::dataset::plain::Unraw;

holds_no_raw!(
    dijkstra::Header,
    dijkstra::AuxiliaryData,
    dijkstra::NativeScript,
    dijkstra::Redeemers
);

impl<T: Unraw + Clone> Unraw for dijkstra::NonEmptySet<T> {
    fn unraw(&mut self) {
        let arm = self.arm();
        let mut items = self.clone().into_vec();
        items.unraw();
        if let Ok(set) = dijkstra::NonEmptySet::try_from(items) {
            *self = set.with_arm(arm);
        }
    }
}

impl Unraw for dijkstra::Block<'_> {
    fn unraw(&mut self) {
        self.header.unraw();
        self.block_body.unraw();
    }
}

impl Unraw for dijkstra::BlockBody<'_> {
    fn unraw(&mut self) {
        self.transactions.unraw();
    }
}

impl Unraw for dijkstra::BlockTransaction<'_> {
    fn unraw(&mut self) {
        self.transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::MempoolTransaction<'_> {
    fn unraw(&mut self) {
        self.transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::TransactionBody<'_> {
    fn unraw(&mut self) {
        self.outputs.unraw();
        self.collateral_return.unraw();
        self.sub_transactions.unraw();
    }
}

impl Unraw for dijkstra::SubTransactionBody<'_> {
    fn unraw(&mut self) {
        self.outputs.unraw();
    }
}

impl Unraw for dijkstra::SubTransaction<'_> {
    fn unraw(&mut self) {
        self.sub_transaction_body.unraw();
        self.transaction_witness_set.unraw();
        self.auxiliary_data.unraw();
    }
}

impl Unraw for dijkstra::WitnessSet<'_> {
    fn unraw(&mut self) {
        self.native_script.unraw();
        self.plutus_data.unraw();
        self.redeemer.unraw();
    }
}

impl Unraw for dijkstra::ScriptRef<'_> {
    fn unraw(&mut self) {
        if let dijkstra::ScriptRef::NativeScript(script) = self {
            script.unraw();
        }
    }
}
