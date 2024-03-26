//! Runs an RPC server with a mock ScanTask

use tower::ServiceBuilder;

use zebra_scan::{service::ScanService, storage::Storage};

#[tokio::main]
/// Runs an RPC server with a mock ScanTask
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (config, network) = Default::default();

    let (scan_service, _cmd_receiver) =
        ScanService::new_with_mock_scanner(Storage::new(&config, &network, false));
    let scan_service = ServiceBuilder::new().buffer(10).service(scan_service);

    // Start the gRPC server.
    zebra_grpc::server::init("127.0.0.1:8231".parse()?, scan_service).await?;

    Ok(())
}
