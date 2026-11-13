//! Paired test with sundae-sync-v2's real follower and filters. All stores and
//! the RPC listener belong to this test; no running node or AWS service is
//! used.
use std::{sync::Arc, time::Duration};

use dolos_core::{
    ArchiveIndexDelta, ArchiveStore, ArchiveWriter, ChainPoint, Domain, EraCbor, StateWriter,
    TipEvent, UtxoSetDelta, WalStore,
};
use dolos_testing::toy_domain::ToyDomain;
use pallas::ledger::traverse::MultiEraBlock;

use crate::serve::CancelTokenImpl;

#[tokio::test]
#[ignore = "requires SUNDAE_SYNC_SUBTX_TEST_BIN; see test_data/utxorpc-subtransactions/README.md"]
async fn sundae_sync_consumes_child_inputs_over_v1() {
    use super::{spec, sync::SyncServiceImpl};
    use dolos_core::StateStore;

    let executable = std::env::var("SUNDAE_SYNC_SUBTX_TEST_BIN")
        .expect("build sundae-sync-v2's dolos_subtransactions test and set its executable path");
    let (raw, outputs) = crate::tests::musashi::child_only_inputs(true);
    let seed = UtxoSetDelta {
        produced_utxo: outputs
            .into_iter()
            .map(|(r, (era, cbor))| (r.into(), Arc::new(EraCbor(era.into(), cbor))))
            .collect(),
        ..Default::default()
    };
    let toy = ToyDomain::new(Some(seed), None);
    let domain = crate::tests::toy_adapter(&toy);
    domain.wal().reset_to(&ChainPoint::Origin).unwrap();
    let block = MultiEraBlock::decode(&raw).unwrap();
    let point = ChainPoint::Specific(block.slot(), block.hash());
    let writer = domain.archive().start_writer().unwrap();
    writer.apply(&point, &raw).unwrap();
    writer
        .apply_index(&[ArchiveIndexDelta {
            slot: block.slot(),
            block_hash: block.hash().to_vec(),
            block_number: Some(block.number()),
            txs: dolos_core::applied_txs(&block)
                .iter()
                .map(|(_, tx)| dolos_core::IndexedTx::unplaced(tx.hash().to_vec()))
                .collect(),
            tags: vec![],
        }])
        .unwrap();
    writer.commit().unwrap();
    let writer = domain.state().start_writer().unwrap();
    writer.set_cursor(point.clone()).unwrap();
    writer.commit().unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let incoming = futures_util::stream::unfold(listener, |listener| async {
        Some((listener.accept().await.map(|(socket, _)| socket), listener))
    });
    let service = spec::sync::sync_service_server::SyncServiceServer::new(SyncServiceImpl::new(
        domain.clone(),
        CancelTokenImpl::default(),
    ));
    let server = tokio::spawn(
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_incoming(incoming),
    );
    // Abort background tasks even if a test assertion fails.
    let server = tokio_util::task::AbortOnDropHandle::new(server);
    let events = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        while domain.tip_broadcast.receiver_count() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        domain.notify_tip(TipEvent::Apply(point.clone(), raw.clone()));
        domain.notify_tip(TipEvent::Undo(point, raw));
    }));
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(executable)
            .args([
                "dolos_child_inputs_survive_fetch_follow_and_undo",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("DOLOS_SUBTX_TEST_URI", uri)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("sundae-sync consumer timed out")
    .unwrap();
    eprintln!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "sundae-sync consumer failed: {}",
        output.status
    );
    events.await.unwrap();
    drop(server);
}
