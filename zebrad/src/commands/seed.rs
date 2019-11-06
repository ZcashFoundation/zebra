//! `seed` subcommand - test stub for talking to zcashd

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::{config::ZebradConfig, prelude::*};

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};

/// `seed` subcommand
///
/// A DNS seeder command to spider and collect as many valid peer
/// addresses as we can.
#[derive(Command, Debug, Options)]
pub struct SeedCmd {
    /// Filter strings
    #[options(free)]
    filters: Vec<String>,
}

impl config::Override<ZebradConfig> for SeedCmd {
    // Process the given command line options, overriding settings
    // from a configuration file using explicit flags taken from
    // command-line arguments.
    fn override_config(&self, mut config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = self.filters.join(",");
        }

        Ok(config)
    }
}

impl Runnable for SeedCmd {
    /// Start the application.
    fn run(&self) {
        use crate::components::tokio::TokioComponent;

        let wait = tokio::future::pending::<()>();
        // Combine the seed future with an infinite wait
        // so that the program has to be explicitly killed and
        // won't die before all tracing messages are written.
        let fut = futures::future::join(
            async {
                match self.seed().await {
                    Ok(()) => {}
                    Err(e) => {
                        // Print any error that occurs.
                        error!(?e);
                    }
                }
            },
            wait,
        );

        let _ = app_reader()
            .state()
            .components
            .get_downcast_ref::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .block_on(fut);
    }
}

impl SeedCmd {
    async fn seed(&self) -> Result<(), failure::Error> {
        use failure::Error;
        use futures::stream::StreamExt;
        use tower::{buffer::Buffer, Service, ServiceExt};
        use zebra_network::{AddressBook, Request, Response};

        info!("begin tower-based peer handling test stub");

        struct SeedService {
            address_book: Option<AddressBook>,
        }

        impl SeedService {
            fn set_address_book(&mut self, address_book: AddressBook) {
                self.address_book = Some(address_book);
            }
        }

        impl Service<Request> for SeedService {
            type Response = Response;
            type Error = Error;
            type Future =
                Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Ok(()).into()
            }

            fn call(&mut self, req: Request) -> Self::Future {
                let response = match req {
                    Request::GetPeers => match &self.address_book {
                        Some(address_book) => Ok::<Response, failure::Error>(Response::Peers(
                            address_book.peers().collect(),
                        )),
                        _ => Ok::<Response, failure::Error>(Response::Ok),
                    },
                    _ => Ok::<Response, failure::Error>(Response::Ok),
                };

                return Box::pin(futures::future::ready(response));
            }
        }

        let node = Buffer::new(SeedService { address_book: None }, 1);

        let config = app_config().network.clone();

        // XXX How do I create a service above that answers questions
        // about this specific address book?
        let (mut peer_set, address_book) = zebra_network::init(config, node).await;

        // XXX Do not tell our DNS seed queries about gossiped addrs
        // that we have not connected to before?
        info!("waiting for peer_set ready");
        peer_set.ready().await.map_err(Error::from_boxed_compat)?;

        info!("peer_set became ready");

        let eternity = tokio::future::pending::<()>();
        eternity.await;

        Ok(())
    }
}
