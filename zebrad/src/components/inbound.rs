use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures::{
    future::{FutureExt, TryFutureExt},
    stream::Stream,
};
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

pub type SetupData = (Outbound, Arc<Mutex<AddressBook>>);

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
    // invariants:
    //  * Before setup: address_book and downloads are None, and the *_setup members are Some
    //  * After setup: address_book and downloads are Some, and the *_setup members are None
    //
    // why not use an enum for the inbound state? because it would mean
    // match-wrapping the body of Service::call rather than just expect()ing
    // some Options.

    // Setup
    /// A oneshot channel used to receive the address_book and outbound services
    /// after the network is set up.
    ///
    /// `None` after the network is set up.
    network_setup: Option<oneshot::Receiver<SetupData>>,

    /// A service that verifies downloaded blocks. Given to `downloads`
    /// after the network is set up.
    ///
    /// `None` after the network is set up and `downloads` is created.
    verifier_setup: Option<Verifier>,

    // Services and Data Stores
    /// A shared list of peer addresses.
    ///
    /// `None` until the network is set up.
    address_book: Option<Arc<Mutex<zn::AddressBook>>>,

    /// A stream that downloads and verifies gossipped blocks.
    ///
    /// `None` until the network is set up.
    downloads: Option<Pin<Box<Downloads<Timeout<Outbound>, Timeout<Verifier>, State>>>>,

    /// A service that manages cached blockchain state.
    state: State,
}

impl Inbound {
    pub fn new(
        network_setup: oneshot::Receiver<SetupData>,
        state: State,
        verifier: Verifier,
    ) -> Self {
        Self {
            network_setup: Some(network_setup),
            verifier_setup: Some(verifier),
            address_book: None,
            downloads: None,
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
        if let Some(mut rx) = self.network_setup.take() {
            use oneshot::error::TryRecvError;
            match rx.try_recv() {
                Ok((outbound, address_book)) => {
                    let verifier = self
                        .verifier_setup
                        .take()
                        .expect("unexpected missing verifier during inbound network setup");

                    self.address_book = Some(address_book);
                    self.downloads = Some(Box::pin(Downloads::new(
                        Timeout::new(outbound, BLOCK_DOWNLOAD_TIMEOUT),
                        Timeout::new(verifier, BLOCK_VERIFY_TIMEOUT),
                        self.state.clone(),
                    )));

                    self.network_setup = None;
                }
                Err(TryRecvError::Empty) => {
                    self.network_setup = Some(rx);
                }
                Err(e @ TryRecvError::Closed) => {
                    // In this case, report that the service failed, and put the
                    // failed oneshot back so we'll fail again in case
                    // poll_ready is called after failure.
                    self.network_setup = Some(rx);
                    return Poll::Ready(Err(e.into()));
                }
            };
        }

        // Clean up completed download tasks, ignoring their results
        if let Some(downloads) = self.downloads.as_mut() {
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
            zn::Request::Peers => match self.address_book.as_ref() {
                Some(addrs) => {
                    // We could truncate the list to try to not reveal our entire
                    // peer set. But because we don't monitor repeated requests,
                    // this wouldn't actually achieve anything, because a crawler
                    // could just repeatedly query it.
                    let mut peers = addrs.lock().unwrap().sanitized();
                    const MAX_ADDR: usize = 1000; // bitcoin protocol constant
                    peers.truncate(MAX_ADDR);
                    async { Ok(zn::Response::Peers(peers)) }.boxed()
                }
                None => {
                    info!("ignoring `Peers` request from remote peer during network setup");
                    async { Ok(zn::Response::Nil) }.boxed()
                }
            },
            zn::Request::BlocksByHash(hashes) => {
                // Correctness:
                //
                // We can't use `call_all` here, because it leaks buffer slots:
                // https://github.com/tower-rs/tower/blob/master/tower/src/util/call_all/common.rs#L112
                let mut state = self.state.clone();
                async move {
                    let mut blocks = Vec::new();
                    for hash in hashes {
                        let request = zs::Request::Block(hash.into());
                        // we can't use ServiceExt::oneshot here, due to lifetime issues
                        match state.ready_and().await?.call(request).await? {
                            zs::Response::Block(Some(block)) => blocks.push(block),
                            // `zcashd` ignores missing blocks in GetData responses,
                            // rather than including them in a trailing `NotFound`
                            // message
                            zs::Response::Block(None) => {}
                            _ => unreachable!("wrong response from state"),
                        }
                    }
                    Ok(zn::Response::Blocks(blocks))
                }
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
                if let Some(downloads) = self.downloads.as_mut() {
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
