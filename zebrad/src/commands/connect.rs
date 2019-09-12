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

        app_reader()
            .state()
            .components
            .get_downcast_ref::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .block_on(fut);
    }
}

impl ConnectCmd {
    async fn connect(&self) {
        use std::net::Shutdown;
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpStream;
        info!("connecting");

        let mut stream = TcpStream::connect(self.addr)
            .await
            .expect("connection should succeed");

        /*
        use chrono::{DateTime, Utc};
        use zebra_network::message;
        use zebra_chain::types::BlockHeight;
        let version = message::Message::Version {
            version: message::ZCASH_VERSION,
            services: message::Services(1),
            timestamp: DateTime::<Utc>::now(),
            address_receiving: (pub self.addr,
            address_from: "127.0.0.1:9000".parse().unwrap(),
            nonce: message::Nonce(1),
            user_agent: "Zebra Connect".to_owned(),
            start_height: BlockHeight(0),
        };
        */

        stream
            .write_all(b"hello zcashd -- this is a bogus message, will you accept it or close the connection?")
            .await
            .expect("bytes should be written into stream");

        stream
            .shutdown(Shutdown::Both)
            .expect("stream should shut down cleanly");
    }
}
