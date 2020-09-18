use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use futures::future::FutureExt;
use tokio::sync::oneshot;
use tower::{buffer::Buffer, util::BoxService, Service};

use zebra_network as zn;
use zebra_network::AddressBook;

type Outbound = Buffer<BoxService<zn::Request, zn::Response, zn::BoxError>, zn::Request>;

pub type SetupData = (Outbound, Arc<Mutex<AddressBook>>);

pub struct Inbound {
    // invariant: outbound, address_book are Some if network_setup is None
    //
    // why not use an enum for the inbound state? because it would mean
    // match-wrapping the body of Service::call rather than just expect()ing
    // some Options.
    network_setup: Option<oneshot::Receiver<SetupData>>,
    outbound: Option<Outbound>,
    address_book: Option<Arc<Mutex<zn::AddressBook>>>,
}

impl Inbound {
    pub fn new(network_setup: oneshot::Receiver<SetupData>) -> Self {
        Self {
            network_setup: Some(network_setup),
            outbound: None,
            address_book: None,
        }
    }
}

impl Service<zn::Request> for Inbound {
    type Response = zn::Response;
    type Error = zn::BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    #[instrument(skip(self, _cx))]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        use oneshot::error::TryRecvError;
        match self.network_setup.take() {
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
            None => Poll::Ready(Ok(())),
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
            _ => {
                debug!("ignoring unimplemented request");
                async { Ok(zn::Response::Nil) }.boxed()
            }
        }
    }
}
