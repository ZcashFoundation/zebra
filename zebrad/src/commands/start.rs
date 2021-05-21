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
//!      tasks running within this node
//!      * via zebra_network::Request
//!  * Consensus Service
//!    * handles all validation logic for the node
//!    * verifies blocks using zebra-chain and zebra-script, then stores verified
//!      blocks in zebra-state
//!  * Sync Task
//!    * runs in the background and continuously queries the network for
//!      new blocks to be verified and added to the local state
//!  * Inbound Service
//!    * handles requests from peers for network data and chain data
//!    * performs transaction and block diffusion
//!    * downloads and verifies gossiped blocks and transactions
use std::cmp::max;

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use color_eyre::eyre::{eyre, Report};
use tokio::sync::oneshot;
use tower::builder::ServiceBuilder;

use crate::components::{tokio::RuntimeRun, Inbound};
use crate::config::ZebradConfig;
use crate::{
    components::{tokio::TokioComponent, ChainSync, MAX_INBOUND_DOWNLOAD_CONCURRENCY},
    prelude::*,
};

/// `start` subcommand
#[derive(Command, Debug, Options)]
pub struct StartCmd {
    /// Filter strings
    #[options(free)]
    filters: Vec<String>,
}

impl StartCmd {
    async fn start(&self) -> Result<(), Report> {
        let config = app_config().clone();
        info!(?config);

        info!("initializing node state");
        // Add buffer slots based on the largest concurrent caller
        //
        // Note: Zebra is currently very sensitive to buffer size changes (#2193)
        //
        // SECURITY
        //
        // Keep this buffer small, to avoid memory denial of service
        let state_limit = max(
            config.sync.max_concurrent_block_requests,
            MAX_INBOUND_DOWNLOAD_CONCURRENCY,
        );
        let state = ServiceBuilder::new()
            .buffer(state_limit)
            .service(zebra_state::init(
                config.state.clone(),
                config.network.network,
            ));

        info!("initializing verifiers");
        let verifier = zebra_consensus::chain::init(
            config.consensus.clone(),
            config.network.network,
            state.clone(),
        )
        .await;

        info!("initializing network");
        // The service that our node uses to respond to requests by peers. The
        // load_shed middleware ensures that we reduce the size of the peer set
        // in response to excess load.

        // Add buffer slots based on the largest concurrent caller.
        // But reserve two thirds the peer connections for outbound requests.
        //
        //
        // Note: Zebra is currently very sensitive to buffer size changes (#2193)
        //
        // # SECURITY
        //
        // This buffer is a memory denial of service risk, because its
        // blocks and transactions have only been structurally validated.
        //
        // TODO: Make this buffer smaller, to avoid memory denial of service (#1685, #2107)
        let inbound_limit = max(
            config.network.peerset_initial_target_size,
            MAX_INBOUND_DOWNLOAD_CONCURRENCY,
        ) / 3;
        let (setup_tx, setup_rx) = oneshot::channel();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(inbound_limit)
            .service(Inbound::new(setup_rx, state.clone(), verifier.clone()));

        let (peer_set, address_book) = zebra_network::init(config.network.clone(), inbound).await;
        setup_tx
            .send((peer_set.clone(), address_book))
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;

        info!("initializing syncer");
        let syncer = ChainSync::new(&config, peer_set, state, verifier);

        syncer.sync().await
    }
}

impl Runnable for StartCmd {
    /// Start the application.
    fn run(&self) {
        info!("Starting zebrad");
        let rt = app_writer()
            .state_mut()
            .components
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .run(self.start());
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
