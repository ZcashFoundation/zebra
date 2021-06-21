use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures::{
    future::{FutureExt, TryFutureExt},
    stream::Stream,
};
use oneshot::error::TryRecvError;
use tokio::sync::oneshot;
use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service, ServiceExt};

use zebra_network as zn;
use zebra_state as zs;

use zebra_chain::block::{self, Block};
use zebra_consensus::chain::VerifyChainError;
use zebra_network::AddressBook;

// Re-use the syncer timeouts for consistency.
use super::sync::{BLOCK_DOWNLOAD_TIMEOUT, BLOCK_VERIFY_TIMEOUT};

mod downloads;
use downloads::Downloads;

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type Verifier = Buffer<BoxService<Arc<Block>, block::Hash, VerifyChainError>, Arc<Block>>;
type InboundDownloads = Downloads<Timeout<Outbound>, Timeout<Verifier>, State>;

pub type NetworkSetupData = (Outbound, Arc<std::sync::Mutex<AddressBook>>);

/// Tracks the internal state of the [`Inbound`] service during network setup.
pub enum Setup {
    /// Waiting for network setup to complete.
    ///
    /// Requests that depend on Zebra's internal network setup are ignored.
    /// Other requests are answered.
    AwaitingNetwork {
        /// A oneshot channel used to receive the address_book and outbound services
        /// after the network is set up.
        network_setup: oneshot::Receiver<NetworkSetupData>,

        /// A service that verifies downloaded blocks. Given to `downloads`
        /// after the network is set up.
        verifier: Verifier,
    },

    /// Network setup is complete.
    ///
    /// All requests are answered.
    Initialized {
        /// A shared list of peer addresses.
        address_book: Arc<std::sync::Mutex<zn::AddressBook>>,

        /// A `futures::Stream` that downloads and verifies gossiped blocks.
        downloads: Pin<Box<InboundDownloads>>,
    },

    /// Temporary state used in the service's internal network initialization
    /// code.
    ///
    /// If this state occurs outside the service initialization code, the
    /// service panics.
    FailedInit,

    /// Network setup failed, because the setup channel permanently failed.
    /// The service keeps returning readiness errors for every request.
    FailedRecv { error: SharedRecvError },
}

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
/// - performing transaction diffusion;
/// - performing block diffusion.
///
/// Because the `Inbound` service is responsible for participating in the gossip
/// protocols used for transaction and block diffusion, there is a potential
/// overlap with the `ChainSync` component.
///
/// The division of responsibility is that the `ChainSync` component is
/// *internally driven*, periodically polling the network to check whether it is
/// behind the current tip, while the `Inbound` service is *externally driven*,
/// responding to block gossip by attempting to download and validate advertised
/// blocks.
pub struct Inbound {
    /// Provides network-dependent services, if they are available.
    ///
    /// Some services are unavailable until Zebra has completed network setup.
    network_setup: Setup,

    /// A service that manages cached blockchain state.
    state: State,
}

impl Inbound {
    pub fn new(
        network_setup: oneshot::Receiver<NetworkSetupData>,
        state: State,
        verifier: Verifier,
    ) -> Self {
        Self {
            network_setup: Setup::AwaitingNetwork {
                network_setup,
                verifier,
            },
            state,
        }
    }

    fn take_setup(&mut self) -> Setup {
        let mut network_setup = Setup::FailedInit;
        std::mem::swap(&mut self.network_setup, &mut network_setup);
        network_setup
    }
}

impl Service<zn::Request> for Inbound {
    type Response = zn::Response;
    type Error = zn::BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // Check whether the network setup is finished, but don't wait for it to
        // become ready before reporting readiness. We expect to get it "soon",
        // and reporting unreadiness might cause unwanted load-shedding, since
        // the load-shed middleware is unable to distinguish being unready due
        // to load from being unready while waiting on setup.

        // Every network_setup state handler must provide a result
        let result;

        self.network_setup = match self.take_setup() {
            Setup::AwaitingNetwork {
                mut network_setup,
                verifier,
            } => match network_setup.try_recv() {
                Ok((outbound, address_book)) => {
                    let downloads = Box::pin(Downloads::new(
                        Timeout::new(outbound, BLOCK_DOWNLOAD_TIMEOUT),
                        Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT),
                        self.state.clone(),
                    ));
                    result = Ok(());
                    Setup::Initialized {
                        address_book,
                        downloads,
                    }
                }
                Err(TryRecvError::Empty) => {
                    // There's no setup data yet, so keep waiting for it
                    result = Ok(());
                    Setup::AwaitingNetwork {
                        network_setup,
                        verifier,
                    }
                }
                Err(error @ TryRecvError::Closed) => {
                    // Mark the service as failed, because network setup failed
                    error!(?error, "inbound network setup failed");
                    let error: SharedRecvError = error.into();
                    result = Err(error.clone().into());
                    Setup::FailedRecv { error }
                }
            },
            // Make sure previous network setups were left in a valid state
            Setup::FailedInit => unreachable!("incomplete previous Inbound initialization"),
            // If network setup failed, report service failure
            Setup::FailedRecv { error } => {
                result = Err(error.clone().into());
                Setup::FailedRecv { error }
            }
            // Clean up completed download tasks, ignoring their results
            Setup::Initialized {
                address_book,
                mut downloads,
            } => {
                while let Poll::Ready(Some(_)) = downloads.as_mut().poll_next(cx) {}

                result = Ok(());
                Setup::Initialized {
                    address_book,
                    downloads,
                }
            }
        };

        // Make sure we're leaving the network setup in a valid state
        if matches!(self.network_setup, Setup::FailedInit) {
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

    #[instrument(name = "inbound", skip(self, req))]
    fn call(&mut self, req: zn::Request) -> Self::Future {
        match req {
            zn::Request::Peers => {
                if let Setup::Initialized { address_book, .. } = &self.network_setup {
                    // # Security
                    //
                    // We could truncate the list to try to not reveal our entire
                    // peer set. But because we don't monitor repeated requests,
                    // this wouldn't actually achieve anything, because a crawler
                    // could just repeatedly query it.
                    //
                    // # Correctness
                    //
                    // Briefly hold the address book threaded mutex while
                    // cloning the address book. Then sanitize after releasing
                    // the lock.
                    let peers = address_book.lock().unwrap().clone();

                    // Send a sanitized response
                    let mut peers = peers.sanitized();
                    const MAX_ADDR: usize = 1000; // bitcoin protocol constant
                    peers.truncate(MAX_ADDR);
                    async { Ok(zn::Response::Peers(peers)) }.boxed()
                } else {
                    info!("ignoring `Peers` request from remote peer during network setup");
                    async { Ok(zn::Response::Nil) }.boxed()
                }
            }
            zn::Request::BlocksByHash(hashes) => {
                // Correctness:
                //
                // We can't use `call_all` here, because it can hold one buffer slot per concurrent
                // future, until the `CallAll` struct is dropped. We can't hold those slots in this
                // future because:
                // * we're not sure when the returned future will complete, and
                // * we don't limit how many returned futures can be concurrently running
                // https://github.com/tower-rs/tower/blob/master/tower/src/util/call_all/common.rs#L112
                use futures::stream::TryStreamExt;
                hashes
                    .into_iter()
                    .map(|hash| zs::Request::Block(hash.into()))
                    .map(|request| self.state.clone().oneshot(request))
                    .collect::<futures::stream::FuturesOrdered<_>>()
                    .try_filter_map(|response| async move {
                        Ok(match response {
                            zs::Response::Block(Some(block)) => Some(block),
                            // `zcashd` ignores missing blocks in GetData responses,
                            // rather than including them in a trailing `NotFound`
                            // message
                            zs::Response::Block(None) => None,
                            _ => unreachable!("wrong response from state"),
                        })
                    })
                    .try_collect::<Vec<_>>()
                    .map_ok(zn::Response::Blocks)
                    .boxed()
            }
            zn::Request::TransactionsByHash(_transactions) => {
                // `zcashd` returns a list of found transactions, followed by a
                // `NotFound` message if any transactions are missing. `zcashd`
                // says that Simplified Payment Verification (SPV) clients rely on
                // this behaviour - are there any of them on the Zcash network?
                // https://github.com/zcash/zcash/blob/e7b425298f6d9a54810cb7183f00be547e4d9415/src/main.cpp#L5632
                // We'll implement this request once we have a mempool:
                // https://en.bitcoin.it/wiki/Protocol_documentation#getdata
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::FindBlocks { known_blocks, stop } => {
                let request = zs::Request::FindBlockHashes { known_blocks, stop };
                self.state.clone().oneshot(request).map_ok(|resp| match resp {
                        zs::Response::BlockHashes(hashes) if hashes.is_empty() => zn::Response::Nil,
                        zs::Response::BlockHashes(hashes) => zn::Response::BlockHashes(hashes),
                        _ => unreachable!("zebra-state should always respond to a `FindBlockHashes` request with a `BlockHashes` response"),
                    })
                .boxed()
            }
            zn::Request::FindHeaders { known_blocks, stop } => {
                let request = zs::Request::FindBlockHeaders { known_blocks, stop };
                self.state.clone().oneshot(request).map_ok(|resp| match resp {
                        zs::Response::BlockHeaders(headers) if headers.is_empty() => zn::Response::Nil,
                        zs::Response::BlockHeaders(headers) => zn::Response::BlockHeaders(headers),
                        _ => unreachable!("zebra-state should always respond to a `FindBlockHeaders` request with a `BlockHeaders` response"),
                    })
                .boxed()
            }
            zn::Request::PushTransaction(_transaction) => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::AdvertiseTransactions(_transactions) => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::AdvertiseBlock(hash) => {
                if let Setup::Initialized { downloads, .. } = &mut self.network_setup {
                    downloads.download_and_verify(hash);
                } else {
                    info!(
                        ?hash,
                        "ignoring `AdvertiseBlock` request from remote peer during network setup"
                    );
                }
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::MempoolTransactions => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::Ping(_) => {
                unreachable!("ping requests are handled internally");
            }
        }
    }
}
