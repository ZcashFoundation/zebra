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
            protocol::{codec::*, message::*, types::*},
            Network,
        };

        info!("tower stub");

        /*
        use tower::Service;
        let mut pc = peer::connector::PeerConnector {};
        let (_, _) = pc.call(self.addr.clone()).await?;
        */

        info!("connecting");

        let mut stream = Framed::new(
            TcpStream::connect(self.addr).await?,
            Codec::builder().for_network(Network::Mainnet).finish(),
        );

        let version = Message::Version {
            version: constants::CURRENT_VERSION,
            services: PeerServices::NODE_NETWORK,
            timestamp: Utc::now(),
            address_recv: (PeerServices::NODE_NETWORK, self.addr),
            // We just make something up because at this stage the `connect` command
            // doesn't run a server or anything -- will the zcashd respond on the
            // same tcp connection or try to open one to the bogus address below?
            address_from: (
                PeerServices::NODE_NETWORK,
                "127.0.0.1:9000".parse().unwrap(),
            ),
            nonce: Nonce(1),
            user_agent: "Zebra Connect".to_owned(),
            start_height: BlockHeight(0),
            relay: false,
        };

        info!(version = ?version);

        stream.send(version).await?;

        let resp_version: Message = stream.next().await.expect("expected data")?;

        info!(resp_version = ?resp_version);

        stream.send(Message::Verack).await?;

        let resp_verack = stream.next().await.expect("expected data")?;
        info!(resp_verack = ?resp_verack);

        while let Some(maybe_msg) = stream.next().await {
            match maybe_msg {
                Ok(msg) => match msg {
                    Message::Ping(nonce) => {
                        stream.send(Message::Pong(nonce)).await?;
                    }
                    _ => warn!("Unknown message"),
                },
                Err(e) => error!("{}", e),
            };
        }

        Ok(())
    }
}
