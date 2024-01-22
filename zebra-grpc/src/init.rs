//! Initializing the scanner gRPC service.

use color_eyre::Report;
use tokio::task::JoinHandle;
use tonic::transport::Server;
use tracing::Instrument;

use crate::{
    methods::ZebraScanRpcImpl, zebra_scan_service::zebra_scan_rpc_server::ZebraScanRpcServer,
};

/// Initialize the scanner gRPC service, and spawn a task for it.
async fn _spawn_init() -> JoinHandle<Result<(), Report>> {
    tokio::spawn(_init().in_current_span())
}

/// Initialize the scanner gRPC service.
async fn _init() -> Result<(), Report> {
    Server::builder()
        .add_service(ZebraScanRpcServer::new(ZebraScanRpcImpl::default()))
        .serve("127.0.0.1:3031".parse()?)
        .await?;

    Ok(())
}
