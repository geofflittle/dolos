//! record-relay command implementation.

use anyhow::{bail, Context, Result};
use clap::Args;
use dolos_testing::relay::{RecordedBlock, Recording};
use pallas::ledger::traverse::MultiEraHeader;
use pallas::network::facades::PeerClient;
use pallas::network::miniprotocols::chainsync::NextResponse;
use pallas::network::miniprotocols::Point;

/// Arguments for the record-relay command.
#[derive(Debug, Args)]
pub struct RecordRelayArgs {
    /// Relay to record, as host:port
    #[arg(long)]
    pub address: String,

    /// Network magic of the relay
    #[arg(long)]
    pub magic: u64,

    /// Number of blocks to record from the origin
    #[arg(long, default_value_t = 100)]
    pub blocks: usize,

    /// File the recording is written to
    #[arg(long)]
    pub out: std::path::PathBuf,
}

pub fn run(args: &RecordRelayArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let recording = runtime.block_on(record(args))?;
    let text = serde_json::to_string_pretty(&recording)?;

    std::fs::write(&args.out, text + "\n")
        .with_context(|| format!("writing {}", args.out.display()))?;

    println!(
        "recorded {} blocks from {} into {}",
        recording.blocks.len(),
        args.address,
        args.out.display()
    );

    Ok(())
}

async fn record(args: &RecordRelayArgs) -> Result<Recording> {
    let mut peer = PeerClient::connect(&args.address, args.magic)
        .await
        .context("connecting to the relay")?;

    peer.chainsync()
        .find_intersect(vec![Point::Origin])
        .await
        .context("intersecting at the origin")?;

    let mut headers = Vec::new();

    while headers.len() < args.blocks {
        match peer.chainsync().request_next().await? {
            NextResponse::RollForward(content, _) => headers.push(content),
            NextResponse::RollBackward(Point::Origin, _) => {}
            NextResponse::RollBackward(point, _) => bail!("relay rolled back to {point:?}"),
            NextResponse::Await => bail!("relay reached its tip at {} blocks", headers.len()),
        }
    }

    let mut blocks = Vec::new();

    for content in headers {
        let header = MultiEraHeader::decode(
            content.variant,
            content.byron_prefix.map(|(subtag, _)| subtag),
            &content.cbor,
        )?;

        let point = Point::Specific(header.slot(), header.hash().to_vec());

        let body = peer
            .blockfetch()
            .fetch_single(point)
            .await
            .context("fetching a block")?;

        blocks.push(RecordedBlock {
            slot: header.slot(),
            hash: header.hash().to_string(),
            number: header.number(),
            variant: content.variant,
            byron_prefix: content.byron_prefix,
            header: hex::encode(&content.cbor),
            body: hex::encode(body),
        });
    }

    peer.abort().await;

    Ok(Recording {
        magic: args.magic,
        blocks,
    })
}
