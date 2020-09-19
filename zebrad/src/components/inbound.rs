use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures::{
    future::{FutureExt, TryFutureExt},
    stream::TryStreamExt,
};
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service, ServiceExt};

use zebra_network as zn;
use zebra_network::AddressBook;
use zebra_state as zs;

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;
type State = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;

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
}

impl Inbound {
    pub fn new(network_setup: oneshot::Receiver<SetupData>, state: State) -> Self {
        Self {
            network_setup: Some(network_setup),
            outbound: None,
            address_book: None,
            state,
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
        use oneshot::error::TryRecvError;
        match self.network_setup.take() {
            // If we're waiting for setup, check if we became ready
            Some(mut rx) => match rx.try_recv() {
                Ok((outbound, address_book)) => {
                    self.outbound = Some(outbound);
                    self.address_book = Some(address_book);
                    self.network_setup = None;
                    Poll::Ready(Ok(()))
                }
                Err(e @ TryRecvError::Closed) => {
                    // returning poll_ready(err) means that poll_ready should
                    // never be called again, but put the oneshot back so we
                    // error again in case someone does.
                    self.network_setup = Some(rx);
                    Poll::Ready(Err(e.into()))
                }
                Err(TryRecvError::Empty) => {
                    self.network_setup = Some(rx);
                    Poll::Pending
                }
            },
            // Otherwise, check readiness of services we might call to propagate backpressure.
            None => {
                match (
                    self.state.poll_ready(cx),
                    self.outbound.as_mut().unwrap().poll_ready(cx),
                ) {
                    (Poll::Ready(Err(e)), _) | (_, Poll::Ready(Err(e))) => Poll::Ready(Err(e)),
                    (Poll::Pending, _) | (_, Poll::Pending) => Poll::Pending,
                    (Poll::Ready(Ok(())), Poll::Ready(Ok(()))) => Poll::Ready(Ok(())),
                }
            }
        }
    }

    #[instrument(skip(self))]
    fn call(&mut self, req: zn::Request) -> Self::Future {
        match req {
            zn::Request::Peers => {
                // We could truncate the list to try to not reveal our entire
                // peer set. But because we don't monitor repeated requests,
                // this wouldn't actually achieve anything, because a crawler
                // could just repeatedly query it.
                let mut peers = self
                    .address_book
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .sanitized();
                const MAX_ADDR: usize = 1000; // bitcoin protocol constant
                peers.truncate(MAX_ADDR);
                async { Ok(zn::Response::Peers(peers)) }.boxed()
            }
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
            zn::Request::AdvertiseBlock(_block) => {
                debug!("ignoring unimplemented request");
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
