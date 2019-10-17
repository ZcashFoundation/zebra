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
        use zebra_network::{Request, Response, TimestampCollector};

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

        // Until we finish fleshing out the peerset -- particularly
        // pulling more peers -- we don't want to start with a single
        // initial peer.  So make a throwaway connection to the first,
        // extract a list of addresses, and discard everything else.
        // All the setup is kept in a sub-scope so we know we're not reusing it.
        //
        // Later, this should turn into initial_peers = vec![self.addr];
        config.initial_peers = {
            use tokio::net::TcpStream;
            use zebra_network::should_be_private::PeerConnector;

            let collector = TimestampCollector::new();
            let mut pc = Buffer::new(
                PeerConnector::new(config.clone(), node.clone(), &collector),
                1,
            );

            let tcp_stream = TcpStream::connect(self.addr).await?;
            pc.ready()
                .await
                .map_err(failure::Error::from_boxed_compat)?;
            let mut client = pc
                .call((tcp_stream, self.addr))
                .await
                .map_err(failure::Error::from_boxed_compat)?;

            client.ready().await?;

            let addrs = match client.call(Request::GetPeers).await? {
                Response::Peers(addrs) => addrs,
                _ => bail!("Got wrong response type"),
            };
            info!(
                addrs.len = addrs.len(),
                "got addresses from first connected peer"
            );

            addrs.into_iter().map(|meta| meta.addr).collect::<Vec<_>>()
        };

        let (mut peer_set, _tc) = zebra_network::init(config, node);

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

        let mut all_addrs = Vec::new();
        while let Some(Ok(Response::Peers(addrs))) = addr_reqs.next().await {
            info!(
                all_addrs.len = all_addrs.len(),
                addrs.len = addrs.len(),
                "got address response"
            );
            all_addrs.extend(addrs);
        }

        loop {
            // empty loop ensures we don't exit the application,
            // and this is throwaway code
        }

        Ok(())
    }
}
