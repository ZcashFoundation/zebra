//! `start` subcommand - entry point for starting a zebra node
//!
//!  ## Application Structure
//!
//!  A zebra node consists of the following services and tasks:
//!
//! Peers:
//!  * Network Service
//!    * primary external interface to the node
//!    * handles all external network requests for the Zcash protocol
//!      * via zebra_network::Message and zebra_network::Response
//!    * provides an interface to the rest of the network for other services and
//!      tasks running within this node
//!      * via zebra_network::Request
//!
//! Blocks & Mempool Transactions:
//!  * Consensus Service
//!    * handles all validation logic for the node
//!    * verifies blocks using zebra-chain, then stores verified blocks in zebra-state
//!    * verifies mempool and block transactions using zebra-chain and zebra-script,
//!      and returns verified mempool transactions for mempool storage
//!  * Groth16 Parameters Download Task
//!    * downloads the Sprout and Sapling Groth16 circuit parameter files
//!    * finishes when the download is complete and the download file hashes have been checked
//!  * Inbound Service
//!    * handles requests from peers for network data, chain data, and mempool transactions
//!    * spawns download and verify tasks for each gossiped block
//!    * sends gossiped transactions to the mempool service
//!
//! Blocks:
//!  * Sync Task
//!    * runs in the background and continuously queries the network for
//!      new blocks to be verified and added to the local state
//!    * spawns download and verify tasks for each crawled block
//!  * State Service
//!    * contextually verifies blocks
//!    * handles in-memory storage of multiple non-finalized chains
//!    * handles permanent storage of the best finalized chain
//!  * Block Gossip Task
//!    * runs in the background and continuously queries the state for
//!      newly committed blocks to be gossiped to peers
//!
//! Mempool Transactions:
//!  * Mempool Service
//!    * activates when the syncer is near the chain tip
//!    * spawns download and verify tasks for each crawled or gossiped transaction
//!    * handles in-memory storage of unmined transactions
//!  * Queue Checker Task
//!    * runs in the background, polling the mempool to store newly verified transactions
//!  * Transaction Gossip Task
//!    * runs in the background and gossips newly added mempool transactions
//!      to peers

use abscissa_core::{config, Command, FrameworkError, Options, Runnable};
use color_eyre::eyre::{eyre, Report};
use futures::FutureExt;
use tokio::{pin, select, sync::oneshot};
use tower::{builder::ServiceBuilder, util::BoxService};

use crate::{
    components::{
        inbound::InboundSetupData,
        mempool::{self, Mempool},
        sync,
        tokio::{RuntimeRun, TokioComponent},
        ChainSync, Inbound,
    },
    config::ZebradConfig,
    prelude::*,
};

/// `start` subcommand
#[derive(Command, Debug, Options)]
pub struct StartCmd {
    /// Filter strings which override the config file and defaults
    #[options(free, help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,
}

impl StartCmd {
    async fn start(&self) -> Result<(), Report> {
        let config = app_config().clone();
        info!(?config);

        info!("initializing node state");
        let (state_service, latest_chain_tip, chain_tip_change) =
            zebra_state::init(config.state.clone(), config.network.network);
        let state = ServiceBuilder::new().buffer(20).service(state_service);

        info!("initializing network");
        // The service that our node uses to respond to requests by peers. The
        // load_shed middleware ensures that we reduce the size of the peer set
        // in response to excess load.
        let (setup_tx, setup_rx) = oneshot::channel();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(20)
            .service(Inbound::new(setup_rx));

        let (peer_set, address_book) =
            zebra_network::init(config.network.clone(), inbound, latest_chain_tip.clone()).await;

        info!("initializing verifiers");
        let (chain_verifier, tx_verifier, mut groth16_download_handle) =
            zebra_consensus::chain::init(
                config.consensus.clone(),
                config.network.network,
                state.clone(),
                config.consensus.debug_skip_parameter_preload,
            )
            .await;

        info!("initializing syncer");
        let (syncer, sync_status) = ChainSync::new(
            &config,
            peer_set.clone(),
            chain_verifier.clone(),
            state.clone(),
            latest_chain_tip.clone(),
        );

        info!("initializing mempool");
        let (mempool, mempool_transaction_receiver) = Mempool::new(
            &config.mempool,
            peer_set.clone(),
            state.clone(),
            tx_verifier,
            sync_status.clone(),
            latest_chain_tip.clone(),
            chain_tip_change.clone(),
        );
        let mempool = BoxService::new(mempool);
        let mempool = ServiceBuilder::new().buffer(20).service(mempool);

        let setup_data = InboundSetupData {
            address_book,
            block_download_peer_set: peer_set.clone(),
            block_verifier: chain_verifier,
            mempool: mempool.clone(),
            state,
            latest_chain_tip,
        };
        setup_tx
            .send(setup_data)
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;

        let syncer_error_future = syncer.sync();

        let mut sync_gossip_task_handle = tokio::spawn(sync::gossip_best_tip_block_hashes(
            sync_status.clone(),
            chain_tip_change.clone(),
            peer_set.clone(),
        ));

        let mempool_crawler_task_handle = mempool::Crawler::spawn(
            &config.mempool,
            peer_set.clone(),
            mempool.clone(),
            sync_status,
            chain_tip_change,
        );

        let mempool_queue_checker_task_handle = mempool::QueueChecker::spawn(mempool);

        let tx_gossip_task_handle = tokio::spawn(mempool::gossip_mempool_transaction_id(
            mempool_transaction_receiver,
            peer_set,
        ));

        info!("spawned initial Zebra tasks");

        // TODO: spawn the syncer task, after making the PeerSet marker::Sync and marker::Send
        //       turn these tasks into a FuturesUnordered?

        // ongoing futures & tasks
        pin!(syncer_error_future);
        pin!(mempool_crawler_task_handle);
        pin!(mempool_queue_checker_task_handle);
        pin!(tx_gossip_task_handle);

        // startup tasks
        let groth16_download_handle_fused = (&mut groth16_download_handle).fuse();
        pin!(groth16_download_handle_fused);

        // Wait for tasks to finish
        let exit_status = loop {
            let mut exit_when_task_finishes = true;

            let result = select! {
                // We don't spawn the syncer future into a separate task yet.
                // So syncer panics automatically propagate to the main zebrad task.
                sync_result = &mut syncer_error_future => sync_result
                    .map(|_| info!("syncer task exited")),

                sync_gossip_result = &mut sync_gossip_task_handle => sync_gossip_result
                    .expect("unexpected panic in the chain tip block gossip task")
                    .map(|_| info!("chain tip block gossip task exited"))
                    .map_err(|e| eyre!(e)),

                mempool_crawl_result = &mut mempool_crawler_task_handle => mempool_crawl_result
                    .expect("unexpected panic in the mempool crawler")
                    .map(|_| info!("mempool crawler task exited"))
                    .map_err(|e| eyre!(e)),

                mempool_queue_result = &mut mempool_queue_checker_task_handle => mempool_queue_result
                    .expect("unexpected panic in the mempool queue checker")
                    .map(|_| info!("mempool queue checker task exited"))
                    .map_err(|e| eyre!(e)),

                tx_gossip_result = &mut tx_gossip_task_handle => tx_gossip_result
                    .expect("unexpected panic in the transaction gossip task")
                    .map(|_| info!("transaction gossip task exited"))
                    .map_err(|e| eyre!(e)),

                // Unlike other tasks, we expect the download task to finish while Zebra is running.
                groth16_download_result = &mut groth16_download_handle_fused => {
                    groth16_download_result
                        .unwrap_or_else(|_| panic!(
                            "unexpected panic in the Groth16 pre-download and check task. {}",
                            zebra_consensus::groth16::Groth16Parameters::failure_hint())
                        );

                    exit_when_task_finishes = false;
                    Ok(())
                }
            };

            // Stop Zebra if a task finished and returned an error,
            // or if an ongoing task exited.
            if let Err(err) = result {
                break Err(err);
            }

            if exit_when_task_finishes {
                break Ok(());
            }
        };

        info!("exiting Zebra because an ongoing task exited: stopping other tasks");

        // futures
        std::mem::drop(syncer_error_future);

        // ongoing tasks
        sync_gossip_task_handle.abort();
        mempool_crawler_task_handle.abort();
        mempool_queue_checker_task_handle.abort();
        tx_gossip_task_handle.abort();

        // startup tasks
        groth16_download_handle.abort();

        exit_status
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
