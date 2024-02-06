//! Initializing the scanner and gRPC server.

use color_eyre::Report;
use tokio::task::JoinHandle;
use tower::ServiceBuilder;

use tracing::Instrument;
use zebra_chain::parameters::Network;
use zebra_state::ChainTipChange;

use crate::{scan, service::ScanService, Config};

/// Initialize [`ScanService`] based on its config.
///
/// TODO: add a test for this function.
pub async fn init(
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> Result<(), Report> {
    info!(?config, "starting scan service");
    let scan_service = ServiceBuilder::new().buffer(10).service(ScanService::new(
        &config,
        network,
        state,
        chain_tip_change,
    ));

    // TODO: move this to zebra-grpc init() function and include addr
    info!("starting scan gRPC server");

    // Start the gRPC server.
    zebra_grpc::server::init(scan_service).await?;

    Ok(())
}

/// Initialize the scanner and its gRPC server based on its config, and spawn a task for it.
pub fn spawn_init(
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> JoinHandle<Result<(), Report>> {
    // TODO: spawn an entirely new executor here, to avoid timing attacks.
    tokio::spawn(init(config, network, state, chain_tip_change).in_current_span())
}
