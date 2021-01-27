use std::{
    future::Future,
    mem,
    pin::Pin,
    sync::{Arc, Mutex},
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

pub type NetworkSetupData = (Outbound, Arc<Mutex<AddressBook>>);

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
        address_book: Arc<Mutex<zn::AddressBook>>,

        /// A `futures::Stream` that downloads and verifies gossipped blocks.
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
    ///
    /// We keep hold of the closed oneshot, so we can use it to create a
    /// new error for each `poll_ready` call.
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
    network: Setup,

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
            network: Setup::AwaitingNetwork {
                network_setup,
                verifier,
            },
            state,
        }
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
        if matches!(self.network, Setup::AwaitingNetwork { .. }) {
            // Unfortunately, we can't match, swap, and destructure at the same time
            let mut awaiting_state = Setup::FailedInit;
            mem::swap(&mut self.network, &mut awaiting_state);
            if let Setup::AwaitingNetwork {
                mut network_setup,
                verifier,
            } = awaiting_state
            {
                match network_setup.try_recv() {
                    Ok((outbound, address_book)) => {
                        let downloads = Box::pin(Downloads::new(
                            Timeout::new(outbound, BLOCK_DOWNLOAD_TIMEOUT),
                            Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT),
                            self.state.clone(),
                        ));
                        self.network = Setup::Initialized {
                            address_book,
                            downloads,
                        };
                    }
                    Err(TryRecvError::Empty) => {
                        // There's no setup data yet, so keep waiting for it
                        self.network = Setup::AwaitingNetwork {
                            network_setup,
                            verifier,
                        };
                    }
                    Err(error @ TryRecvError::Closed) => {
                        // Mark the service as failed, because network setup failed
                        error!(?error, "inbound network setup failed");
                        let error: SharedRecvError = error.into();
                        self.network = Setup::FailedRecv {
                            error: error.clone(),
                        };
                        return Poll::Ready(Err(error.into()));
                    }
                }
            }
        }

        // Unfortunately, we can't combine these matches into an exhaustive match statement,
        // because they use mutable references, or they depend on the state we've just modified.

        // Make sure we left the network setup in a valid state
        if matches!(self.network, Setup::FailedInit) {
            unreachable!("incomplete Inbound initialization");
        }

        // If network setup failed, report service failure
        if let Setup::FailedRecv { error } = &mut self.network {
            return Poll::Ready(Err(error.clone().into()));
        }

        // Clean up completed download tasks, ignoring their results
        if let Setup::Initialized { downloads, .. } = &mut self.network {
            while let Poll::Ready(Some(_)) = downloads.as_mut().poll_next(cx) {}
        }

        // TODO:
        //  * do we want to propagate backpressure from the download queue or its outbound network?
        //    currently, the download queue waits for the outbound network in the download future,
        //    and drops new requests after it reaches a hard-coded limit. This is the
        //    "load shed directly" pattern from #1618.
        //  * currently, the state service is always ready, unless its buffer is full.
        //    So we might also want to propagate backpressure from its buffer.
        //  * if we want to propagate backpressure, add a ReadyCache for each service, to ensure
        //    that each poll_ready has a matching call. See #1593 for details.
        Poll::Ready(Ok(()))
    }

    #[instrument(name = "inbound", skip(self, req))]
    fn call(&mut self, req: zn::Request) -> Self::Future {
        match req {
            zn::Request::Peers => {
                if let Setup::Initialized { address_book, .. } = &self.network {
                    // We could truncate the list to try to not reveal our entire
                    // peer set. But because we don't monitor repeated requests,
                    // this wouldn't actually achieve anything, because a crawler
                    // could just repeatedly query it.
                    let mut peers = address_book.lock().unwrap().sanitized();
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
                // We can't use `call_all` here, because it leaks buffer slots:
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
                if let Setup::Initialized { downloads, .. } = &mut self.network {
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
