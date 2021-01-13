use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures::{
    future::{FutureExt, TryFutureExt},
    stream::{Stream, TryStreamExt},
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
    // invariant: address_book, outbound, downloads are Some if network_setup is None
    //
    // why not use an enum for the inbound state? because it would mean
    // match-wrapping the body of Service::call rather than just expect()ing
    // some Options.

    // Setup
    /// A oneshot channel used to receive the address_book and outbound services
    /// after the network is set up.
    network_setup: Option<oneshot::Receiver<SetupData>>,

    // Services
    /// A list of peer addresses.
    address_book: Option<Arc<Mutex<zn::AddressBook>>>,

    /// A service that downloads and verifies gossipped blocks.
    downloads: Option<Pin<Box<Downloads<Timeout<Outbound>, Timeout<Verifier>, State>>>>,

    /// A service that forwards requests to connected peers, and returns their
    /// responses.
    ///
    /// Only used for readiness checks, and via `downloads`.
    outbound: Option<Outbound>,

    /// A service that manages cached blockchain state.
    state: State,

    /// A service that verifies downloaded blocks.
    ///
    /// Only used for readiness checks, and via `downloads`.
    verifier: Verifier,
}

impl Inbound {
    pub fn new(
        network_setup: oneshot::Receiver<SetupData>,
        state: State,
        verifier: Verifier,
    ) -> Self {
        Self {
            network_setup: Some(network_setup),
            address_book: None,
            downloads: None,
            outbound: None,
            state,
            verifier,
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
                    self.outbound = Some(outbound.clone());
                    self.address_book = Some(address_book);
                    self.network_setup = None;
                    self.downloads = Some(Box::pin(Downloads::new(
                        Timeout::new(outbound, BLOCK_DOWNLOAD_TIMEOUT),
                        Timeout::new(self.verifier.clone(), BLOCK_VERIFY_TIMEOUT),
                        self.state.clone(),
                    )));
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

        // Clean up completed download tasks
        if let Some(downloads) = self.downloads.as_mut() {
            while let Poll::Ready(Some(_)) = downloads.as_mut().poll_next(cx) {}
        }

        // Now report readiness based on readiness of the inner services, if they're available.
        // XXX do we want to propagate backpressure from the network here?
        match (
            self.state.poll_ready(cx),
            self.outbound
                .as_mut()
                .map(|svc| svc.poll_ready(cx))
                .unwrap_or(Poll::Ready(Ok(()))),
        ) {
            (Poll::Ready(Err(e)), _) | (_, Poll::Ready(Err(e))) => Poll::Ready(Err(e)),
            (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
            (Poll::Ready(Ok(())), Poll::Ready(Ok(()))) => Poll::Ready(Ok(())),
        }
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
                None => async { Err("not ready to serve addresses".into()) }.boxed(),
            },
            zn::Request::BlocksByHash(hashes) => {
                let state = self.state.clone();
                let requests = futures::stream::iter(
                    hashes
                        .into_iter()
                        .map(|hash| zs::Request::Block(hash.into())),
                );

                state
                    .call_all(requests)
                    .try_filter_map(|rsp| {
                        futures::future::ready(match rsp {
                            zs::Response::Block(Some(block)) => Ok(Some(block)),
                            // `zcashd` ignores missing blocks in GetData responses,
                            // rather than including them in a trailing `NotFound`
                            // message
                            zs::Response::Block(None) => Ok(None),
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
                self.state.call(request).map_ok(|resp| match resp {
                        zs::Response::BlockHashes(hashes) if hashes.is_empty() => zn::Response::Nil,
                        zs::Response::BlockHashes(hashes) => zn::Response::BlockHashes(hashes),
                        _ => unreachable!("zebra-state should always respond to a `FindBlockHashes` request with a `BlockHashes` response"),
                    })
                .boxed()
            }
            zn::Request::FindHeaders { known_blocks, stop } => {
                let request = zs::Request::FindBlockHeaders { known_blocks, stop };
                self.state.call(request).map_ok(|resp| match resp {
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
