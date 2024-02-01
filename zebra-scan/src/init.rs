//! Initializing the scanner and RPC server.

use color_eyre::Report;
use tower::ServiceBuilder;

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
    let scan_service = ServiceBuilder::new().buffer(10).service(ScanService::new(
        &config,
        network,
        state,
        chain_tip_change,
    ));

    // Start the gRPC server.
    zebra_grpc::server::init(scan_service).await?;

    Ok(())
}
