//! The gRPC server implementation

use std::{collections::BTreeMap, net::SocketAddr, pin::Pin};

use color_eyre::eyre::eyre;
use futures_util::future::TryFutureExt;
use tokio::task::JoinHandle;
use tokio_stream::{wrappers::ReceiverStream, Stream};
use tonic::{
    transport::{server::TcpIncoming, Server},
    Request, Response, Status,
};
use tower::{timeout::error::Elapsed, ServiceExt};

use zebra_chain::{block::Height, transaction};
use zebra_node_services::scan_service::{
    request::Request as ScanServiceRequest,
    response::{Response as ScanServiceResponse, ScanResult},
};

use crate::scanner::{
    scanner_server::{Scanner, ScannerServer},
    ClearResultsRequest, DeleteKeysRequest, Empty, GetResultsRequest, GetResultsResponse,
    InfoReply, KeyWithHeight, RegisterKeysRequest, RegisterKeysResponse, Results, ScanRequest,
    ScanResponse, Transaction, Transactions,
};

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The maximum number of keys that can be requested in a single request.
pub const MAX_KEYS_PER_REQUEST: usize = 10;

/// The maximum number of messages that can be queued to be streamed to a client
/// from the `scan` method.
const SCAN_RESPONDER_BUFFER_SIZE: usize = 10_000;

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

        let register_keys_response_fut = self
            .scan_service
            .clone()
            .ready()
            .await
            .map_err(|_| Status::unknown("service poll_ready() method returned an error"))?
            .call(ScanServiceRequest::RegisterKeys(keys.clone()));

        let keys: Vec<_> = keys.into_iter().map(|(key, _start_at)| key).collect();

        let subscribe_results_response_fut = self
            .scan_service
            .clone()
            .ready()
            .await
            .map_err(|_| Status::unknown("service poll_ready() method returned an error"))?
            .call(ScanServiceRequest::SubscribeResults(
                keys.iter().cloned().collect(),
            ));

        let (register_keys_response, subscribe_results_response) =
            tokio::join!(register_keys_response_fut, subscribe_results_response_fut);

        // Ignores errors from the register keys request, we expect there to be a timeout if the keys
        // are already registered, or an empty response if no new keys could be parsed as Sapling efvks.
        //
        // This method will still return an error if every key in the `scan` request is invalid, since
        // the SubscribeResults request will return an error once the `rsp_tx` is dropped in `ScanTask::process_messages`
        // when it finds that none of the keys in the request are registered.
        let register_keys_err = match register_keys_response {
            Ok(ScanServiceResponse::RegisteredKeys(_)) => None,
            Ok(response) => {
                return Err(Status::internal(format!(
                    "unexpected response from scan service: {response:?}"
                )))
            }
            Err(err) if err.downcast_ref::<Elapsed>().is_some() => {
                return Err(Status::deadline_exceeded(
                    "scan service requests timed out, is Zebra synced past Sapling activation height?")
                )
            }
            Err(err) => Some(err),
        };

        let ScanServiceResponse::SubscribeResults(mut results_receiver) =
            subscribe_results_response.map_err(|err| {
                register_keys_err
                    .map(|err| Status::invalid_argument(err.to_string()))
                    .unwrap_or(Status::internal(err.to_string()))
            })?
        else {
            return Err(Status::unknown(
                "scan service returned an unexpected response",
            ));
        };

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

        let (response_sender, response_receiver) =
            tokio::sync::mpsc::channel(SCAN_RESPONDER_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            let mut initial_results = process_results(keys, results);

            // Empty results receiver channel to filter out duplicate results between the channel and cache
            while let Ok(ScanResult { key, height, tx_id }) = results_receiver.try_recv() {
                let entry = initial_results
                    .entry(key)
                    .or_default()
                    .by_height
                    .entry(height.0)
                    .or_default();

                let tx_id = Transaction {
                    hash: tx_id.to_string(),
                };

                // Add the scan result to the initial results if it's not already present.
                if !entry.transactions.contains(&tx_id) {
                    entry.transactions.push(tx_id);
                }
            }

            let send_result = response_sender
                .send(Ok(ScanResponse {
                    results: initial_results,
                }))
                .await;

            if send_result.is_err() {
                // return early if the client has disconnected
                return;
            }

            while let Some(scan_result) = results_receiver.recv().await {
                let send_result = response_sender.send(Ok(scan_result.into())).await;

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
            .map_err(|err| Status::unknown(format!("scan service returned error: {err}")))?
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
            let msg = format!("must provide at most {MAX_KEYS_PER_REQUEST} keys to register keys");
            return Err(Status::invalid_argument(msg));
        }

        match self
            .scan_service
            .clone()
            .ready()
            .and_then(|service| service.call(ScanServiceRequest::RegisterKeys(keys)))
            .await
        {
            Ok(ScanServiceResponse::RegisteredKeys(keys)) => {
                Ok(Response::new(RegisterKeysResponse { keys }))
            }

            Ok(response) => {
                return Err(Status::internal(format!(
                    "unexpected response from scan service: {response:?}"
                )))
            }

            Err(err) if err.downcast_ref::<Elapsed>().is_some() => Err(Status::deadline_exceeded(
                "RegisterKeys scan service request timed out, \
                    is Zebra synced past Sapling activation height?",
            )),

            Err(err) => Err(Status::unknown(err.to_string())),
        }
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
            let msg = format!("must provide at most {MAX_KEYS_PER_REQUEST} keys to clear results");
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
            let msg = format!("must provide at most {MAX_KEYS_PER_REQUEST} keys to delete");
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
            let msg = format!("must provide at most {MAX_KEYS_PER_REQUEST} keys to get results");
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

        let results = process_results(keys, response);

        Ok(Response::new(GetResultsResponse { results }))
    }
}

fn process_results(
    keys: Vec<String>,
    results: BTreeMap<String, BTreeMap<Height, Vec<transaction::Hash>>>,
) -> BTreeMap<String, Results> {
    // If there are no results for a key, we still want to return it with empty results.
    let empty_map = BTreeMap::new();

    keys.into_iter()
        .map(|key| {
            let values = results.get(&key).unwrap_or(&empty_map);

            // Skip heights with no transactions, they are scanner markers and should not be returned.
            let transactions = Results {
                by_height: values
                    .iter()
                    .filter(|(_, transactions)| !transactions.is_empty())
                    .map(|(height, transactions)| {
                        let transactions = transactions
                            .iter()
                            .map(ToString::to_string)
                            .map(|hash| Transaction { hash })
                            .collect();
                        (height.0, Transactions { transactions })
                    })
                    .collect(),
            };

            (key, transactions)
        })
        .collect::<BTreeMap<_, _>>()
}

impl From<ScanResult> for ScanResponse {
    fn from(
        ScanResult {
            key,
            height: Height(height),
            tx_id,
        }: ScanResult,
    ) -> Self {
        ScanResponse {
            results: [(
                key,
                Results {
                    by_height: [(
                        height,
                        Transactions {
                            transactions: [tx_id.to_string()]
                                .map(|hash| Transaction { hash })
                                .to_vec(),
                        },
                    )]
                    .into_iter()
                    .collect(),
                },
            )]
            .into_iter()
            .collect(),
        }
    }
}

type ServerTask = JoinHandle<Result<(), tonic::transport::Error>>;

/// Initializes the zebra-scan gRPC server
pub async fn init<ScanService>(
    listen_addr: SocketAddr,
    scan_service: ScanService,
) -> Result<(ServerTask, SocketAddr), color_eyre::Report>
where
    ScanService: tower::Service<ScanServiceRequest, Response = ScanServiceResponse, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    <ScanService as tower::Service<ScanServiceRequest>>::Future: Send,
{
    let service = ScannerRPC { scan_service };
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::scanner::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;
    let listen_addr = tcp_listener.local_addr()?;
    let incoming =
        TcpIncoming::from_listener(tcp_listener, true, None).map_err(|err| eyre!(err))?;

    let server_task: JoinHandle<Result<(), tonic::transport::Error>> = tokio::spawn(async move {
        Server::builder()
            .add_service(reflection_service)
            .add_service(ScannerServer::new(service))
            .serve_with_incoming(incoming)
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}
