//! The gRPC server implementation

use std::{collections::BTreeMap, net::SocketAddr, pin::Pin};

use futures_util::future::TryFutureExt;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{transport::Server, Request, Response, Status};
use tower::ServiceExt;

use zebra_chain::block::Height;
use zebra_node_services::scan_service::{
    request::Request as ScanServiceRequest,
    response::{Response as ScanServiceResponse, ScanResult},
};

use crate::scanner::{
    scanner_server::{Scanner, ScannerServer},
    ClearResultsRequest, DeleteKeysRequest, Empty, GetResultsRequest, GetResultsResponse,
    InfoReply, KeyWithHeight, RegisterKeysRequest, RegisterKeysResponse, Results, ScanRequest,
    ScanResponse, ScanResults, TransactionHash,
};

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
    type ScanStream = Pin<Box<dyn Stream<Item = Result<ScanResponse, Status>> + Send>>;

    async fn scan(
        &self,
        request: tonic::Request<ScanRequest>,
    ) -> Result<Response<Self::ScanStream>, Status> {
        let keys = request.into_inner().keys;

        if keys.is_empty() {
            let msg = "must provide at least 1 key in scan request";
            return Err(Status::invalid_argument(msg));
        }

        let keys: Vec<_> = keys
            .into_iter()
            .map(|KeyWithHeight { key, height }| (key, height))
            .collect();

        let ScanServiceResponse::RegisteredKeys(_) = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::RegisterKeys(keys.clone())))
            .await
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        let keys: Vec<_> = keys.into_iter().map(|(key, _start_at)| key).collect();

        let ScanServiceResponse::Results(results) = self
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

        let ScanServiceResponse::SubscribeResults(mut results_receiver) = self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| {
                service.call(ScanServiceRequest::SubscribeResults(
                    keys.iter().cloned().collect(),
                ))
            })
            .await
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(10_000);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            // Transpose the nested BTreeMaps
            let mut initial_results: BTreeMap<Height, BTreeMap<String, Vec<Vec<u8>>>> =
                BTreeMap::new();
            for (key, results_by_height) in results {
                assert!(
                    keys.contains(&key),
                    "should not return results for keys that weren't provided"
                );

                for (height, results_for_key) in results_by_height {
                    if results_for_key.is_empty() {
                        continue;
                    }

                    let results_for_height = initial_results.entry(height).or_default();
                    results_for_height.entry(key.clone()).or_default().extend(
                        results_for_key
                            .into_iter()
                            .map(|result| result.bytes_in_display_order().to_vec()),
                    );
                }
            }

            for (Height(height), results) in initial_results {
                response_sender
                    .send(Ok(ScanResponse {
                        height,
                        results: results
                            .into_iter()
                            .map(|(key, results)| (key, ScanResults { tx_ids: results }))
                            .collect(),
                    }))
                    .await
                    .expect("channel should not be disconnected");
            }

            while let Some(ScanResult {
                key,
                height: Height(height),
                tx_id,
            }) = results_receiver.recv().await
            {
                let send_result = response_sender
                    .send(Ok(ScanResponse {
                        height,
                        results: [(
                            key,
                            ScanResults {
                                tx_ids: vec![tx_id.bytes_in_display_order().to_vec()],
                            },
                        )]
                        .into_iter()
                        .collect(),
                    }))
                    .await;

                // Finish task if the client has disconnected
                if send_result.is_err() {
                    break;
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

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

        let reply = InfoReply {
            min_sapling_birthday_height: min_sapling_birthday_height.0,
        };

        Ok(Response::new(reply))
    }

    async fn register_keys(
        &self,
        request: Request<RegisterKeysRequest>,
    ) -> Result<Response<RegisterKeysResponse>, Status> {
        let keys = request
            .into_inner()
            .keys
            .into_iter()
            .map(|key_with_height| (key_with_height.key, key_with_height.height))
            .collect();

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
