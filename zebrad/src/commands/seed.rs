//! `seed` subcommand - runs a dns seeder

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use futures::channel::oneshot;
use tower::{buffer::Buffer, Service, ServiceExt};
use zebra_network::{AddressBook, BoxedStdError, Request, Response};

use crate::{config::ZebradConfig, prelude::*};

/// Whether our `SeedService` is poll_ready or not.
#[derive(Debug)]
enum SeederState {
    /// Waiting for the address book to be shared with us via the oneshot channel.
    AwaitingAddressBook(oneshot::Receiver<Arc<Mutex<AddressBook>>>),
    /// Address book received, ready to service requests.
    Ready(Arc<Mutex<AddressBook>>),
}

#[derive(Debug)]
struct SeedService {
    state: SeederState,
}

impl Service<Request> for SeedService {
    type Response = Response;
    type Error = BoxedStdError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        debug!("SeedService.state: {:?}", self.state);

        match self.state {
            SeederState::Ready(_) => return Poll::Ready(Ok(())),
            SeederState::AwaitingAddressBook(ref mut rx) => match rx.try_recv() {
                Err(e) => {
                    error!("SeedService oneshot sender dropped: {:?}", e);
                    return Poll::Ready(Err(e.into()));
                }
                Ok(None) => {
                    trace!("SeedService hasn't received a message via the oneshot yet.");
                    return Poll::Pending;
                }
                Ok(Some(address_book)) => {
                    info!(
                        "SeedService received address_book via oneshot {:?}",
                        address_book
                    );
                    self.state = SeederState::Ready(address_book);
                    return Poll::Ready(Ok(()));
                }
            },
        }
    }

    fn call(&mut self, req: Request) -> Self::Future {
        info!("SeedService handling a request: {:?}", req);

        let address_book = if let SeederState::Ready(address_book) = &self.state {
            address_book
        } else {
            panic!("SeedService::call without SeedService::poll_ready");
        };

        let response = match req {
            Request::GetPeers => {
                debug!(address_book.len = address_book.lock().unwrap().len());
                info!("SeedService responding to GetPeers");
                Ok::<Response, Self::Error>(Response::Peers(
                    address_book.lock().unwrap().peers().collect(),
                ))
            }
            _ => Ok::<Response, Self::Error>(Response::Ok),
        };

        info!("SeedService response: {:?}", response);
        return Box::pin(futures::future::ready(response));
    }
}

/// `seed` subcommand
///
/// A DNS seeder command to spider and collect as many valid peer
/// addresses as we can.
// This is not a unit-like struct because it makes Command and Options sad.
#[derive(Command, Debug, Default, Options)]
pub struct SeedCmd {}

impl Runnable for SeedCmd {
    /// Start the application.
    fn run(&self) {
        use crate::components::tokio::TokioComponent;

        let _ = app_reader()
            .state()
            .components
            .get_downcast_ref::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .block_on(self.seed());
    }
}

impl SeedCmd {
    async fn seed(&self) -> Result<(), failure::Error> {
        use failure::Error;

        info!("begin tower-based peer handling test stub");

        let (addressbook_tx, addressbook_rx) = oneshot::channel();
        let seed_service = SeedService {
            state: SeederState::AwaitingAddressBook(addressbook_rx),
        };
        let node = Buffer::new(seed_service, 1);

        let config = app_config().network.clone();

        let (mut peer_set, address_book) = zebra_network::init(config, node).await;

        let _ = addressbook_tx.send(address_book);

        info!("waiting for peer_set ready");
        peer_set.ready().await.map_err(Error::from_boxed_compat)?;

        info!("peer_set became ready");

        #[cfg(dos)]
        use std::time::Duration;
        use tokio::timer::Interval;

        #[cfg(dos)]
        // Fire GetPeers requests at ourselves, for testing.
        tokio::spawn(async move {
            let mut interval_stream = Interval::new_interval(Duration::from_secs(1));

            loop {
                interval_stream.next().await;

                let _ = seed_service.call(Request::GetPeers);
            }
        });

        let eternity = tokio::future::pending::<()>();
        eternity.await;

        Ok(())
    }
}
