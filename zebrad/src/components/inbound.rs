use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use abscissa_core::{Component, FrameworkError};
use futures::{channel::oneshot, prelude::*};
use tower::{buffer::Buffer, util::BoxService, Service};

use zebra_network::{AddressBook, BoxedStdError, Request, Response};

/// Sets up the service that responds to inbound network requests.
#[derive(Component)]
pub struct Inbound {
    svc: Buffer<BoxService<Request, Response, BoxedStdError>, Request>,
    address_book_sender: Option<oneshot::Sender<Arc<Mutex<AddressBook>>>>,
}

impl std::fmt::Debug for Inbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inbound").finish()
    }
}

impl Default for Inbound {
    fn default() -> Self {
        let (tx, rx) = oneshot::channel();
        Self {
            svc: Buffer::new(
                BoxService::new(InboundService {
                    state: State::AwaitingAddressBook(rx),
                }),
                10,
            ),
            address_book_sender: Some(tx),
        }
    }
}

impl Inbound {
    /// Get a cloneable handle for the inbound service.
    pub fn svc(&self) -> Buffer<BoxService<Request, Response, BoxedStdError>, Request> {
        self.svc.clone()
    }

    /// Get a handle to send a shared address book to the inbound service.
    ///
    /// Constructing the inbound and outbound services is awkward because they
    /// have a cyclic dependency: the peer set (the outbound service) needs a
    /// handle for the inbound service to forward requests from peers, but the
    /// inbound service needs the addressbook from the outbound service to
    /// respond to requests for peers. This method provides a way to send the
    /// shared address book from the peer set to the inbound service after the
    /// peer set has finished (async) initialization.
    pub fn address_book_sender(&mut self) -> Option<oneshot::Sender<Arc<Mutex<AddressBook>>>> {
        self.address_book_sender.take()
    }
}

#[derive(Debug)]
struct InboundService {
    state: State,
}

/// TODO: we may need to revisit this as we hook more components into
/// the inbound service, but only the peer set causes an (async) cyclic dependency.
#[derive(Debug)]
enum State {
    /// Waiting for the address book to be shared with us via the oneshot channel.
    AwaitingAddressBook(oneshot::Receiver<Arc<Mutex<AddressBook>>>),
    /// Address book received, ready to service requests.
    Ready(Arc<Mutex<AddressBook>>),
}

impl Service<Request> for InboundService {
    type Response = Response;
    type Error = BoxedStdError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.state {
            State::Ready(_) => Poll::Ready(Ok(())),
            State::AwaitingAddressBook(ref mut rx) => match rx.try_recv() {
                Err(e) => {
                    error!("oneshot sender dropped, failing service: {:?}", e);
                    Poll::Ready(Err(e.into()))
                }
                Ok(None) => {
                    trace!("awaiting address book, service is unready");
                    Poll::Pending
                }
                Ok(Some(address_book)) => {
                    debug!("received address_book via oneshot, service becomes ready");
                    self.state = State::Ready(address_book);
                    Poll::Ready(Ok(()))
                }
            },
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let address_book = if let State::Ready(address_book) = &self.state {
            address_book
        } else {
            unreachable!("InboundService::call without InboundService::poll_ready");
        };

        let response = match req {
            Request::Peers => {
                let mut peers = address_book.lock().unwrap().sanitized();
                // Truncate the list so that we do not trivially
                // reveal our entire peer set.
                peers.truncate(50);
                debug!(peers.len = peers.len());
                Ok(Response::Peers(peers))
            }
            _ => {
                debug!("ignoring request");
                Ok(Response::Nil)
            }
        };
        Box::pin(futures::future::ready(response))
    }
}
