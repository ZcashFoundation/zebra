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
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_chain as zc;
use zebra_consensus as zcon;
use zebra_network as zn;
use zebra_network::AddressBook;
use zebra_state as zs;

mod downloads;
use downloads::Downloads;

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;
type Verifier = Buffer<
    BoxService<Arc<zc::block::Block>, zc::block::Hash, zcon::chain::VerifyChainError>,
    Arc<zc::block::Block>,
>;

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
    // invariant: outbound, address_book are Some if network_setup is None
    //
    // why not use an enum for the inbound state? because it would mean
    // match-wrapping the body of Service::call rather than just expect()ing
    // some Options.
    network_setup: Option<oneshot::Receiver<SetupData>>,
    outbound: Option<Outbound>,
    address_book: Option<Arc<Mutex<zn::AddressBook>>>,
    state: State,
    verifier: Verifier,
    downloads: Option<Downloads<Outbound, Verifier, State>>,
}

impl Inbound {
    pub fn new(
        network_setup: oneshot::Receiver<SetupData>,
        state: State,
        verifier: Verifier,
    ) -> Self {
        Self {
            network_setup: Some(network_setup),
            outbound: None,
            address_book: None,
            state,
            verifier,
            downloads: None,
        }
    }
}

impl Service<zn::Request> for Inbound {
    type Response = zn::Response;
    type Error = zn::BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    #[instrument(skip(self, cx))]
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
                    self.outbound = Some(outbound);
                    self.address_book = Some(address_book);
                    self.network_setup = None;
                    self.downloads = Some(Downloads::new(
                        self.outbound.clone().unwrap(),
                        self.verifier.clone(),
                        self.state.clone(),
                    ));
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
            while let Poll::Ready(Some(_)) = Pin::new(downloads).poll_next(cx) {}
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

    #[instrument(skip(self))]
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
                            // XXX: check how zcashd handles missing blocks?
                            zs::Response::Block(None) => Err("missing block".into()),
                            _ => unreachable!("wrong response from state"),
                        })
                    })
                    .try_collect::<Vec<_>>()
                    .map_ok(zn::Response::Blocks)
                    .boxed()
            }
            zn::Request::TransactionsByHash(_transactions) => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::FindBlocks { .. } => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
            zn::Request::FindHeaders { .. } => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
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
                // this sucks
                let mut downloads = self.downloads.take().unwrap();
                self.downloads = Some(Downloads::new(
                    self.outbound.as_ref().unwrap().clone(),
                    self.verifier.clone(),
                    self.state.clone(),
                ));

                async move {
                    if downloads.download_and_verify(hash).await? {
                        tracing::info!(?hash, "queued download and verification of gossiped block");
                    } else {
                        tracing::debug!(?hash, "gossiped block already queued or verified");
                    }

                    Ok(zn::Response::Nil)
                }
                .boxed()
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
