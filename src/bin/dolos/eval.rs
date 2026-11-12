use dolos::{
    adapters::DomainAdapter,
    core::{ChainPoint, Domain, MempoolAwareUtxoStore},
};
use dolos_core::config::RootConfig;
use miette::{Context, IntoDiagnostic};
use std::path::PathBuf;

#[derive(Debug, clap::Args)]
pub struct Args {
    #[arg(long, short)]
    file: PathBuf,

    #[arg(long, short)]
    era: u16,

    #[arg(long, short)]
    epoch: u64,

    #[arg(long, short)]
    block_slot: u64,

    #[arg(long, short)]
    network_id: u8,
}

#[tokio::main]
pub async fn run(config: &RootConfig, args: &Args) -> miette::Result<()> {
    crate::common::setup_tracing(&config.logging, &config.telemetry)?;

    let domain = crate::common::setup_domain(config)?;

    let cbor = std::fs::read_to_string(&args.file)
        .into_diagnostic()
        .context("reading tx from file")?;

    let cbor = hex::decode(cbor)
        .into_diagnostic()
        .context("decoding hex content from file")?;

    let genesis = domain.genesis();
    let epoch = dolos_cardano::load_epoch::<DomainAdapter>(domain.state()).into_diagnostic()?;
    let era = dolos_cardano::validate::submission_era(
        epoch
            .pparams
            .unwrap_live()
            .ensure_protocol_version()
            .into_diagnostic()?,
    )
    .into_diagnostic()?;
    miette::ensure!(
        u16::from(era) == args.era,
        "requested era differs from active chain era"
    );
    miette::ensure!(
        epoch.number == args.epoch,
        "requested epoch differs from active chain state"
    );
    let network = match genesis.shelley.network_id.as_deref() {
        Some("Mainnet") => 1,
        Some("Testnet") => 0,
        _ => return Err(miette::miette!("genesis network id is missing")),
    };
    miette::ensure!(
        network == args.network_id,
        "requested network differs from chain genesis"
    );
    let utxos = MempoolAwareUtxoStore::<DomainAdapter>::new(domain.state(), domain.mempool());
    dolos_cardano::validate::validate_tx(
        &cbor,
        &utxos,
        Some(ChainPoint::Slot(args.block_slot)),
        &genesis,
        &Default::default(),
    )
    .into_diagnostic()
    .context("validating and evaluating transaction")?;
    Ok(())
}
