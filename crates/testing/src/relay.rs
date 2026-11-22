//! A loopback node to node peer that serves a recorded chain over chainsync and
//! blockfetch.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

use pallas::network::facades::PeerServer;
use pallas::network::miniprotocols::blockfetch::{self, BlockRequest};
use pallas::network::miniprotocols::chainsync::{ClientRequest, HeaderContent, N2NServer, Tip};
use pallas::network::miniprotocols::keepalive;
use pallas::network::miniprotocols::Point;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

/// The first blocks of a chain as a relay served them, from the origin onwards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recording {
    pub magic: u64,
    pub blocks: Vec<RecordedBlock>,
}

/// One block of a recording, with its chainsync header and its blockfetch body
/// as hex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedBlock {
    pub slot: u64,
    pub hash: String,
    pub number: u64,
    pub variant: u8,
    pub byron_prefix: Option<(u8, u64)>,
    pub header: String,
    pub body: String,
}

struct Block {
    point: Point,
    number: u64,
    header: HeaderContent,
    body: Vec<u8>,
}

struct Chain {
    blocks: Vec<Block>,
}

impl Chain {
    fn from_recording(recording: &Recording) -> Self {
        let blocks = recording
            .blocks
            .iter()
            .map(|block| Block {
                point: Point::Specific(
                    block.slot,
                    hex::decode(&block.hash).expect("recorded hash is hex"),
                ),
                number: block.number,
                header: HeaderContent {
                    variant: block.variant,
                    byron_prefix: block.byron_prefix,
                    cbor: hex::decode(&block.header).expect("recorded header is hex"),
                },
                body: hex::decode(&block.body).expect("recorded body is hex"),
            })
            .collect();

        Self { blocks }
    }

    fn tip(&self) -> Tip {
        match self.blocks.last() {
            Some(block) => Tip(block.point.clone(), block.number),
            None => Tip(Point::Origin, 0),
        }
    }

    /// The index of the block after the point, which is zero for the origin.
    fn after(&self, point: &Point) -> Option<usize> {
        match point {
            Point::Origin => Some(0),
            _ => self
                .blocks
                .iter()
                .position(|block| &block.point == point)
                .map(|at| at + 1),
        }
    }
}

/// A relay serving a recording on a loopback port until it is dropped.
pub struct Relay {
    address: SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Relay {
    /// Starts serving the recording on a free loopback port.
    pub fn start(recording: &Recording) -> std::io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let magic = recording.magic;
        let chain = Arc::new(Chain::from_recording(recording));
        let (stop, stopped) = oneshot::channel();

        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("relay runtime starts");

            runtime.block_on(async move {
                let listener =
                    tokio::net::TcpListener::from_std(listener).expect("relay listener registers");

                tokio::select! {
                    _ = stopped => {}
                    _ = accept(listener, magic, chain) => {}
                }
            });

            runtime.shutdown_background();
        });

        Ok(Self {
            address,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    /// Reads the recording for a network magic from a directory of
    /// `<magic>.json` files.
    pub fn recording(dir: &Path, magic: u64) -> Recording {
        let path = dir.join(format!("{magic}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("failed to parse {}: {err}", path.display()))
    }

    /// The loopback address the relay listens on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn accept(listener: tokio::net::TcpListener, magic: u64, chain: Arc<Chain>) {
    loop {
        let Ok(peer) = PeerServer::accept(&listener, magic).await else {
            continue;
        };

        tokio::spawn(serve(peer, chain.clone()));
    }
}

async fn serve(peer: PeerServer, chain: Arc<Chain>) {
    let PeerServer {
        plexer,
        chainsync,
        blockfetch,
        keepalive,
        ..
    } = peer;

    tokio::select! {
        _ = serve_chainsync(chainsync, &chain) => {}
        _ = serve_blockfetch(blockfetch, &chain) => {}
        _ = serve_keepalive(keepalive) => {}
    }

    plexer.abort().await;
}

async fn serve_chainsync(mut server: N2NServer, chain: &Chain) {
    let mut next = 0;
    let mut rollback = None;

    while let Ok(Some(request)) = server.recv_while_idle().await {
        let sent = match request {
            ClientRequest::Intersect(points) => {
                let found = points
                    .iter()
                    .find_map(|point| chain.after(point).map(|at| (point.clone(), at)));

                match found {
                    Some((point, at)) => {
                        next = at;
                        rollback = Some(point.clone());
                        server.send_intersect_found(point, chain.tip()).await
                    }
                    None => server.send_intersect_not_found(chain.tip()).await,
                }
            }
            ClientRequest::RequestNext => {
                if let Some(point) = rollback.take() {
                    server.send_roll_backward(point, chain.tip()).await
                } else if let Some(block) = chain.blocks.get(next) {
                    next += 1;
                    server
                        .send_roll_forward(block.header.clone(), chain.tip())
                        .await
                } else {
                    let _ = server.send_await_reply().await;
                    std::future::pending::<()>().await;
                    return;
                }
            }
        };

        if sent.is_err() {
            return;
        }
    }
}

async fn serve_blockfetch(mut server: blockfetch::Server, chain: &Chain) {
    while let Ok(Some(BlockRequest((first, last)))) = server.recv_while_idle().await {
        let range = match (chain.after(&first), chain.after(&last)) {
            (Some(first), Some(last)) if first >= 1 && first <= last => chain.blocks
                [first - 1..last]
                .iter()
                .map(|block| block.body.clone())
                .collect(),
            _ => Vec::new(),
        };

        if server.send_block_range(range).await.is_err() {
            return;
        }
    }
}

async fn serve_keepalive(mut server: keepalive::Server) {
    while server.keepalive_roundtrip().await.is_ok() {}
}

#[cfg(test)]
mod tests {
    use pallas::network::facades::PeerClient;
    use pallas::network::miniprotocols::chainsync::NextResponse;

    use super::*;

    fn recording() -> Recording {
        let blocks = (0..3u8)
            .map(|i| RecordedBlock {
                slot: 10 * u64::from(i),
                hash: hex::encode([i; 32]),
                number: u64::from(i),
                variant: 6,
                byron_prefix: None,
                header: hex::encode([i]),
                body: hex::encode([0x10 + i]),
            })
            .collect();

        Recording { magic: 42, blocks }
    }

    fn point(block: &RecordedBlock) -> Point {
        Point::Specific(block.slot, hex::decode(&block.hash).expect("hex"))
    }

    #[tokio::test]
    async fn a_client_follows_the_recording_from_the_origin() {
        let recording = recording();
        let relay = Relay::start(&recording).expect("relay starts");
        let mut peer = PeerClient::connect(relay.address(), recording.magic)
            .await
            .expect("client connects");

        let (found, _) = peer
            .chainsync()
            .find_intersect(vec![Point::Origin])
            .await
            .expect("intersect answered");
        assert_eq!(found, Some(Point::Origin));

        let mut responses = Vec::new();

        loop {
            match peer
                .chainsync()
                .request_next()
                .await
                .expect("next answered")
            {
                NextResponse::RollBackward(point, _) => responses.push(format!("back {point:?}")),
                NextResponse::RollForward(content, _) => {
                    responses.push(format!("forward {}", hex::encode(content.cbor)))
                }
                NextResponse::Await => break,
            }
        }

        assert_eq!(
            responses,
            ["back Origin", "forward 00", "forward 01", "forward 02"]
        );

        let body = peer
            .blockfetch()
            .fetch_single(point(&recording.blocks[1]))
            .await
            .expect("block fetched");
        assert_eq!(hex::encode(body), "11");

        peer.abort().await;
    }

    #[tokio::test]
    async fn a_point_off_the_recording_finds_no_intersection_and_no_block() {
        let recording = recording();
        let relay = Relay::start(&recording).expect("relay starts");
        let mut peer = PeerClient::connect(relay.address(), recording.magic)
            .await
            .expect("client connects");
        let off = Point::Specific(5, vec![9; 32]);

        let (found, tip) = peer
            .chainsync()
            .find_intersect(vec![off.clone()])
            .await
            .expect("intersect answered");
        assert_eq!(found, None);
        assert_eq!((tip.0, tip.1), (point(&recording.blocks[2]), 2));

        assert!(peer.blockfetch().fetch_single(off).await.is_err());

        peer.abort().await;
    }
}
