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
        use zebra_network::{
            peer::PeerConnector,
            peer_set::PeerSet,
            protocol::internal::{Request, Response},
            timestamp_collector::TimestampCollector,
            Network,
        };

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

        use tokio::net::TcpStream;

        let config = app_config().network.clone();
        let collector = TimestampCollector::new();
        let mut pc = PeerConnector::new(config, Network::Mainnet, node, &collector);

        let tcp_stream = TcpStream::connect(self.addr).await?;
        pc.ready().await?;
        let mut client = pc.call((tcp_stream, self.addr)).await?;

        client.ready().await?;

        let addrs = match client.call(Request::GetPeers).await? {
            Response::Peers(addrs) => addrs,
            _ => bail!("Got wrong response type"),
        };
        info!(
            addrs.len = addrs.len(),
            "got addresses from first connected peer"
        );

/*
        use failure::Error;
        use futures::{
            future,
            stream::{FuturesUnordered, StreamExt},
        };
        use std::time::Duration;
        use tower::discover::{Change, ServiceStream};
        use tower_load::{peak_ewma::PeakEwmaDiscover, NoInstrument};

        // construct a stream of services
        let client_stream = PeakEwmaDiscover::new(
            ServiceStream::new(
                addrs
                    .into_iter()
                    .map(|meta| {
                        let svc_fut = pc.call(meta.addr);
                        async move { Ok::<_, Error>(Change::Insert(meta.addr, svc_fut.await?)) }
                    })
                    .collect::<FuturesUnordered<_>>()
                    // Discard any errored connections...
                    .filter(|result| future::ready(result.is_ok())),
            ),
            Duration::from_secs(1),  // default rtt estimate
            Duration::from_secs(60), // decay time
            NoInstrument,
        );

        info!("finished constructing discover");

        let mut peer_set = PeerSet::new(client_stream);

        info!("waiting for peer_set ready");
        peer_set.ready().await.map_err(Error::from_boxed_compat)?;

        info!("peer_set became ready, constructing addr requests");

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
        */

        Ok(())
    }
}
