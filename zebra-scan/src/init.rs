//! Initializing the scanner and gRPC server.

use std::{net::SocketAddr, time::Duration};

use color_eyre::Report;
use tokio::task::JoinHandle;
use tower::ServiceBuilder;

use tracing::Instrument;
use zebra_chain::{diagnostic::task::WaitForPanics, parameters::Network};
use zebra_state::ChainTipChange;

use crate::{scan, service::ScanService, storage::Storage, Config};

/// The timeout applied to scan service calls
pub const SCAN_SERVICE_TIMEOUT: Duration = Duration::from_secs(30);

/// Initialize [`ScanService`] based on its config.
///
/// TODO: add a test for this function.
pub async fn init_with_server(
    listen_addr: SocketAddr,
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> Result<(), Report> {
    info!(?config, "starting scan service");
    let scan_service = ServiceBuilder::new()
        .buffer(10)
        .timeout(SCAN_SERVICE_TIMEOUT)
        .service(ScanService::new(&config, &network, state, chain_tip_change).await);

    // TODO: move this to zebra-grpc init() function and include addr
    info!(?listen_addr, "starting scan gRPC server");

    // Start the gRPC server.
    zebra_grpc::server::init(listen_addr, scan_service).await?;

    Ok(())
}

/// Initialize the scanner and its gRPC server based on its config, and spawn a task for it.
pub fn spawn_init(
    config: Config,
    network: Network,
    state: scan::State,
    chain_tip_change: ChainTipChange,
) -> JoinHandle<Result<(), Report>> {
    if let Some(listen_addr) = config.listen_addr {
        // TODO: spawn an entirely new executor here, to avoid timing attacks.
        tokio::spawn(
            init_with_server(listen_addr, config, network, state, chain_tip_change)
                .in_current_span(),
        )
    } else {
        // TODO: spawn an entirely new executor here, to avoid timing attacks.
        tokio::spawn(
            async move {
                let storage =
                    tokio::task::spawn_blocking(move || Storage::new(&config, &network, false))
                        .wait_for_panics()
                        .await;
                let (_cmd_sender, cmd_receiver) = tokio::sync::mpsc::channel(1);
                scan::start(state, chain_tip_change, storage, cmd_receiver).await
            }
            .in_current_span(),
        )
    }
}
