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

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use color_eyre::eyre::{eyre, Report};
use futures::{select, FutureExt};
use tokio::sync::oneshot;
use tower::builder::ServiceBuilder;

use crate::components::{tokio::RuntimeRun, Inbound};
use crate::config::ZebradConfig;
use crate::{
    components::{mempool, tokio::TokioComponent, ChainSync},
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
        // TODO: use ChainTipChange to get tip changes (#2374, #2710, #2711, #2712, #2713, #2714)
        let (state_service, latest_chain_tip, _chain_tip_change) =
            zebra_state::init(config.state.clone(), config.network.network);
        let state = ServiceBuilder::new().buffer(20).service(state_service);

        info!("initializing verifiers");
        // TODO: use the transaction verifier to verify mempool transactions (#2637, #2606)
        let (chain_verifier, tx_verifier) = zebra_consensus::chain::init(
            config.consensus.clone(),
            config.network.network,
            state.clone(),
        )
        .await;

        info!("initializing mempool");
        let mempool = mempool::Mempool::new(config.network.network);

        info!("initializing network");
        // The service that our node uses to respond to requests by peers. The
        // load_shed middleware ensures that we reduce the size of the peer set
        // in response to excess load.
        let (setup_tx, setup_rx) = oneshot::channel();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(20)
            .service(Inbound::new(
                setup_rx,
                state.clone(),
                chain_verifier.clone(),
                tx_verifier.clone(),
                mempool,
            ));

        let (peer_set, address_book) =
            zebra_network::init(config.network.clone(), inbound, latest_chain_tip).await;
        setup_tx
            .send((peer_set.clone(), address_book))
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;

        info!("initializing syncer");
        // TODO: use sync_status to activate the mempool (#2592)
        let (syncer, sync_status) =
            ChainSync::new(&config, peer_set.clone(), state, chain_verifier);

        select! {
            result = syncer.sync().fuse() => result,
            _ = mempool::Crawler::spawn(peer_set, sync_status).fuse() => {
                unreachable!("The mempool crawler only stops if it panics");
            }
        }
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
