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

        use chrono::{DateTime, TimeZone, Utc};
        use tokio::io::AsyncWriteExt;
        use tokio::net::TcpStream;

        use zebra_chain::types::BlockHeight;
        use zebra_network::{constants, message::*, meta_addr::*, types::*};

        info!("connecting");

        let mut stream = TcpStream::connect(self.addr)
            .await
            .expect("connection should succeed");

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

        let mut version_bytes = Vec::new();
        version
            .zcash_serialize(
                std::io::Cursor::new(&mut version_bytes),
                constants::magics::MAINNET,
                constants::CURRENT_VERSION,
            )
            .expect("version message should serialize");

        info!(version_bytes = ?hex::encode(&version_bytes));

        stream
            .write_all(&version_bytes)
            .await
            .expect("bytes should be written into stream");
        stream
            .shutdown(Shutdown::Both)
            .expect("stream should shut down cleanly");
    }
}
