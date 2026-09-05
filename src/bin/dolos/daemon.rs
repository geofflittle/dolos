use dolos_core::config::RootConfig;
use futures_util::stream::FuturesUnordered;
use miette::{Context, IntoDiagnostic};
use tracing::warn;

#[derive(Debug, clap::Args)]
pub struct Args {}

/// Refuses to relay a chain whose blocks have been resolved against their
/// endorser blocks.
///
/// Following a Leios chain means storing each certifying ranking block with the
/// certified endorser block's transactions inlined, which is the shape a node
/// serves its local clients. The node to node wire shape is the unresolved
/// block, so relaying the stored body next to the original header would hand a
/// peer a body its header does not commit to. Serving that is worse than not
/// serving, so it is refused at startup rather than discovered by a peer.
fn refuse_relaying_a_resolved_chain(config: &RootConfig) -> miette::Result<()> {
    let follows_leios = config
        .upstream
        .leios_peer_address()
        .is_some();

    if follows_leios && config.relay.is_some() {
        miette::bail!(
            "this node follows a Leios chain, so its stored blocks carry endorser block \
             transactions the node to node wire form does not. Remove the relay section, or \
             remove leios_peer_address and accept that the ledger will be short."
        );
    }

    Ok(())
}

#[tokio::main]
pub async fn run(config: RootConfig, _args: &Args) -> miette::Result<()> {
    crate::common::setup_tracing(&config.logging, &config.telemetry)?;

    let domain = crate::common::setup_domain(&config)?;

    refuse_relaying_a_resolved_chain(&config)?;

    let exit = crate::common::hook_exit_token();

    let sync = dolos::sync::pipeline(
        &config.sync,
        &config.chain,
        &config.upstream,
        domain.clone(),
        &config.retries,
    )
    .into_diagnostic()
    .context("bootstrapping sync pipeline")?;

    let sync = tokio::spawn(crate::common::run_pipeline(
        gasket::daemon::Daemon::new(sync),
        exit.clone(),
    ));

    let drivers = FuturesUnordered::new();
    let network_magic = config.chain.magic();

    dolos::serve::load_drivers(
        &drivers,
        config.serve,
        network_magic,
        domain.clone(),
        exit.clone(),
    );
    dolos::relay::load_drivers(
        &drivers,
        config.relay,
        network_magic,
        domain.clone(),
        exit.clone(),
    );

    let outcome = crate::common::monitor_drivers(drivers, exit.clone()).await;

    sync.await.unwrap();

    warn!("shutdown complete");

    outcome.into_diagnostic().context("serving clients")
}
