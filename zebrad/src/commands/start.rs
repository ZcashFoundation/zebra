//! `start` subcommand - entry point for starting a zebra node
//!
//! ## Application Structure
//!
//! A zebra node consists of the following major services and tasks:
//!
//! Peers:
//!  * Peer Connection Pool Service
//!    * primary external interface for outbound requests from this node to remote peers
//!    * accepts requests from services and tasks in this node, and sends them to remote peers
//!  * Peer Discovery Service
//!    * maintains a list of peer addresses, and connection priority metadata
//!    * discovers new peer addresses from existing peer connections
//!    * initiates new outbound peer connections in response to demand from tasks within this node
//!  * Peer Cache Service
//!    * Reads previous peer cache on startup, and adds it to the configured DNS seed peers
//!    * Periodically updates the peer cache on disk from the latest address book state
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
//!    * primary external interface for inbound peer requests to this node
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
//!  * Old State Version Cleanup Task
//!    * deletes outdated state versions
//!  * Block Gossip Task
//!    * runs in the background and continuously queries the state for
//!      newly committed blocks to be gossiped to peers
//!  * Progress Task
//!    * logs progress towards the chain tip
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
//!
//! Remote Procedure Calls:
//!  * JSON-RPC Service
//!    * answers RPC client requests using the State Service and Mempool Service
//!    * submits client transactions to the node's mempool
//!
//! Zebra also has diagnostic support:
//! * [metrics](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/metrics.md)
//! * [tracing](https://github.com/ZcashFoundation/zebra/blob/main/book/src/user/tracing.md)
//! * [progress-bar](https://docs.rs/howudoin/0.1.1/howudoin)
//!
//! Some of the diagnostic features are optional, and need to be enabled at compile-time.

use abscissa_core::{config, Command, FrameworkError, Runnable};
use color_eyre::eyre::{eyre, Report};
use futures::FutureExt;
use tokio::{pin, select, sync::oneshot};
use tower::{builder::ServiceBuilder, util::BoxService};
use tracing_futures::Instrument;

use zebra_consensus::router::BackgroundTaskHandles;
use zebra_rpc::server::RpcServer;

use crate::{
    application::{build_version, user_agent},
    components::{
        inbound::{self, InboundSetupData, MAX_INBOUND_RESPONSE_TIME},
        mempool::{self, Mempool},
        sync::{self, show_block_chain_progress, VERIFICATION_PIPELINE_SCALING_MULTIPLIER},
        tokio::{RuntimeRun, TokioComponent},
        ChainSync, Inbound,
    },
    config::ZebradConfig,
    prelude::*,
};

/// Start the application (default command)
#[derive(Command, Debug, Default, clap::Parser)]
pub struct StartCmd {
    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,
}

impl StartCmd {
    async fn start(&self) -> Result<(), Report> {
        let config = APPLICATION.config();

        info!("initializing node state");
        let (_, max_checkpoint_height) = zebra_consensus::router::init_checkpoint_list(
            config.consensus.clone(),
            config.network.network,
        );

        info!("opening database, this may take a few minutes");

        let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
            zebra_state::spawn_init(
                config.state.clone(),
                config.network.network,
                max_checkpoint_height,
                config.sync.checkpoint_verify_concurrency_limit
                    * (VERIFICATION_PIPELINE_SCALING_MULTIPLIER + 1),
            )
            .await?;

        let state = ServiceBuilder::new()
            .buffer(Self::state_buffer_bound())
            .service(state_service);

        info!("initializing network");
        // The service that our node uses to respond to requests by peers. The
        // load_shed middleware ensures that we reduce the size of the peer set
        // in response to excess load.
        //
        // # Security
        //
        // This layer stack is security-sensitive, modifying it can cause hangs,
        // or enable denial of service attacks.
        //
        // See `zebra_network::Connection::drive_peer_request()` for details.
        let (setup_tx, setup_rx) = oneshot::channel();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(inbound::downloads::MAX_INBOUND_CONCURRENCY)
            .timeout(MAX_INBOUND_RESPONSE_TIME)
            .service(Inbound::new(
                config.sync.full_verify_concurrency_limit,
                setup_rx,
            ));

        let (peer_set, address_book) = zebra_network::init(
            config.network.clone(),
            inbound,
            latest_chain_tip.clone(),
            user_agent(),
        )
        .await;

        info!("initializing verifiers");
        let (block_verifier_router, tx_verifier, consensus_task_handles, max_checkpoint_height) =
            zebra_consensus::router::init(
                config.consensus.clone(),
                config.network.network,
                state.clone(),
                config.consensus.debug_skip_parameter_preload,
            )
            .await;

        info!("initializing syncer");
        let (syncer, sync_status) = ChainSync::new(
            &config,
            max_checkpoint_height,
            peer_set.clone(),
            block_verifier_router.clone(),
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
        let mempool = ServiceBuilder::new()
            .buffer(mempool::downloads::MAX_INBOUND_CONCURRENCY)
            .service(mempool);

        info!("fully initializing inbound peer request handler");
        // Fully start the inbound service as soon as possible
        let setup_data = InboundSetupData {
            address_book: address_book.clone(),
            block_download_peer_set: peer_set.clone(),
            block_verifier: block_verifier_router.clone(),
            mempool: mempool.clone(),
            state,
            latest_chain_tip: latest_chain_tip.clone(),
        };
        setup_tx
            .send(setup_data)
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;
        // And give it time to clear its queue
        tokio::task::yield_now().await;

        // Launch RPC server
        let (rpc_task_handle, rpc_tx_queue_task_handle, rpc_server) = RpcServer::spawn(
            config.rpc.clone(),
            #[cfg(feature = "getblocktemplate-rpcs")]
            config.mining.clone(),
            #[cfg(not(feature = "getblocktemplate-rpcs"))]
            (),
            build_version(),
            user_agent(),
            mempool.clone(),
            read_only_state_service,
            block_verifier_router,
            sync_status.clone(),
            address_book,
            latest_chain_tip.clone(),
            config.network.network,
        );

        // Start concurrent tasks which don't add load to other tasks
        let block_gossip_task_handle = tokio::spawn(
            sync::gossip_best_tip_block_hashes(
                sync_status.clone(),
                chain_tip_change.clone(),
                peer_set.clone(),
            )
            .in_current_span(),
        );

        let mempool_queue_checker_task_handle = mempool::QueueChecker::spawn(mempool.clone());

        let tx_gossip_task_handle = tokio::spawn(
            mempool::gossip_mempool_transaction_id(mempool_transaction_receiver, peer_set.clone())
                .in_current_span(),
        );

        let mut old_databases_task_handle =
            zebra_state::check_and_delete_old_databases(config.state.clone());

        let progress_task_handle = tokio::spawn(
            show_block_chain_progress(
                config.network.network,
                latest_chain_tip.clone(),
                sync_status.clone(),
            )
            .in_current_span(),
        );

        let end_of_support_task_handle = tokio::spawn(
            sync::end_of_support::start(config.network.network, latest_chain_tip).in_current_span(),
        );

        // Give the inbound service more time to clear its queue,
        // then start concurrent tasks that can add load to the inbound service
        // (by opening more peer connections, so those peers send us requests)
        tokio::task::yield_now().await;

        // The crawler only activates immediately in tests that use mempool debug mode
        let mempool_crawler_task_handle = mempool::Crawler::spawn(
            &config.mempool,
            peer_set,
            mempool.clone(),
            sync_status,
            chain_tip_change,
        );

        let syncer_task_handle = tokio::spawn(syncer.sync().in_current_span());

        info!("spawned initial Zebra tasks");

        // TODO: put tasks into an ongoing FuturesUnordered and a startup FuturesUnordered?

        // ongoing tasks
        pin!(rpc_task_handle);
        pin!(rpc_tx_queue_task_handle);
        pin!(syncer_task_handle);
        pin!(block_gossip_task_handle);
        pin!(mempool_crawler_task_handle);
        pin!(mempool_queue_checker_task_handle);
        pin!(tx_gossip_task_handle);
        pin!(progress_task_handle);
        pin!(end_of_support_task_handle);

        // startup tasks
        let BackgroundTaskHandles {
            mut groth16_download_handle,
            mut state_checkpoint_verify_handle,
        } = consensus_task_handles;

        let groth16_download_handle_fused = (&mut groth16_download_handle).fuse();
        pin!(groth16_download_handle_fused);

        let state_checkpoint_verify_handle_fused = (&mut state_checkpoint_verify_handle).fuse();
        pin!(state_checkpoint_verify_handle_fused);

        let old_databases_task_handle_fused = (&mut old_databases_task_handle).fuse();
        pin!(old_databases_task_handle_fused);

        // Wait for tasks to finish
        let exit_status = loop {
            let mut exit_when_task_finishes = true;

            let result = select! {
                rpc_result = &mut rpc_task_handle => {
                    rpc_result
                        .expect("unexpected panic in the rpc task");
                    info!("rpc task exited");
                    Ok(())
                }

                rpc_tx_queue_result = &mut rpc_tx_queue_task_handle => {
                    rpc_tx_queue_result
                        .expect("unexpected panic in the rpc transaction queue task");
                    info!("rpc transaction queue task exited");
                    Ok(())
                }

                sync_result = &mut syncer_task_handle => sync_result
                    .expect("unexpected panic in the syncer task")
                    .map(|_| info!("syncer task exited")),

                block_gossip_result = &mut block_gossip_task_handle => block_gossip_result
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

                // The progress task runs forever, unless it panics.
                // So we don't need to provide an exit status for it.
                progress_result = &mut progress_task_handle => {
                    info!("chain progress task exited");
                    progress_result
                        .expect("unexpected panic in the chain progress task");
                }

                end_of_support_result = &mut end_of_support_task_handle => end_of_support_result
                    .expect("unexpected panic in the end of support task")
                    .map(|_| info!("end of support task exited")),


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

                // We also expect the state checkpoint verify task to finish.
                state_checkpoint_verify_result = &mut state_checkpoint_verify_handle_fused => {
                    state_checkpoint_verify_result
                        .unwrap_or_else(|_| panic!(
                            "unexpected panic checking previous state followed the best chain"));

                    exit_when_task_finishes = false;
                    Ok(())
                }

                // And the old databases task should finish while Zebra is running.
                old_databases_result = &mut old_databases_task_handle_fused => {
                    old_databases_result
                        .unwrap_or_else(|_| panic!(
                            "unexpected panic deleting old database directories"));

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

        // ongoing tasks
        rpc_task_handle.abort();
        rpc_tx_queue_task_handle.abort();
        syncer_task_handle.abort();
        block_gossip_task_handle.abort();
        mempool_crawler_task_handle.abort();
        mempool_queue_checker_task_handle.abort();
        tx_gossip_task_handle.abort();
        progress_task_handle.abort();
        end_of_support_task_handle.abort();

        // startup tasks
        groth16_download_handle.abort();
        state_checkpoint_verify_handle.abort();
        old_databases_task_handle.abort();

        // Wait until the RPC server shuts down.
        // This can take around 150 seconds.
        //
        // Without this shutdown, Zebra's RPC unit tests sometimes crashed with memory errors.
        if let Some(rpc_server) = rpc_server {
            rpc_server.shutdown_blocking();
        }

        exit_status
    }

    /// Returns the bound for the state service buffer,
    /// based on the configurations of the services that use the state concurrently.
    fn state_buffer_bound() -> usize {
        let config = APPLICATION.config();

        // Ignore the checkpoint verify limit, because it is very large.
        //
        // TODO: do we also need to account for concurrent use across services?
        //       we could multiply the maximum by 3/2, or add a fixed constant
        [
            config.sync.download_concurrency_limit,
            config.sync.full_verify_concurrency_limit,
            inbound::downloads::MAX_INBOUND_CONCURRENCY,
            mempool::downloads::MAX_INBOUND_CONCURRENCY,
        ]
        .into_iter()
        .max()
        .unwrap()
    }
}

impl Runnable for StartCmd {
    /// Start the application.
    fn run(&self) {
        info!("Starting zebrad");
        let rt = APPLICATION
            .state()
            .components_mut()
            .get_downcast_mut::<TokioComponent>()
            .expect("TokioComponent should be available")
            .rt
            .take();

        rt.expect("runtime should not already be taken")
            .run(self.start());

        info!("stopping zebrad");
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
