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
        let fut = futures_util::future::join(
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
        use chrono::Utc;
        use tokio::{codec::Framed, net::TcpStream, prelude::*};

        use zebra_chain::types::BlockHeight;
        use zebra_network::{
            constants, peer,
            protocol::{
                codec::*,
                internal::{Request, Response},
                message::*,
                types::*,
            },
            timestamp_collector::TimestampCollector,
            Network,
        };

        info!("tower stub");

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

        let collector = TimestampCollector::new();

        let mut pc = peer::connector::PeerConnector::new(Network::Mainnet, node, &collector);
        // no need to call ready because pc is always ready
        let mut client = pc.call(self.addr.clone()).await?;

        client.ready().await?;
        let rsp = client.call(Request::GetPeers).await?;
        info!(?rsp);

        loop {
            // empty loop ensures we don't exit the application
        }

        Ok(())
    }
}
