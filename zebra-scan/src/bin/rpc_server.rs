//! Runs an RPC server with a mock ScanTask

use tower::ServiceBuilder;

use zebra_scan::service::ScanService;

#[tokio::main]
/// Runs an RPC server with a mock ScanTask
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (config, network) = Default::default();
    let scan_service = ServiceBuilder::new()
        .buffer(10)
        .service(ScanService::new_with_mock_scanner(&config, network));

    // Start the gRPC server.
    zebra_grpc::server::init(scan_service).await?;

    Ok(())
}
