//! `listen` subcommand - test stub for talking to zcashd

use crate::prelude::*;

use abscissa_core::{Command, Options, Runnable};

/// `listen` subcommand
#[derive(Command, Debug, Options)]
pub struct ListenCmd {
    /// The address of the node to connect to.
    #[options(help = "The address to listen on.", default = "127.0.0.1:28233")]
    addr: std::net::SocketAddr,
}

impl Runnable for ListenCmd {
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
                match self.listen().await {
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

impl ListenCmd {
    async fn listen(&self) -> Result<(), failure::Error> {
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

        use tokio::net::{TcpListener, TcpStream};
        use tokio::prelude::*;

        let config = app_config().network.clone();
        let collector = TimestampCollector::new();
        let mut pc = Buffer::new(PeerConnector::new(config, node, &collector), 1);

        let mut listener = TcpListener::bind(self.addr).await?;

        loop {
            let (tcp_stream, addr) = listener.accept().await?;

            pc.ready()
                .await
                .map_err(failure::Error::from_boxed_compat)?;
            let mut client = pc
                .call((tcp_stream, addr))
                .await
                .map_err(failure::Error::from_boxed_compat)?;

            let addrs = match client.call(Request::GetPeers).await? {
                Response::Peers(addrs) => addrs,
                _ => bail!("Got wrong response type"),
            };
            info!(
                addrs.len = addrs.len(),
                "asked for addresses from remote peer"
            );
        }
        Ok(())
    }
}
