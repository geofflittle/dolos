//! Runs the pull stage under the gasket runtime against a live node, with the
//! Leios connection sent through a local proxy that holds its first
//! connections open and silent, so each of those endorser block fetches times
//! out.
//!
//! One test per process, since the log capture is a global subscriber:
//! `EB_NODE=172.17.0.2:3001 cargo test --lib sync::pull::endorser_retry::two_failures_carries_s -- --exact --ignored --nocapture`

use std::collections::HashMap;
use std::io::Write;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use dolos_core::{LogValue, WalStore};
use gasket::messaging::InputPort;

use super::*;
use crate::adapters::storage::WalStoreBackend;

const MAGIC: u64 = 164;

/// The last block stored before S, whose announcement S certifies.
const PARENT_SLOT: u64 = 1_393_034;
const PARENT_HASH: &str = "1c0917d43c68ba0dfb36643e5c51e16ab6f3e0d894d0571722184dd30a545d66";

/// The certifying block whose endorser block fetch is made to fail.
const S: u64 = 1_393_060;

/// The next certifying block after S.
const NEXT_CERT: u64 = 1_393_102;

fn node() -> String {
    std::env::var("EB_NODE").unwrap_or_else(|_| "172.17.0.2:3001".into())
}

/// A proxy to the node that accepts its first `hang` connections and never
/// answers them, and forwards every later one both ways.
fn proxy(hang: usize) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();

    std::thread::spawn(move || {
        let mut held = Vec::new();

        for inbound in listener.incoming() {
            let Ok(inbound) = inbound else { continue };
            let n = counter.fetch_add(1, Ordering::SeqCst);

            if n < hang {
                eprintln!("PROXY connection {n} held silent");
                held.push(inbound);
                continue;
            }

            eprintln!("PROXY connection {n} forwarded");

            let outbound = TcpStream::connect(node()).unwrap();
            let (mut in_r, mut in_w) = (inbound.try_clone().unwrap(), inbound);
            let (mut out_r, mut out_w) = (outbound.try_clone().unwrap(), outbound);

            std::thread::spawn(move || {
                let _ = std::io::copy(&mut in_r, &mut out_w);
                let _ = out_w.shutdown(Shutdown::Write);
            });
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut out_r, &mut in_w);
                let _ = in_w.shutdown(Shutdown::Write);
            });
        }
    });

    (addr, accepted)
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stderr().write_all(buf)?;
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

fn parent_point() -> ChainPoint {
    ChainPoint::Specific(PARENT_SLOT, PARENT_HASH.parse().unwrap())
}

/// A write ahead log holding the parent block, fetched from the node.
fn seeded_wal(rt: &tokio::runtime::Runtime) -> WalAdapter {
    let raw = rt.block_on(async {
        let mut peer = PeerClient::connect(&node(), MAGIC).await.unwrap();
        let point = Point::try_from(parent_point()).unwrap();
        let block = peer.blockfetch().fetch_single(point).await.unwrap();
        peer.abort().await;
        block
    });

    let wal = WalStoreBackend::in_memory().unwrap();

    wal.append_entries(vec![(
        parent_point(),
        LogValue {
            block: raw,
            delta: vec![],
            inputs: HashMap::new(),
        },
    )])
    .unwrap();

    wal
}

struct Outcome {
    rolled_forward: Vec<(u64, usize)>,
    stage_ended: bool,
    log: String,
    proxy_connections: usize,
}

/// Runs the stage from the parent block until it sends the next certifying
/// block downstream, ends, or sends nothing for 400 seconds.
fn run(hang: usize) -> Outcome {
    let capture = Capture::default();
    let writer = capture.clone();

    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .init();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let wal = seeded_wal(&rt);
    let (leios_addr, accepted) = proxy(hang);

    let mut stage = Stage {
        peer_address: node(),
        leios_peer_address: Some(leios_addr),
        network_magic: MAGIC,
        block_fetch_batch_size: 1,
        wal,
        quota: PullQuota::Unlimited,
        downstream: Default::default(),
        block_count: Default::default(),
        endorser_block_count: Default::default(),
        endorser_tx_count: Default::default(),
        chain_tip: Default::default(),
    };

    let mut input = InputPort::<PullEvent>::default();
    gasket::messaging::tokio::connect_ports(&mut stage.downstream, &mut input, 1000);

    // The default retry count and dismissal, with a constant one second backoff.
    let retries = gasket::retries::Policy {
        max_retries: 20,
        backoff_unit: Duration::from_secs(1),
        backoff_factor: 1,
        max_backoff: Duration::from_secs(1),
        dismissible: false,
    };

    let policy = gasket::runtime::Policy {
        tick_timeout: None,
        bootstrap_retry: retries.clone(),
        work_retry: retries.clone(),
        teardown_retry: retries,
    };

    let _tether = gasket::runtime::spawn_stage(stage, policy);

    let mut rolled_forward = Vec::new();
    let mut stage_ended = false;

    rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(400), input.recv()).await {
                Err(_) => break,
                Ok(Err(_)) => {
                    stage_ended = true;
                    break;
                }
                Ok(Ok(msg)) => match msg.payload {
                    PullEvent::RollForward(raw) => {
                        let block = MultiEraBlock::decode(&raw).unwrap();
                        let slot = block.slot();
                        eprintln!(
                            "DOWNSTREAM roll forward slot {slot} txs {}",
                            block.tx_count()
                        );
                        rolled_forward.push((slot, block.tx_count()));

                        if slot >= NEXT_CERT {
                            break;
                        }
                    }
                    PullEvent::Rollback(point) => {
                        eprintln!("DOWNSTREAM rollback to {}", point.slot());
                    }
                },
            }
        }
    });

    Outcome {
        rolled_forward,
        stage_ended,
        log: capture.text(),
        proxy_connections: accepted.load(Ordering::SeqCst),
    }
}

/// Asserts that every silent connection was used, S went downstream with its
/// endorsed transactions, the stage reached the next certifying block, and
/// nothing was left undelivered.
fn assert_carries_s(name: &str, hang: usize) {
    let out = run(hang);

    eprintln!(
        "VERDICT {name}: forwarded={:?} stage_ended={} proxy_connections={}",
        out.rolled_forward, out.stage_ended, out.proxy_connections
    );

    let slots: Vec<u64> = out.rolled_forward.iter().map(|(s, _)| *s).collect();
    let at_s = out.rolled_forward.iter().find(|(s, _)| *s == S);

    assert_eq!(
        out.proxy_connections,
        hang + 1,
        "every silent connection was used"
    );
    assert!(
        matches!(at_s, Some((_, txs)) if *txs > 0),
        "S missing or empty: {slots:?}"
    );
    assert!(
        slots.contains(&NEXT_CERT),
        "did not reach the next certifying block: {slots:?}"
    );
    assert!(!out.stage_ended, "the stage stopped");
    assert!(
        !out.log
            .contains("was fetched and no block of that slot arrived"),
        "an endorser block was left undelivered"
    );
}

/// S is delivered when its fetch fails once, before its batch is drained.
#[test]
#[ignore]
fn one_failure_carries_s() {
    assert_carries_s("one_failure_carries_s", 1);
}

/// S is delivered when its fetch fails twice, the second time once its header
/// batch is read.
#[test]
#[ignore]
fn two_failures_carries_s() {
    assert_carries_s("two_failures_carries_s", 2);
}

/// S is delivered when its fetch fails three times.
#[test]
#[ignore]
fn three_failures_carries_s() {
    assert_carries_s("three_failures_carries_s", 3);
}
