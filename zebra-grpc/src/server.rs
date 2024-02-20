//! The gRPC server implementation

use std::{collections::BTreeMap, net::SocketAddr};

use futures_util::future::TryFutureExt;
use tonic::{transport::Server, Request, Response, Status};
use tower::ServiceExt;

use zebra_node_services::scan_service::{
    request::Request as ScanServiceRequest, response::Response as ScanServiceResponse,
};

use crate::scanner::{
    scanner_server::{Scanner, ScannerServer},
    ClearResultsRequest, DeleteKeysRequest, Empty, GetResultsRequest, GetResultsResponse,
    InfoReply, RegisterKeysRequest, RegisterKeysResponse, Results, TransactionHash,
};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The maximum number of keys that can be requested in a single request.
pub const MAX_KEYS_PER_REQUEST: usize = 10;

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
    async fn get_info(&self, _request: Request<Empty>) -> Result<Response<InfoReply>, Status> {
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

        let reply = InfoReply {
            min_sapling_birthday_height: min_sapling_birthday_height.0,
        };

        Ok(Response::new(reply))
    }

    async fn register_keys(
        &self,
        request: Request<RegisterKeysRequest>,
    ) -> Result<Response<RegisterKeysResponse>, Status> {
        let keys: Vec<_> = request
            .into_inner()
            .keys
            .into_iter()
            .map(|key_with_height| (key_with_height.key, key_with_height.height))
            .collect();

        if keys.is_empty() {
            let msg = "must provide at least 1 key for which to register keys";
            return Err(Status::invalid_argument(msg));
        }

        if keys.len() > MAX_KEYS_PER_REQUEST {
            let msg = format!(
                "must provide at most {} keys to register keys",
                MAX_KEYS_PER_REQUEST
            );
            return Err(Status::invalid_argument(msg));
        }

        let ScanServiceResponse::RegisteredKeys(keys) = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::RegisterKeys(keys)))
            .await
            .map_err(|_| Status::unknown("scan service was unavailable"))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        Ok(Response::new(RegisterKeysResponse { keys }))
    }

    async fn clear_results(
        &self,
        request: Request<ClearResultsRequest>,
    ) -> Result<Response<Empty>, Status> {
        let keys = request.into_inner().keys;

        if keys.is_empty() {
            let msg = "must provide at least 1 key for which to clear results";
            return Err(Status::invalid_argument(msg));
        }

        if keys.len() > MAX_KEYS_PER_REQUEST {
            let msg = format!(
                "must provide at most {} keys to clear results",
                MAX_KEYS_PER_REQUEST
            );
            return Err(Status::invalid_argument(msg));
        }

        let ScanServiceResponse::ClearedResults = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::ClearResults(keys)))
            .await
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        Ok(Response::new(Empty {}))
    }

    async fn delete_keys(
        &self,
        request: Request<DeleteKeysRequest>,
    ) -> Result<Response<Empty>, Status> {
        let keys = request.into_inner().keys;

        if keys.is_empty() {
            let msg = "must provide at least 1 key to delete";
            return Err(Status::invalid_argument(msg));
        }

        if keys.len() > MAX_KEYS_PER_REQUEST {
            let msg = format!(
                "must provide at most {} keys to delete",
                MAX_KEYS_PER_REQUEST
            );
            return Err(Status::invalid_argument(msg));
        }

        let ScanServiceResponse::DeletedKeys = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::DeleteKeys(keys)))
            .await
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        Ok(Response::new(Empty {}))
    }

    async fn get_results(
        &self,
        request: Request<GetResultsRequest>,
    ) -> Result<Response<GetResultsResponse>, Status> {
        let keys = request.into_inner().keys;

        if keys.is_empty() {
            let msg = "must provide at least 1 key to get results";
            return Err(Status::invalid_argument(msg));
        }

        if keys.len() > MAX_KEYS_PER_REQUEST {
            let msg = format!(
                "must provide at most {} keys to get results",
                MAX_KEYS_PER_REQUEST
            );
            return Err(Status::invalid_argument(msg));
        }

        let ScanServiceResponse::Results(response) = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::Results(keys.clone())))
            .await
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        // If there are no results for a key, we still want to return it with empty results.
        let empty_map = BTreeMap::new();

        let results = keys
            .into_iter()
            .map(|key| {
                let values = response.get(&key).unwrap_or(&empty_map);

                // Skip heights with no transactions, they are scanner markers and should not be returned.
                let transactions = Results {
                    transactions: values
                        .iter()
                        .filter(|(_, transactions)| !transactions.is_empty())
                        .map(|(height, transactions)| {
                            let txs = transactions.iter().map(ToString::to_string).collect();
                            (height.0, TransactionHash { hash: txs })
                        })
                        .collect(),
                };

                (key, transactions)
            })
            .collect::<BTreeMap<_, _>>();

        Ok(Response::new(GetResultsResponse { results }))
    }
}

/// Initializes the zebra-scan gRPC server
pub async fn init<ScanService>(
    listen_addr: SocketAddr,
    scan_service: ScanService,
) -> Result<(), color_eyre::Report>
where
    ScanService: tower::Service<ScanServiceRequest, Response = ScanServiceResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ScanService as tower::Service<ScanServiceRequest>>::Future: Send,
{
    let service = ScannerRPC { scan_service };

    Server::builder()
        .add_service(ScannerServer::new(service))
        .serve(listen_addr)
        .await?;

    Ok(())
}
