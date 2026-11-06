use std::sync::Arc;

use dolos_core::{ChainLogic as _, Domain as _};
use dolos_testing::toy_domain::ToyDomain;

use crate::adapters::{
    ArchiveStoreBackend, DomainAdapter, MempoolBackend, StateStoreBackend, WalAdapter,
};

pub(crate) mod musashi;
mod upstream;

/// A serving domain over the stores and genesis of a toy domain, with an empty mempool.
pub(crate) fn toy_adapter(toy: &ToyDomain) -> DomainAdapter {
    let state = StateStoreBackend::Memory(toy.state().clone());
    let genesis = toy.genesis();
    let chain = dolos_cardano::CardanoLogic::initialize::<DomainAdapter>(
        Default::default(),
        &state,
        &genesis,
    )
    .unwrap();
    DomainAdapter {
        storage_config: Arc::new(toy.storage_config().clone()),
        sync_config: Arc::new(toy.sync_config().clone()),
        genesis,
        chain: Arc::new(std::sync::RwLock::new(chain)),
        state,
        archive: ArchiveStoreBackend::Memory(toy.archive().clone()),
        wal: WalAdapter::Redb(toy.wal().clone()),
        mempool: MempoolBackend::Ephemeral(Default::default()),
        tip_broadcast: tokio::sync::broadcast::channel(8).0,
    }
}
