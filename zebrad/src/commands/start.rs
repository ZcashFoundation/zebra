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
use tokio::sync::oneshot;
use tower::{builder::ServiceBuilder, ServiceExt};

use crate::components::{tokio::RuntimeRun, Inbound};
use crate::config::ZebradConfig;
use crate::{
    components::{tokio::TokioComponent, ChainSync},
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
        let state = ServiceBuilder::new().buffer(20).service(zebra_state::init(
            config.state.clone(),
            config.network.network,
        ));

        info!("initializing verifiers");
        let chain_verifier = zebra_consensus::chain::init(
            config.consensus.clone(),
            config.network.network,
            state.clone(),
        )
        .await;

        let block_verifier =
            zebra_consensus::block::BlockVerifier::new(config.network.network, state.clone());

        // 1- get the finalized tip
        let tip = match state.clone().oneshot(zebra_state::Request::Tip).await {
            Ok(zebra_state::Response::Tip(Some(tip))) => tip,
            Ok(zebra_state::Response::Tip(None)) => (
                zebra_chain::block::Height(1),
                zebra_chain::parameters::genesis_hash(config.network.network),
            ),
            _ => unreachable!("wrong response to Request::Tip"),
        };

        // 2- skip check if the block height is below the mandatory checkpoint
        let tip_height = tip.0;
        if tip_height >= config.network.network.mandatory_checkpoint_height() {
            // 3- we need a call to Request::Block as block verifier needs the full block
            let tip_block = match state
                .clone()
                .oneshot(zebra_state::Request::Block(
                    zebra_state::HashOrHeight::Height(tip.0),
                ))
                .await
            {
                Ok(zebra_state::Response::Block(Some(b))) => b,
                _ => unreachable!("wrong response to Request::Block"),
            };

            // 3- call BlockVerifier with the tip block
            let verifier_response = match block_verifier.oneshot(tip_block).await {
                Ok(zebra_chain::block::Hash(h)) => h,
                Err(e) => panic!("{}", e),
            };
        }

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
            ));

        let (peer_set, address_book) = zebra_network::init(config.network.clone(), inbound).await;
        setup_tx
            .send((peer_set.clone(), address_book))
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;

        info!("initializing syncer");
        let syncer = ChainSync::new(&config, peer_set, state, chain_verifier);

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
