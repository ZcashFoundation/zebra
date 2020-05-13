//! `seed` subcommand - runs a dns seeder

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use abscissa_core::{Command, Options, Runnable};
use futures::{channel::oneshot, prelude::*};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_network::{AddressBook, BoxedStdError, Request, Response};

use crate::prelude::*;
use color_eyre::Report;
use eyre::eyre;

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

    #[instrument(skip(self, _cx))]
    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.state {
            SeederState::Ready(_) => Poll::Ready(Ok(())),
            SeederState::AwaitingAddressBook(ref mut rx) => match rx.try_recv() {
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
                    self.state = SeederState::Ready(address_book);
                    Poll::Ready(Ok(()))
                }
            },
        }
    }

    // Note: the generated span applies only to this function, not
    // to the future, but this is OK because the current implementation
    // is not actually async.
    #[instrument]
    fn call(&mut self, req: Request) -> Self::Future {
        let address_book = if let SeederState::Ready(address_book) = &self.state {
            address_book
        } else {
            panic!("SeedService::call without SeedService::poll_ready");
        };

        let response = match req {
            Request::Peers => {
                // Collect a list of known peers from the address book
                // and sanitize their timestamps.
                let mut peers = address_book
                    .lock()
                    .unwrap()
                    .peers()
                    .map(|addr| addr.sanitize())
                    .collect::<Vec<_>>();
                // The peers are still ordered by recency, so shuffle them.
                use rand::seq::SliceRandom;
                peers.shuffle(&mut rand::thread_rng());
                // Finally, truncate the list so that we do not trivially
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

        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .block_on(self.seed())
            // Surface any error that occurred executing the future.
            .unwrap();
    }
}

impl SeedCmd {
    async fn seed(&self) -> Result<(), Report> {
        info!("begin tower-based peer handling test stub");

        let (addressbook_tx, addressbook_rx) = oneshot::channel();
        let mut seed_service = SeedService {
            state: SeederState::AwaitingAddressBook(addressbook_rx),
        };
        let node = Buffer::new(seed_service, 1);

        let config = app_config().network.clone();

        let (mut peer_set, address_book) = zebra_network::init(config, node).await;

        let _ = addressbook_tx.send(address_book);

        info!("waiting for peer_set ready");
        peer_set.ready_and().await.map_err(|e| eyre!(e))?;

        info!("peer_set became ready");

        #[cfg(feature = "dos")]
        use std::time::Duration;

        #[cfg(feature = "dos")]
        // Fire GetPeers requests at ourselves, for testing.
        tokio::spawn(async move {
            let mut interval_stream = tokio::time::interval(Duration::from_secs(1));

            loop {
                interval_stream.next().await;

                let _ = seed_service.call(Request::Peers);
            }
        });

        let eternity = future::pending::<()>();
        eternity.await;

        Ok(())
    }
}
