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
        let fut = futures_util::future::join(self.connect(), wait);

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
        use std::net::Shutdown;

        use chrono::Utc;
        use tokio::net::TcpStream;

        use zebra_chain::types::BlockHeight;
        use zebra_network::{constants, message::*, types::*};

        info!("connecting");

        let mut stream = TcpStream::connect(self.addr).await?;

        let version = Message::Version {
            version: constants::CURRENT_VERSION,
            services: Services(1),
            timestamp: Utc::now(),
            address_recv: (Services(1), self.addr),
            // We just make something up because at this stage the `connect` command
            // doesn't run a server or anything -- will the zcashd respond on the
            // same tcp connection or try to open one to the bogus address below?
            address_from: (Services(1), "127.0.0.1:9000".parse().unwrap()),
            nonce: Nonce(1),
            user_agent: "Zebra Connect".to_owned(),
            start_height: BlockHeight(0),
            relay: false,
        };

        info!(version = ?version);

        version
            .send(
                &mut stream,
                constants::magics::MAINNET,
                constants::CURRENT_VERSION,
            )
            .await?;

        let resp_version = Message::recv(
            &mut stream,
            constants::magics::MAINNET,
            constants::CURRENT_VERSION,
        )
        .await?;
        info!(resp_version = ?resp_version);

        Message::Verack
            .send(
                &mut stream,
                constants::magics::MAINNET,
                constants::CURRENT_VERSION,
            )
            .await?;

        let resp_verack = Message::recv(
            &mut stream,
            constants::magics::MAINNET,
            constants::CURRENT_VERSION,
        )
        .await?;
        info!(resp_verack = ?resp_verack);

        loop {
            match Message::recv(
                &mut stream,
                constants::magics::MAINNET,
                constants::CURRENT_VERSION,
            )
            .await
            {
                Ok(msg) => match msg {
                    Message::Ping(nonce) => {
                        let pong = Message::Pong(nonce);
                        pong.send(
                            &mut stream,
                            constants::magics::MAINNET,
                            constants::CURRENT_VERSION,
                        )
                        .await?;
                    }
                    _ => warn!("Unknown message"),
                },
                Err(e) => error!("{}", e),
            };
        }

        stream.shutdown(Shutdown::Both)?;

        Ok(())
    }
}
