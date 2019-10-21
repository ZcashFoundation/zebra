//! `connect` subcommand - test stub for talking to zcashd

use crate::prelude::*;

use abscissa_core::{Command, Options, Runnable};

/// `connect` subcommand
#[derive(Command, Debug, Options)]
pub struct ConnectCmd {
    /// The address of the node to connect to.
    #[options(
        help = "The address of the node to connect to.",
        default = "127.0.0.1:8233"
    )]
    addr: std::net::SocketAddr,
}

impl Runnable for ConnectCmd {
    /// Start the application.
    fn run(&self) {
        info!(connect.addr = ?self.addr);

        use crate::components::tokio::TokioComponent;

        let wait = tokio::future::pending::<()>();
        // Combine the connect future with an infinite wait
        // so that the program has to be explicitly killed and
        // won't die before all tracing messages are written.
        let fut = futures::future::join(
            async {
                match self.connect().await {
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

impl ConnectCmd {
    async fn connect(&self) -> Result<(), failure::Error> {
        use zebra_network::{AddressBook, Request, Response};

        info!("begin tower-based peer handling test stub");
        use tower::{buffer::Buffer, service_fn, Service, ServiceExt};

        let node = Buffer::new(
            service_fn(|req| {
                async move {
                    info!(?req);
                    Ok::<Response, failure::Error>(Response::Ok)
                }
            }),
            1,
        );

        let mut config = app_config().network.clone();

        config.initial_peers = vec![self.addr];

        let (mut peer_set, address_book) = zebra_network::init(config, node);

        info!("waiting for peer_set ready");
        peer_set.ready().await.map_err(Error::from_boxed_compat)?;

        info!("peer_set became ready, constructing addr requests");

        use failure::Error;
        use futures::stream::{FuturesUnordered, StreamExt};

        let mut addr_reqs = FuturesUnordered::new();
        for i in 0..10usize {
            info!(i, "awaiting peer_set ready");
            peer_set.ready().await.map_err(Error::from_boxed_compat)?;
            info!(i, "calling peer_set");
            addr_reqs.push(peer_set.call(Request::GetPeers));
        }

        use tracing::Level;
        let mut all_addrs = AddressBook::new(span!(Level::TRACE, "connect stub addressbook"));
        while let Some(Ok(Response::Peers(addrs))) = addr_reqs.next().await {
            info!(addrs.len = addrs.len(), "got address response");

            let prev_count = all_addrs.peers().count();
            all_addrs.extend(addrs.into_iter());
            let count = all_addrs.peers().count();
            info!(
                new_addrs = count - prev_count,
                count, "added addrs to addressbook"
            );
        }

        let addrs = all_addrs.drain_newest().collect::<Vec<_>>();

        info!(addrs.len = addrs.len(), ab.len = all_addrs.peers().count());
        let mut head = Vec::new();
        head.extend_from_slice(&addrs[0..5]);
        let mut tail = Vec::new();
        tail.extend_from_slice(&addrs[addrs.len() - 5..]);
        info!(addrs.first = ?head, addrs.last = ?tail);

        loop {
            // empty loop ensures we don't exit the application,
            // and this is throwaway code
        }

        Ok(())
    }
}
