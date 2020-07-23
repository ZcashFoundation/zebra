//! `start` subcommand - entry point for starting a zebra node
//!
//!  ## Application Structure
//!
//!  A zebra node consists of the following services and tasks:
//!
//!  * Network Service
//!    * primary interface to the node
//!    * handles all external network requests for the Zcash protocol
//!      * via zebra_network::Message and zebra_network::Response
//!    * provides an interface to the rest of the network for other services and
//!    tasks running within this node
//!      * via zebra_network::Request
//!  * Consensus Service
//!    * handles all validation logic for the node
//!    * verifies blocks using zebra-chain and zebra-script, then stores verified
//!    blocks in zebra-state
//!  * Sync Task
//!    * This task runs in the background and continuously queries the network for
//!    new blocks to be verified and added to the local state

use crate::config::ZebradConfig;
use crate::{components::tokio::TokioComponent, prelude::*};

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use color_eyre::eyre::Report;
use tower::{buffer::Buffer, service_fn};

mod sync;

/// `start` subcommand
#[derive(Command, Debug, Options)]
pub struct StartCmd {
    /// Filter strings
    #[options(free)]
    filters: Vec<String>,
}

impl StartCmd {
    async fn start(&self) -> Result<(), Report> {
        info!(?self, "starting to connect to the network");

        // The service that our node uses to respond to requests by peers
        let node = Buffer::new(
            service_fn(|req| async move {
                info!(?req);
                Ok::<zebra_network::Response, Report>(zebra_network::Response::Nil)
            }),
            1,
        );
        let config = app_config();
        let state = zebra_state::on_disk::init(config.state.clone());
        let (peer_set, _address_book) = zebra_network::init(config.network.clone(), node).await;
        let verifier = zebra_consensus::chain::init(config.network.network, state.clone());

        let mut syncer = sync::Syncer::new(config.network.network, peer_set, state, verifier);

        syncer.sync().await
    }
}

impl Runnable for StartCmd {
    /// Start the application.
    fn run(&self) {
        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        let result = rt
            .expect("runtime should not already be taken")
            .block_on(self.start());

        match result {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {:?}", e);
                std::process::exit(1);
            }
        }
    }
}

impl config::Override<ZebradConfig> for StartCmd {
    // Process the given command line options, overriding settings from
    // a configuration file using explicit flags taken from command-line
    // arguments.
    fn override_config(&self, mut config: ZebradConfig) -> Result<ZebradConfig, FrameworkError> {
        if !self.filters.is_empty() {
            config.tracing.filter = Some(self.filters.join(","));
        }

        Ok(config)
    }
}
