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

        app_reader()
            .state()
            .components
            .get_downcast_ref::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .block_on(tokio::future::pending::<()>());
    }
}
