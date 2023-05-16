//! The inbound service handles requests from Zebra's peers.
//!
//! It downloads and verifies gossiped blocks and mempool transactions,
//! when Zebra is close to the chain tip.
//!
//! It also responds to peer requests for blocks, transactions, and peer addresses.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use chrono::Utc;
use futures::{
    future::{FutureExt, TryFutureExt},
    stream::Stream,
};
use tokio::sync::oneshot::{self, error::TryRecvError};
use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service, ServiceExt};

use zebra_network as zn;
use zebra_state as zs;

use zebra_chain::{
    block::{self, Block},
    serialization::ZcashSerialize,
    transaction::UnminedTxId,
};
use zebra_consensus::chain::VerifyChainError;
use zebra_network::{
    constants::{ADDR_RESPONSE_LIMIT_DENOMINATOR, MAX_ADDRS_IN_MESSAGE},
    AddressBook, InventoryResponse,
};
use zebra_node_services::mempool;

use crate::BoxError;

// Re-use the syncer timeouts for consistency.
use super::sync::{BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT};

use InventoryResponse::*;

pub(crate) mod downloads;

#[cfg(test)]
mod tests;

use downloads::Downloads as BlockDownloads;

/// The number of bytes the [`Inbound`] service will queue in response to a single block or
/// transaction request, before ignoring any additional block or transaction IDs in that request.
///
/// This is the same as `zcashd`'s default send buffer limit:
/// <https://github.com/zcash/zcash/blob/829dd94f9d253bb705f9e194f13cb8ca8e545e1e/src/net.h#L84>
/// as used in `ProcessGetData()`:
/// <https://github.com/zcash/zcash/blob/829dd94f9d253bb705f9e194f13cb8ca8e545e1e/src/main.cpp#L6410-L6412>
pub const GETDATA_SENT_BYTES_LIMIT: usize = 1_000_000;

/// The maximum number of blocks the [`Inbound`] service will queue in response to a block request,
/// before ignoring any additional block IDs in that request.
///
/// This is the same as `zcashd`'s request limit:
/// <https://github.com/zcash/zcash/blob/829dd94f9d253bb705f9e194f13cb8ca8e545e1e/src/main.h#L108>
///
/// (Zebra's request limit is one block in transit per peer, because it fans out block requests to
/// many peers instead of just a few peers.)
pub const GETDATA_MAX_BLOCK_COUNT: usize = 16;

type BlockDownloadPeerSet =
    Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type Mempool = Buffer<BoxService<mempool::Request, mempool::Response, BoxError>, mempool::Request>;
type BlockVerifier = Buffer<
    BoxService<zebra_consensus::Request, block::Hash, VerifyChainError>,
    zebra_consensus::Request,
>;
type GossipedBlockDownloads =
    BlockDownloads<Timeout<BlockDownloadPeerSet>, Timeout<BlockVerifier>, State>;

/// The services used by the [`Inbound`] service.
pub struct InboundSetupData {
    /// A shared list of peer addresses.
    pub address_book: Arc<std::sync::Mutex<AddressBook>>,

    /// A service that can be used to download gossiped blocks.
    pub block_download_peer_set: BlockDownloadPeerSet,

    /// A service that verifies downloaded blocks.
    ///
    /// Given to `Inbound.block_downloads` after the required services are set up.
    pub block_verifier: BlockVerifier,

    /// A service that manages transactions in the memory pool.
    pub mempool: Mempool,

    /// A service that manages cached blockchain state.
    pub state: State,

    /// Allows efficient access to the best tip of the blockchain.
    pub latest_chain_tip: zs::LatestChainTip,
}

/// Tracks the internal state of the [`Inbound`] service during setup.
pub enum Setup {
    /// Waiting for service setup to complete.
    ///
    /// All requests are ignored.
    Pending {
        // Configuration
        //
        /// The configured full verification concurrency limit.
        full_verify_concurrency_limit: usize,

        // Services
        //
        /// A oneshot channel used to receive required services,
        /// after they are set up.
        setup: oneshot::Receiver<InboundSetupData>,
    },

    /// Setup is complete.
    ///
    /// All requests are answered.
    Initialized {
        // Services
        //
        /// A shared list of peer addresses.
        address_book: Arc<std::sync::Mutex<zn::AddressBook>>,

        /// A `futures::Stream` that downloads and verifies gossiped blocks.
        block_downloads: Pin<Box<GossipedBlockDownloads>>,

        /// A service that manages transactions in the memory pool.
        mempool: Mempool,

        /// A service that manages cached blockchain state.
        state: State,
    },

    /// Temporary state used in the inbound service's internal initialization code.
    ///
    /// If this state occurs outside the service initialization code, the service panics.
    FailedInit,

    /// Setup failed, because the setup channel permanently failed.
    /// The service keeps returning readiness errors for every request.
    FailedRecv {
        /// The original channel error.
        error: SharedRecvError,
    },
}

/// A wrapper around `Arc<TryRecvError>` that implements `Error`.
#[derive(thiserror::Error, Debug, Clone)]
#[error(transparent)]
pub struct SharedRecvError(Arc<TryRecvError>);

impl From<TryRecvError> for SharedRecvError {
    fn from(source: TryRecvError) -> Self {
        Self(Arc::new(source))
    }
}

/// Uses the node state to respond to inbound peer requests.
///
/// This service, wrapped in appropriate middleware, is passed to
/// `zebra_network::init` to respond to inbound peer requests.
///
/// The `Inbound` service is responsible for:
///
/// - supplying network data like peer addresses to other nodes;
/// - supplying chain data like blocks to other nodes;
/// - supplying mempool transactions to other nodes;
/// - receiving gossiped transactions; and
/// - receiving gossiped blocks.
///
/// Because the `Inbound` service is responsible for participating in the gossip
/// protocols used for transaction and block diffusion, there is a potential
/// overlap with the `ChainSync` and `Mempool` components.
///
/// The division of responsibility is that:
///
/// The `ChainSync` and `Mempool` components are *internally driven*,
/// periodically polling the network to check for new blocks or transactions.
///
/// The `Inbound` service is *externally driven*, responding to block gossip
/// by attempting to download and validate advertised blocks.
///
/// Gossiped transactions are forwarded to the mempool downloader,
/// which unifies polled and gossiped transactions into a single download list.
pub struct Inbound {
    /// Provides service dependencies, if they are available.
    ///
    /// Some services are unavailable until Zebra has completed setup.
    setup: Setup,
}

impl Inbound {
    /// Create a new inbound service.
    ///
    /// Dependent services are sent via the `setup` channel after initialization.
    pub fn new(
        full_verify_concurrency_limit: usize,
        setup: oneshot::Receiver<InboundSetupData>,
    ) -> Inbound {
        Inbound {
            setup: Setup::Pending {
                full_verify_concurrency_limit,
                setup,
            },
        }
    }

    /// Remove `self.setup`, temporarily replacing it with an invalid state.
    fn take_setup(&mut self) -> Setup {
        let mut setup = Setup::FailedInit;
        std::mem::swap(&mut self.setup, &mut setup);
        setup
    }
}

impl Service<zn::Request> for Inbound {
    type Response = zn::Response;
    type Error = zn::BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Check whether the setup is finished, but don't wait for it to
        // become ready before reporting readiness. We expect to get it "soon",
        // and reporting unreadiness might cause unwanted load-shedding, since
        // the load-shed middleware is unable to distinguish being unready due
        // to load from being unready while waiting on setup.

        // Every setup variant handler must provide a result
        let result;

        self.setup = match self.take_setup() {
            Setup::Pending {
                full_verify_concurrency_limit,
                mut setup,
            } => match setup.try_recv() {
                Ok(setup_data) => {
                    let InboundSetupData {
                        address_book,
                        block_download_peer_set,
                        block_verifier,
                        mempool,
                        state,
                        latest_chain_tip,
                    } = setup_data;

                    let block_downloads = Box::pin(BlockDownloads::new(
                        full_verify_concurrency_limit,
                        Timeout::new(block_download_peer_set, BLOCK_DOWNLOAD_TIMEOUT),
                        Timeout::new(block_verifier, BLOCK_VERIFY_TIMEOUT),
                        state.clone(),
                        latest_chain_tip,
                    ));

                    result = Ok(());
                    Setup::Initialized {
                        address_book,
                        block_downloads,
                        mempool,
                        state,
                    }
                }
                Err(TryRecvError::Empty) => {
                    // There's no setup data yet, so keep waiting for it
                    result = Ok(());
                    Setup::Pending {
                        full_verify_concurrency_limit,
                        setup,
                    }
                }
                Err(error @ TryRecvError::Closed) => {
                    // Mark the service as failed, because setup failed
                    error!(?error, "inbound setup failed");
                    let error: SharedRecvError = error.into();
                    result = Err(error.clone().into());
                    Setup::FailedRecv { error }
                }
            },
            // Make sure previous setups were left in a valid state
            Setup::FailedInit => unreachable!("incomplete previous Inbound initialization"),
            // If setup failed, report service failure
            Setup::FailedRecv { error } => {
                result = Err(error.clone().into());
                Setup::FailedRecv { error }
            }
            // Clean up completed download tasks, ignoring their results
            Setup::Initialized {
                address_book,
                mut block_downloads,
                mempool,
                state,
            } => {
                while let Poll::Ready(Some(_)) = block_downloads.as_mut().poll_next(cx) {}

                result = Ok(());

                Setup::Initialized {
                    address_book,
                    block_downloads,
                    mempool,
                    state,
                }
            }
        };

        // Make sure we're leaving the setup in a valid state
        if matches!(self.setup, Setup::FailedInit) {
            unreachable!("incomplete Inbound initialization after poll_ready state handling");
        }

        // TODO:
        //  * do we want to propagate backpressure from the download queue or its outbound network?
        //    currently, the download queue waits for the outbound network in the download future,
        //    and drops new requests after it reaches a hard-coded limit. This is the
        //    "load shed directly" pattern from #1618.
        //  * currently, the state service is always ready, unless its buffer is full.
        //    So we might also want to propagate backpressure from its buffer.
        //  * poll_ready needs to be implemented carefully, to avoid hangs or deadlocks.
        //    See #1593 for details.
        Poll::Ready(result)
    }

    /// Call the inbound service.
    ///
    /// Errors indicate that the peer has done something wrong or unexpected,
    /// and will cause callers to disconnect from the remote peer.
    #[instrument(name = "inbound", skip(self, req))]
    fn call(&mut self, req: zn::Request) -> Self::Future {
        let (address_book, block_downloads, mempool, state) = match &mut self.setup {
            Setup::Initialized {
                address_book,
                block_downloads,
                mempool,
                state,
            } => (address_book, block_downloads, mempool, state),
            _ => {
                debug!("ignoring request from remote peer during setup");
                return async { Ok(zn::Response::Nil) }.boxed();
            }
        };

        match req {
            zn::Request::Peers => {
                // # Security
                //
                // We truncate the list to not reveal our entire peer set in one call.
                // But we don't monitor repeated requests and the results are shuffled,
                // a crawler could just send repeated queries and get the full list.
                //
                // # Correctness
                //
                // Briefly hold the address book threaded mutex while
                // cloning the address book. Then sanitize in the future,
                // after releasing the lock.
                let peers = address_book.lock().unwrap().clone();

                async move {
                    // Correctness: get the current time after acquiring the address book lock.
                    //
                    // This time is used to filter outdated peers, so it doesn't really matter
                    // if we get it when the future is created, or when it starts running.
                    let now = Utc::now();

                    // Send a sanitized response
                    let mut peers = peers.sanitized(now);

                    // Truncate the list
                    //
                    // TODO: replace with div_ceil once it stabilises
                    //       https://github.com/rust-lang/rust/issues/88581
                    let address_limit = (peers.len() + ADDR_RESPONSE_LIMIT_DENOMINATOR - 1) / ADDR_RESPONSE_LIMIT_DENOMINATOR;
                    let address_limit = MAX_ADDRS_IN_MESSAGE.min(address_limit);
                    peers.truncate(address_limit);

                    if peers.is_empty() {
                        // We don't know if the peer response will be empty until we've sanitized them.
                        debug!("ignoring `Peers` request from remote peer because our address book is empty");
                        Ok(zn::Response::Nil)
                    } else {
                        Ok(zn::Response::Peers(peers))
                    }
                }.boxed()
            }
            zn::Request::BlocksByHash(hashes) => {
                // We return an available or missing response to each inventory request,
                // unless the request is empty, or it reaches a response limit.
                if hashes.is_empty() {
                    return async { Ok(zn::Response::Nil) }.boxed();
                }

                let state = state.clone();

                async move {
                    let mut blocks: Vec<InventoryResponse<Arc<Block>, block::Hash>> = Vec::new();
                    let mut total_size = 0;

                    // Ignore any block hashes past the response limit.
                    // This saves us expensive database lookups.
                    for &hash in hashes.iter().take(GETDATA_MAX_BLOCK_COUNT) {
                        // We check the limit after including at least one block, so that we can
                        // send blocks greater than 1 MB (but only one at a time)
                        if total_size >= GETDATA_SENT_BYTES_LIMIT {
                            break;
                        }

                        let response = state.clone().ready().await?.call(zs::Request::Block(hash.into())).await?;

                        // Add the block responses to the list, while updating the size limit.
                        //
                        // If there was a database error, return the error,
                        // and stop processing further chunks.
                        match response {
                            zs::Response::Block(Some(block)) => {
                                // If checking the serialized size of the block performs badly,
                                // return the size from the state using a wrapper type.
                                total_size += block.zcash_serialized_size();

                                blocks.push(Available(block))
                            },
                            // We don't need to limit the size of the missing block IDs list,
                            // because it is already limited to the size of the getdata request
                            // sent by the peer. (Their content and encodings are the same.)
                            zs::Response::Block(None) => blocks.push(Missing(hash)),
                            _ => unreachable!("wrong response from state"),
                        }

                    }

                    // The network layer handles splitting this response into multiple `block`
                    // messages, and a `notfound` message if needed.
                    Ok(zn::Response::Blocks(blocks))
                }.boxed()
            }
            zn::Request::TransactionsById(req_tx_ids) => {
                // We return an available or missing response to each inventory request,
                // unless the request is empty, or it reaches a response limit.
                if req_tx_ids.is_empty() {
                    return async { Ok(zn::Response::Nil) }.boxed();
                }

                let request = mempool::Request::TransactionsById(req_tx_ids.clone());
                mempool.clone().oneshot(request).map_ok(move |resp| {
                    let mut total_size = 0;

                    let transactions = match resp {
                        mempool::Response::Transactions(transactions) => transactions,
                        _ => unreachable!("Mempool component should always respond to a `TransactionsById` request with a `Transactions` response"),
                    };

                    // Work out which transaction IDs were missing.
                    let available_tx_ids: HashSet<UnminedTxId> = transactions.iter().map(|tx| tx.id).collect();
                    // We don't need to limit the size of the missing transaction IDs list,
                    // because it is already limited to the size of the getdata request
                    // sent by the peer. (Their content and encodings are the same.)
                    let missing = req_tx_ids.into_iter().filter(|tx_id| !available_tx_ids.contains(tx_id)).map(Missing);

                    // If we skip sending some transactions because the limit has been reached,
                    // they aren't reported as missing. This matches `zcashd`'s behaviour:
                    // <https://github.com/zcash/zcash/blob/829dd94f9d253bb705f9e194f13cb8ca8e545e1e/src/main.cpp#L6410-L6412>
                    let available = transactions.into_iter().take_while(|tx| {
                        // We check the limit after including the transaction,
                        // so that we can send transactions greater than 1 MB
                        // (but only one at a time)
                        let within_limit = total_size < GETDATA_SENT_BYTES_LIMIT;

                        total_size += tx.size;

                        within_limit
                    }).map(Available);

                    // The network layer handles splitting this response into multiple `tx`
                    // messages, and a `notfound` message if needed.
                    zn::Response::Transactions(available.chain(missing).collect())
                }).boxed()
            }
            // Find* responses are already size-limited by the state.
            zn::Request::FindBlocks { known_blocks, stop } => {
                let request = zs::Request::FindBlockHashes { known_blocks, stop };
                state.clone().oneshot(request).map_ok(|resp| match resp {
                    zs::Response::BlockHashes(hashes) if hashes.is_empty() => zn::Response::Nil,
                    zs::Response::BlockHashes(hashes) => zn::Response::BlockHashes(hashes),
                    _ => unreachable!("zebra-state should always respond to a `FindBlockHashes` request with a `BlockHashes` response"),
                })
                    .boxed()
            }
            zn::Request::FindHeaders { known_blocks, stop } => {
                let request = zs::Request::FindBlockHeaders { known_blocks, stop };
                state.clone().oneshot(request).map_ok(|resp| match resp {
                    zs::Response::BlockHeaders(headers) if headers.is_empty() => zn::Response::Nil,
                    zs::Response::BlockHeaders(headers) => zn::Response::BlockHeaders(headers),
                    _ => unreachable!("zebra-state should always respond to a `FindBlockHeaders` request with a `BlockHeaders` response"),
                })
                    .boxed()
            }
            zn::Request::PushTransaction(transaction) => {
                mempool
                    .clone()
                    .oneshot(mempool::Request::Queue(vec![transaction.into()]))
                    // The response just indicates if processing was queued or not; ignore it
                    .map_ok(|_resp| zn::Response::Nil)
                    .boxed()
            }
            zn::Request::AdvertiseTransactionIds(transactions) => {
                let transactions = transactions.into_iter().map(Into::into).collect();
                mempool
                    .clone()
                    .oneshot(mempool::Request::Queue(transactions))
                    // The response just indicates if processing was queued or not; ignore it
                    .map_ok(|_resp| zn::Response::Nil)
                    .boxed()
            }
            zn::Request::AdvertiseBlock(hash) => {
                block_downloads.download_and_verify(hash);
                async { Ok(zn::Response::Nil) }.boxed()
            }
            // The size of this response is limited by the `Connection` state machine in the network layer
            zn::Request::MempoolTransactionIds => {
                mempool.clone().oneshot(mempool::Request::TransactionIds).map_ok(|resp| match resp {
                    mempool::Response::TransactionIds(transaction_ids) if transaction_ids.is_empty() => zn::Response::Nil,
                    mempool::Response::TransactionIds(transaction_ids) => zn::Response::TransactionIds(transaction_ids.into_iter().collect()),
                    _ => unreachable!("Mempool component should always respond to a `TransactionIds` request with a `TransactionIds` response"),
                })
                    .boxed()
            }
            zn::Request::Ping(_) => {
                unreachable!("ping requests are handled internally");
            }
        }
    }
}
