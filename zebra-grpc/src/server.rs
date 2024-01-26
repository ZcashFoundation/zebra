//! The gRPC server implementation

use futures_util::future::TryFutureExt;
use tonic::{transport::Server, Response, Status};
use tower::ServiceExt;

use scanner::scanner_server::{Scanner, ScannerServer};
use scanner::{Empty, InfoReply};

use zebra_node_services::scan_service::{
    request::Request as ScanServiceRequest, response::Response as ScanServiceResponse,
};

/// The generated scanner proto
pub mod scanner {
    tonic::include_proto!("scanner");
}

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug)]
/// The server implementation
pub struct ScannerRPC<ScanService>
where
    ScanService: tower::Service<ScanServiceRequest, Response = ScanServiceResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ScanService as tower::Service<ScanServiceRequest>>::Future: Send,
{
    scan_service: ScanService,
}

#[tonic::async_trait]
impl<ScanService> Scanner for ScannerRPC<ScanService>
where
    ScanService: tower::Service<ScanServiceRequest, Response = ScanServiceResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ScanService as tower::Service<ScanServiceRequest>>::Future: Send,
{
    async fn get_info(
        &self,
        _request: tonic::Request<Empty>,
    ) -> Result<Response<InfoReply>, Status> {
        let ScanServiceResponse::Info {
            min_sapling_birthday_height,
        } = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::Info))
            .await
            .map_err(|_| Status::unknown("scan service was unavailable"))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        let reply = scanner::InfoReply {
            min_sapling_birthday_height: min_sapling_birthday_height.0,
        };

        Ok(Response::new(reply))
    }
}

/// Initializes the zebra-scan gRPC server
pub async fn init<ScanService>(scan_service: ScanService) -> Result<(), color_eyre::Report>
where
    ScanService: tower::Service<ScanServiceRequest, Response = ScanServiceResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ScanService as tower::Service<ScanServiceRequest>>::Future: Send,
{
    let addr = "[::1]:50051".parse()?;
    let service = ScannerRPC { scan_service };

    Server::builder()
        .add_service(ScannerServer::new(service))
        .serve(addr)
        .await?;

    Ok(())
}
