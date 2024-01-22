//! The gRPC server implementation

use tonic::{transport::Server, Response, Status};

use scanner::scanner_server::{Scanner, ScannerServer};
use scanner::{Empty, InfoReply};

use zebra_scan::{storage::Storage, Config};

/// The generated scanner proto
pub mod scanner {
    tonic::include_proto!("scanner");
}

#[derive(Debug, Default)]
/// The server implementation
pub struct ScannerRPC {}

#[tonic::async_trait]
impl Scanner for ScannerRPC {
    async fn get_info(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<Response<InfoReply>, Status> {
        let network = zcash_primitives::consensus::Network::MainNetwork;
        let storage = Storage::new(&Config::default(), network.into(), true);

        let reply = scanner::InfoReply {
            min_sapling_birthday_height: storage.min_sapling_birthday_height().0,
        };

        Ok(Response::new(reply))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = "[::1]:50051".parse()?;
    let service = ScannerRPC::default();

    Server::builder()
        .add_service(ScannerServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
