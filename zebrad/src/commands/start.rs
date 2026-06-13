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
//! Block Mining:
//!  * Internal Miner Task
//!    * if the user has configured Zebra to mine blocks, spawns tasks to generate new blocks,
//!      and submits them for verification. This automatically shares these new blocks with peers.
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

use std::{net::SocketAddr, path::Path, sync::Arc};

use abscissa_core::{config, Command, FrameworkError};
use color_eyre::eyre::{eyre, Report};
use futures::FutureExt;
use tokio::{
    pin, select,
    sync::{oneshot, watch},
};
use tower::{builder::ServiceBuilder, util::BoxService, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::genesis::regtest_genesis_block;
use zebra_consensus::router::BackgroundTaskHandles;
use zebra_rpc::{methods::RpcImpl, server::RpcServer, SubmitBlockChannel};

use crate::{
    application::{build_version, user_agent, LAST_WARN_ERROR_LOG_SENDER},
    components::{
        health,
        inbound::{self, InboundSetupData, MAX_INBOUND_RESPONSE_TIME},
        mempool::{self, Mempool},
        sync::{self, show_block_chain_progress, VERIFICATION_PIPELINE_SCALING_MULTIPLIER},
        tokio::{RuntimeRun, TokioComponent},
        zcashd_compat, ChainSync, Inbound,
    },
    config::ZebradConfig,
    prelude::*,
};

#[cfg(feature = "internal-miner")]
use crate::components;

/// Start the application (default command)
#[derive(Command, Debug, Default, clap::Parser)]
pub struct StartCmd {
    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    filters: Vec<String>,

    /// Enable zcashd-compat mode and apply zcashd-compat RPC guardrails.
    #[clap(long)]
    zcashd_compat: bool,

    /// Continue startup even when zcashd-compat preflight detects minimum hardware shortfalls.
    #[clap(long = "unsafe-low-specs")]
    unsafe_low_specs: bool,
}

/// Warns if Linux TCP slow-start-after-idle is enabled, which significantly
/// reduces single-peer throughput for block propagation.
///
/// See `book/src/user/troubleshooting.md`.
#[cfg(target_os = "linux")]
fn check_tcp_slow_start_after_idle() {
    const PATH: &str = "/proc/sys/net/ipv4/tcp_slow_start_after_idle";

    let raw = match std::fs::read_to_string(PATH) {
        Ok(raw) => raw,
        Err(error) => {
            debug!(
                ?error,
                path = PATH,
                "could not read TCP sysctl, skipping check"
            );
            return;
        }
    };

    if raw.trim() == "0" {
        return;
    }

    warn!(
        setting = "net.ipv4.tcp_slow_start_after_idle",
        "TCP slow-start-after-idle is enabled, which resets TCP's congestion window \
         between block requests and significantly reduces single-peer throughput for \
         block propagation. \
         Hint: set `net.ipv4.tcp_slow_start_after_idle=0` via sysctl. \
         See https://zebra.zfnd.org/user/troubleshooting.html#linux-tcp-tuning-for-block-propagation"
    );
}

#[cfg(not(target_os = "linux"))]
fn check_tcp_slow_start_after_idle() {}

impl StartCmd {
    /// Minimum response body size used in zcashd-compat mode.
    ///
    /// zcashd defaults to a 128 MiB response budget. That allows a memory-clamped
    /// batch of 33 raw blocks, whose worst-case response is 133,082,368 bytes.
    /// jsonrpsee applies this limit to the whole JSON-RPC batch response.
    ///
    /// These `ZCASHD_COMPAT_*` constants mirror zcashd's batch/budget arithmetic
    /// in `zcash/src/zebra_compat/zebra_client.cpp`
    /// (`ZebraCompatSyncBatchSizeFromMemoryBudget` / `ZebraRpcMaxResponseBodySize`).
    /// If the formula or constants change on either side, update both together,
    /// or startup validation will accept configs the other process rejects.
    const ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE: usize = 128 * 1024 * 1024;
    /// zcashd's default sync batch size.
    const ZCASHD_COMPAT_DEFAULT_SYNC_BATCH_SIZE: u64 = 30;
    /// Default zcashd raw-block sync response budget.
    const ZCASHD_COMPAT_DEFAULT_SYNC_RESPONSE_BUDGET_MB: u64 = 128;
    /// MiB in bytes, matching zcashd's `-zebra-compat-sync-response-budget-mb`.
    const ZCASHD_COMPAT_MIB: u64 = 1024 * 1024;
    /// zcashd's consensus maximum serialized block size.
    const ZCASHD_COMPAT_MAX_BLOCK_BYTES: u64 = 2_000_000;
    /// zcashd's per-block JSON-RPC response overhead allowance.
    const ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES: u64 = 1024;
    /// zcashd's whole-batch JSON-RPC response margin.
    const ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES: u64 = 1024 * 1024;
    /// Conservative response budget needed per raw-block sync batch entry.
    const ZCASHD_COMPAT_SYNC_RESPONSE_BUDGET_BYTES_PER_BLOCK: u64 = 4 * 1024 * 1024;
    /// Extra time Zebra waits for the zcashd-compat supervisor task beyond the
    /// child's `shutdown_grace_period`. The supervisor's `terminate_child` waits
    /// the full grace period before its SIGKILL last resort, so the outer wait
    /// must be strictly longer or aborting the task races the graceful path.
    const ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN: std::time::Duration =
        std::time::Duration::from_secs(30);
    /// Default zcashd-compat RPC listen address when `--zcashd-compat` is enabled.
    fn zcashd_compat_default_rpc_listen_addr() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 28232))
    }

    fn zcashd_compat_cookie_path(config: &ZebradConfig) -> std::path::PathBuf {
        config
            .zcashd_compat
            .cookie_dir
            .join(&config.zcashd_compat.cookie_file_name)
    }

    fn zcashd_compat_rpc_config(config: &ZebradConfig) -> zebra_rpc::config::rpc::Config {
        let mut compat_rpc_config = config.rpc.clone();
        compat_rpc_config.listen_addr = config.zcashd_compat.listen_addr;
        compat_rpc_config.enable_cookie_auth = config.zcashd_compat.enable_cookie_auth;
        compat_rpc_config.cookie_dir = config.zcashd_compat.cookie_dir.clone();
        compat_rpc_config.cookie_file_name = config.zcashd_compat.cookie_file_name.clone();
        compat_rpc_config.tls = match (
            &config.zcashd_compat.tls_cert_file,
            &config.zcashd_compat.tls_key_file,
        ) {
            (Some(cert_file), Some(key_file)) => Some(zebra_rpc::config::rpc::TlsConfig {
                cert_file: cert_file.clone(),
                key_file: key_file.clone(),
            }),
            _ => None,
        };
        compat_rpc_config.max_response_body_size = compat_rpc_config
            .max_response_body_size
            .max(Self::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE);
        compat_rpc_config
    }

    fn zcashd_compat_extra_arg_u64(
        config: &ZebradConfig,
        name: &str,
    ) -> Result<Option<u64>, Report> {
        let option_name = format!("-{name}=");
        let long_option_name = format!("--{name}=");
        let bare_option_name = format!("-{name}");
        let bare_long_option_name = format!("--{name}");

        config
            .zcashd_compat
            .zcashd_extra_args
            .iter()
            .filter_map(|arg| {
                arg.strip_prefix(&option_name)
                    .or_else(|| arg.strip_prefix(&long_option_name))
                    .map(|value| (arg, Some(value)))
                    .or_else(|| {
                        (arg == &bare_option_name || arg == &bare_long_option_name)
                            .then_some((arg, None))
                    })
            })
            .map(|(arg, value)| {
                let value = value.ok_or_else(|| {
                    eyre!("zcashd_compat.zcashd_extra_args contains {arg:?} without a value")
                })?;

                value.parse::<u64>().map_err(|error| {
                    eyre!(
                        "zcashd_compat.zcashd_extra_args contains invalid {name} value {value:?}: {error}"
                    )
                })
            })
            .next_back()
            .transpose()
    }

    fn validate_zcashd_compat_sync_batch_response_size(
        config: &ZebradConfig,
    ) -> Result<(), Report> {
        let sync_batch_size =
            Self::zcashd_compat_extra_arg_u64(config, "zebra-compat-sync-batch-size")?
                .unwrap_or(Self::ZCASHD_COMPAT_DEFAULT_SYNC_BATCH_SIZE);
        let sync_response_budget_mb =
            Self::zcashd_compat_extra_arg_u64(config, "zebra-compat-sync-response-budget-mb")?
                .unwrap_or(Self::ZCASHD_COMPAT_DEFAULT_SYNC_RESPONSE_BUDGET_MB)
                .max(1);
        let sync_response_budget_bytes = sync_response_budget_mb
            .checked_mul(Self::ZCASHD_COMPAT_MIB)
            .ok_or_else(|| {
                eyre!(
                    "zcashd-compat sync response budget {sync_response_budget_mb} MiB is too large"
                )
            })?;
        let max_batch_size_for_response_budget =
            if sync_response_budget_bytes <= Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES {
                1
            } else {
                (sync_response_budget_bytes - Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES)
                    / (2 * Self::ZCASHD_COMPAT_MAX_BLOCK_BYTES
                        + Self::ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES)
            }
            .max(1);
        let required_sync_response_budget_bytes =
            Self::ZCASHD_COMPAT_RPC_RESPONSE_BODY_MARGIN_BYTES
                .checked_add(
                    sync_batch_size
                        .checked_mul(
                            2 * Self::ZCASHD_COMPAT_MAX_BLOCK_BYTES
                                + Self::ZCASHD_COMPAT_JSON_RPC_BLOCK_OVERHEAD_BYTES,
                        )
                        .ok_or_else(|| {
                            eyre!("zcashd-compat sync batch size {sync_batch_size} is too large")
                        })?,
                )
                .ok_or_else(|| {
                    eyre!("zcashd-compat sync batch size {sync_batch_size} is too large")
                })?;
        let required_sync_response_budget_mb =
            required_sync_response_budget_bytes.div_ceil(Self::ZCASHD_COMPAT_MIB);
        let required_max_response_body_size = sync_batch_size
            .checked_mul(Self::ZCASHD_COMPAT_SYNC_RESPONSE_BUDGET_BYTES_PER_BLOCK)
            .ok_or_else(|| eyre!("zcashd-compat sync batch size {sync_batch_size} is too large"))?;
        let required_max_response_body_size: usize = required_max_response_body_size
            .try_into()
            .map_err(|_| eyre!("zcashd-compat sync batch size {sync_batch_size} is too large"))?;
        let effective_max_response_body_size = config
            .rpc
            .max_response_body_size
            .max(Self::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE);

        let mut errors = Vec::new();
        if sync_batch_size > max_batch_size_for_response_budget {
            errors.push(format!(
                "zcashd-compat sync batch size {sync_batch_size} requires \
                 zcashd_compat.zcashd_extra_args to include \
                 -zebra-compat-sync-response-budget-mb={required_sync_response_budget_mb} or higher; \
                 configured effective value is {sync_response_budget_mb}"
            ));
        }

        if effective_max_response_body_size < required_max_response_body_size {
            errors.push(format!(
                "zcashd-compat sync batch size {sync_batch_size} requires \
                 rpc.max_response_body_size = {required_max_response_body_size} or higher; \
                 configured effective value is {effective_max_response_body_size}"
            ));
        }

        if !errors.is_empty() {
            return Err(eyre!("{}", errors.join("\n")));
        }

        Ok(())
    }

    fn validate_zcashd_compat_tls_config(config: &ZebradConfig) -> Result<(), Report> {
        if config.zcashd_compat.tls_cert_file.is_some()
            != config.zcashd_compat.tls_key_file.is_some()
        {
            return Err(eyre!(
                "zcashd-compat TLS requires both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file"
            ));
        }

        if config.zcashd_compat.tls_ca_file.is_some() && !config.zcashd_compat.tls_enabled() {
            return Err(eyre!(
                "zcashd_compat.tls_ca_file requires zcashd-compat TLS with both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file"
            ));
        }

        for (name, path) in [
            (
                "zcashd_compat.tls_cert_file",
                &config.zcashd_compat.tls_cert_file,
            ),
            (
                "zcashd_compat.tls_key_file",
                &config.zcashd_compat.tls_key_file,
            ),
            (
                "zcashd_compat.tls_ca_file",
                &config.zcashd_compat.tls_ca_file,
            ),
        ] {
            if let Some(path) = path {
                std::fs::File::open(path)
                    .map_err(|error| eyre!("could not read {name}={}: {error}", path.display()))?;
            }
        }

        if let Some(listen_addr) = config.zcashd_compat.listen_addr {
            if !listen_addr.ip().is_loopback()
                && !config.zcashd_compat.tls_enabled()
                && !config.zcashd_compat.unsafe_allow_remote_http
            {
                return Err(eyre!(
                    "zcashd_compat.listen_addr={listen_addr} is non-loopback and requires TLS with both zcashd_compat.tls_cert_file and zcashd_compat.tls_key_file, \
                     or zcashd_compat.unsafe_allow_remote_http=true when a container or private network boundary secures the listener"
                ));
            }
        }

        if config.zcashd_compat.enabled
            && config.zcashd_compat.manage_zcashd
            && config.zcashd_compat.tls_enabled()
            && config.zcashd_compat.tls_ca_file.is_none()
        {
            return Err(eyre!(
                "zcashd-compat supervision with TLS requires zcashd_compat.tls_ca_file so zcashd can verify Zebra"
            ));
        }

        Ok(())
    }

    fn zcashd_compat_rpc_url(config: &ZebradConfig) -> Result<String, Report> {
        let listen_addr = config.zcashd_compat.listen_addr.ok_or_else(|| {
            eyre!("zcashd-compat mode requires zcashd_compat.listen_addr to be set")
        })?;
        let scheme = if config.zcashd_compat.tls_enabled() {
            "https"
        } else {
            "http"
        };
        Ok(format!("{scheme}://{listen_addr}"))
    }

    /// Returns the supervisor shutdown timeout when zcashd-compat `zcashd` supervision is active.
    ///
    /// This is the configured `shutdown_grace_period` plus a fixed margin, so the
    /// supervisor task always gets to finish its own SIGTERM → grace → SIGKILL
    /// sequence before Zebra gives up on the task.
    fn zcashd_compat_supervisor_shutdown_timeout(
        config: &ZebradConfig,
    ) -> Option<std::time::Duration> {
        (config.zcashd_compat.enabled && config.zcashd_compat.manage_zcashd).then_some(
            config
                .zcashd_compat
                .shutdown_grace_period
                .saturating_add(Self::ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN),
        )
    }

    async fn start(&self) -> Result<(), Report> {
        check_tcp_slow_start_after_idle();

        let config = APPLICATION.config();
        let is_regtest = config.network.network.is_regtest();

        let config = if is_regtest {
            Arc::new(ZebradConfig {
                mempool: mempool::Config {
                    debug_enable_at_height: Some(0),
                    ..config.mempool
                },
                ..Arc::unwrap_or_clone(config)
            })
        } else {
            config
        };

        if config.zcashd_compat.enabled {
            zcashd_compat::run_preflight(&config, self.unsafe_low_specs)?;
        }

        let resolved_zcashd_path = if config.zcashd_compat.enabled
            && config.zcashd_compat.manage_zcashd
        {
            let zcashd_compat_config = config.zcashd_compat.clone();
            let state_cache_dir = config.state.cache_dir.clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    zcashd_compat::resolve_zcashd_binary_path(
                        &zcashd_compat_config,
                        &state_cache_dir,
                    )
                })
                .await
                .map_err(|err| eyre!("failed to join managed zcashd binary resolver: {err}"))??,
            )
        } else {
            None
        };

        info!("initializing node state");
        let (_, max_checkpoint_height) = zebra_consensus::router::init_checkpoint_list(
            config.consensus.clone(),
            &config.network.network,
        );

        info!("opening database, this may take a few minutes");

        let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
            zebra_state::init(
                config.state.clone(),
                &config.network.network,
                max_checkpoint_height,
                config.sync.checkpoint_verify_concurrency_limit
                    * (VERIFICATION_PIPELINE_SCALING_MULTIPLIER + 1),
            )
            .await;

        info!("logging database metrics on startup");
        read_only_state_service.log_db_metrics();

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

        let (peer_set, address_book, misbehavior_sender) = zebra_network::init(
            config.network.clone(),
            inbound,
            latest_chain_tip.clone(),
            user_agent(),
        )
        .await;

        // Start health server if configured (after sync_status is available)

        info!("initializing verifiers");
        let (tx_verifier_setup_tx, tx_verifier_setup_rx) = oneshot::channel();
        let (block_verifier_router, tx_verifier, consensus_task_handles, max_checkpoint_height) =
            zebra_consensus::router::init(
                config.consensus.clone(),
                &config.network.network,
                state.clone(),
                tx_verifier_setup_rx,
            )
            .await;

        info!("initializing syncer");
        let (mut syncer, sync_status) = ChainSync::new(
            &config,
            max_checkpoint_height,
            peer_set.clone(),
            block_verifier_router.clone(),
            state.clone(),
            latest_chain_tip.clone(),
            misbehavior_sender.clone(),
        );

        info!("initializing mempool");
        let (mempool, mempool_transaction_subscriber) = Mempool::new(
            &config.mempool,
            peer_set.clone(),
            state.clone(),
            tx_verifier,
            sync_status.clone(),
            latest_chain_tip.clone(),
            chain_tip_change.clone(),
            misbehavior_sender.clone(),
        );
        let mempool = BoxService::new(mempool);
        let mempool = ServiceBuilder::new()
            .buffer(mempool::downloads::MAX_INBOUND_CONCURRENCY)
            .service(mempool);

        if tx_verifier_setup_tx.send(mempool.clone()).is_err() {
            warn!("error setting up the transaction verifier with a handle to the mempool service");
        };

        info!("fully initializing inbound peer request handler");
        // Fully start the inbound service as soon as possible
        let setup_data = InboundSetupData {
            address_book: address_book.clone(),
            block_download_peer_set: peer_set.clone(),
            block_verifier: block_verifier_router.clone(),
            mempool: mempool.clone(),
            state: state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            misbehavior_sender,
        };
        setup_tx
            .send(setup_data)
            .map_err(|_| eyre!("could not send setup data to inbound service"))?;
        // And give it time to clear its queue
        tokio::task::yield_now().await;

        // Create a channel to send mined blocks to the gossip task
        let submit_block_channel = SubmitBlockChannel::new();

        // Launch RPC server
        let (rpc_impl, mut rpc_tx_queue_handle) = RpcImpl::new(
            config.network.network.clone(),
            config.mining.clone(),
            config.rpc.debug_force_finished_sync,
            build_version(),
            user_agent(),
            mempool.clone(),
            state.clone(),
            read_only_state_service.clone(),
            block_verifier_router.clone(),
            sync_status.clone(),
            latest_chain_tip.clone(),
            address_book.clone(),
            LAST_WARN_ERROR_LOG_SENDER.subscribe(),
            Some(submit_block_channel.sender()),
        );

        let rpc_task_handle = if config.rpc.listen_addr.is_some() {
            RpcServer::start(rpc_impl.clone(), config.rpc.clone())
                .await
                .expect("server should start")
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        let zcashd_compat_rpc_task_handle = if config.zcashd_compat.enabled {
            RpcServer::start(rpc_impl.clone(), Self::zcashd_compat_rpc_config(&config))
                .await
                .expect("zcashd-compat RPC server should start")
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        let zcashd_compat_shutdown_timeout =
            Self::zcashd_compat_supervisor_shutdown_timeout(&config);
        let (zcashd_compat_shutdown_tx, zcashd_compat_shutdown_rx) = watch::channel(false);
        let mut zcashd_compat_task_handle = if let Some(resolved_zcashd_path) = resolved_zcashd_path
        {
            let supervisor_config = zcashd_compat::SupervisorConfig::new(
                &config.zcashd_compat,
                resolved_zcashd_path,
                &config.state.cache_dir,
                config.network.network.kind(),
                Self::zcashd_compat_rpc_url(&config)?,
                Self::zcashd_compat_cookie_path(&config),
                Self::zcashd_compat_rpc_config(&config).max_response_body_size,
            );

            info!(
                rpc_url = %supervisor_config.rpc_url,
                cookie_file = %supervisor_config.cookie_path.display(),
                "zcashd-compat source enabled"
            );

            tokio::spawn(
                zcashd_compat::run_supervisor(supervisor_config, zcashd_compat_shutdown_rx)
                    .in_current_span(),
            )
        } else {
            if config.zcashd_compat.enabled {
                zcashd_compat::set_supervision_config_disabled_metrics();
                info!(
                    rpc_url = %Self::zcashd_compat_rpc_url(&config)?,
                    cookie_file = %Self::zcashd_compat_cookie_path(&config).display(),
                    "zcashd-compat source enabled: zcashd supervision disabled"
                );
            }

            tokio::spawn(std::future::pending().in_current_span())
        };

        // TODO: Add a shutdown signal and start the server with `serve_with_incoming_shutdown()` if
        //       any related unit tests sometimes crash with memory errors
        let indexer_rpc_task_handle = {
            if let Some(indexer_listen_addr) = config.rpc.indexer_listen_addr {
                info!("spawning indexer RPC server");
                let (indexer_rpc_task_handle, _listen_addr) = zebra_rpc::indexer::server::init(
                    indexer_listen_addr,
                    read_only_state_service.clone(),
                    latest_chain_tip.clone(),
                    mempool_transaction_subscriber.clone(),
                )
                .await
                .map_err(|err| eyre!(err))?;

                indexer_rpc_task_handle
            } else {
                warn!("configure an indexer_listen_addr to start the indexer RPC server");
                tokio::spawn(std::future::pending().in_current_span())
            }
        };

        // Start concurrent tasks which don't add load to other tasks
        info!("spawning block gossip task");
        let block_gossip_task_handle = tokio::spawn(
            sync::gossip_best_tip_block_hashes(
                sync_status.clone(),
                chain_tip_change.clone(),
                peer_set.clone(),
                Some(submit_block_channel.receiver()),
            )
            .in_current_span(),
        );

        info!("spawning mempool queue checker task");
        let mempool_queue_checker_task_handle = mempool::QueueChecker::spawn(mempool.clone());

        info!("spawning mempool transaction gossip task");
        let tx_gossip_task_handle = tokio::spawn(
            mempool::gossip_mempool_transaction_id(
                mempool_transaction_subscriber.subscribe(),
                peer_set.clone(),
            )
            .in_current_span(),
        );

        info!("spawning delete old databases task");
        let mut old_databases_task_handle = zebra_state::check_and_delete_old_state_databases(
            &config.state,
            &config.network.network,
        );

        info!("spawning progress logging task");
        let (chain_tip_metrics_sender, chain_tip_metrics_receiver) =
            health::ChainTipMetrics::channel();
        let progress_task_handle = tokio::spawn(
            show_block_chain_progress(
                config.network.network.clone(),
                latest_chain_tip.clone(),
                sync_status.clone(),
                chain_tip_metrics_sender,
            )
            .in_current_span(),
        );

        // Start health server if configured
        info!("initializing health endpoints");
        let (health_task_handle, _) = health::init(
            config.health.clone(),
            config.network.network.clone(),
            chain_tip_metrics_receiver,
            sync_status.clone(),
            address_book.clone(),
        )
        .await;

        // Spawn never ending end of support task.
        info!("spawning end of support checking task");
        let end_of_support_task_handle = tokio::spawn(
            sync::end_of_support::start(config.network.network.clone(), latest_chain_tip.clone())
                .in_current_span(),
        );

        // Give the inbound service more time to clear its queue,
        // then start concurrent tasks that can add load to the inbound service
        // (by opening more peer connections, so those peers send us requests)
        tokio::task::yield_now().await;

        // The crawler only activates immediately in tests that use mempool debug mode
        info!("spawning mempool crawler task");
        let mempool_crawler_task_handle = mempool::Crawler::spawn(
            &config.mempool,
            peer_set,
            mempool.clone(),
            sync_status.clone(),
            chain_tip_change.clone(),
        );

        info!("spawning syncer task");
        // In regtest, commit the genesis block directly (bypassing the syncer's genesis
        // download, which requires a connected peer). Then run the syncer normally so
        // that multi-hop block propagation works: gossiped blocks that arrive out of
        // order (e.g. only the latest tip hash was gossiped) will be recovered by the
        // syncer using block locators within REGTEST_SYNC_RESTART_DELAY (2 seconds).
        if is_regtest
            && !syncer
                .state_contains(config.network.network.genesis_hash())
                .await?
        {
            let genesis_hash = block_verifier_router
                .clone()
                .oneshot(zebra_consensus::Request::Commit(regtest_genesis_block()))
                .await
                .expect("should validate Regtest genesis block");

            assert_eq!(
                genesis_hash,
                config.network.network.genesis_hash(),
                "validated block hash should match network genesis hash"
            )
        }
        let syncer_task_handle = tokio::spawn(syncer.sync().in_current_span());

        // And finally, spawn the internal Zcash miner, if it is enabled.
        //
        // TODO: add a config to enable the miner rather than a feature.
        #[cfg(feature = "internal-miner")]
        let miner_task_handle = if config.mining.is_internal_miner_enabled() {
            info!("spawning Zcash miner");
            components::miner::spawn_init(&config.metrics, rpc_impl)
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        #[cfg(not(feature = "internal-miner"))]
        // Spawn a dummy miner task which doesn't do anything and never finishes.
        let miner_task_handle: tokio::task::JoinHandle<Result<(), Report>> =
            tokio::spawn(std::future::pending().in_current_span());

        info!("spawned initial Zebra tasks");

        // TODO: put tasks into an ongoing FuturesUnordered and a startup FuturesUnordered?

        // ongoing tasks
        pin!(rpc_task_handle);
        pin!(zcashd_compat_rpc_task_handle);
        pin!(indexer_rpc_task_handle);
        pin!(syncer_task_handle);
        pin!(block_gossip_task_handle);
        pin!(mempool_crawler_task_handle);
        pin!(mempool_queue_checker_task_handle);
        pin!(tx_gossip_task_handle);
        pin!(progress_task_handle);
        pin!(end_of_support_task_handle);
        pin!(miner_task_handle);

        // startup tasks
        let BackgroundTaskHandles {
            mut state_checkpoint_verify_handle,
        } = consensus_task_handles;

        let state_checkpoint_verify_handle_fused = (&mut state_checkpoint_verify_handle).fuse();
        pin!(state_checkpoint_verify_handle_fused);

        let old_databases_task_handle_fused = (&mut old_databases_task_handle).fuse();
        pin!(old_databases_task_handle_fused);

        // Wait for tasks to finish
        let mut zcashd_compat_task_finished = false;
        let exit_status = {
            let zcashd_compat_task_handle_fused = (&mut zcashd_compat_task_handle).fuse();
            pin!(zcashd_compat_task_handle_fused);

            loop {
                let mut exit_when_task_finishes = true;

                let result = select! {
                rpc_join_result = &mut rpc_task_handle => {
                    let rpc_server_result = rpc_join_result
                        .expect("unexpected panic in the rpc task");
                    info!(?rpc_server_result, "rpc task exited");
                    Ok(())
                }

                zcashd_compat_rpc_join_result = &mut zcashd_compat_rpc_task_handle => {
                    let compat_rpc_server_result = zcashd_compat_rpc_join_result
                        .expect("unexpected panic in the zcashd-compat rpc task");
                    info!(?compat_rpc_server_result, "zcashd-compat rpc task exited");
                    Ok(())
                }

                rpc_tx_queue_result = &mut rpc_tx_queue_handle => {
                    rpc_tx_queue_result
                        .expect("unexpected panic in the rpc transaction queue task");
                    info!("rpc transaction queue task exited");
                    Ok(())
                }

                indexer_rpc_join_result = &mut indexer_rpc_task_handle => {
                    let indexer_rpc_server_result = indexer_rpc_join_result
                        .expect("unexpected panic in the indexer task");
                    info!(?indexer_rpc_server_result, "indexer rpc task exited");
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

                miner_result = &mut miner_task_handle => miner_result
                    .expect("unexpected panic in the miner task")
                    .map(|_| info!("miner task exited")),

                    zcashd_compat_result = &mut zcashd_compat_task_handle_fused => {
                        zcashd_compat_task_finished = true;
                        exit_when_task_finishes =
                            Self::zcashd_compat_supervisor_should_exit(zcashd_compat_result);
                        Ok(())
                    },
                };

                // Stop Zebra if a task finished and returned an error,
                // or if an ongoing task exited.
                if let Err(err) = result {
                    break Err(err);
                }

                if exit_when_task_finishes {
                    break Ok(());
                }
            }
        };

        info!("exiting Zebra because an ongoing task exited: asking other tasks to stop");

        // ongoing tasks
        rpc_task_handle.abort();
        zcashd_compat_rpc_task_handle.abort();
        rpc_tx_queue_handle.abort();
        health_task_handle.abort();
        syncer_task_handle.abort();
        block_gossip_task_handle.abort();
        mempool_crawler_task_handle.abort();
        mempool_queue_checker_task_handle.abort();
        tx_gossip_task_handle.abort();
        progress_task_handle.abort();
        end_of_support_task_handle.abort();
        miner_task_handle.abort();
        if zcashd_compat_task_finished {
            debug!("zcashd-compat supervisor task already exited before shutdown");
        } else if let Some(zcashd_compat_shutdown_timeout) = zcashd_compat_shutdown_timeout {
            info!(
                ?zcashd_compat_shutdown_timeout,
                "requesting zcashd-compat supervisor shutdown"
            );
            if zcashd_compat_shutdown_tx.send(true).is_err() {
                warn!("zcashd-compat supervisor shutdown request was not delivered");
            }
            if tokio::time::timeout(
                zcashd_compat_shutdown_timeout,
                &mut zcashd_compat_task_handle,
            )
            .await
            .is_err()
            {
                warn!(
                    ?zcashd_compat_shutdown_timeout,
                    "zcashd-compat supervisor did not finish before shutdown timeout; \
                     abandoning child process handle"
                );
                // The supervisor spawns zcashd without kill_on_drop, so this
                // abort abandons an already-signalled child rather than
                // SIGKILLing it mid-flush.
                zcashd_compat_task_handle.abort();
            }
        } else {
            debug!("aborting zcashd-compat supervisor task without managed child shutdown");
            zcashd_compat_task_handle.abort();
        }

        // startup tasks
        state_checkpoint_verify_handle.abort();
        old_databases_task_handle.abort();

        info!(
            "exiting Zebra: all tasks have been asked to stop, waiting for remaining tasks to finish"
        );

        exit_status
    }

    /// Returns `false` so Zebra keeps running if zcashd-compat supervision exits unexpectedly.
    fn zcashd_compat_supervisor_should_exit(
        zcashd_compat_result: Result<Result<(), Report>, tokio::task::JoinError>,
    ) -> bool {
        zcashd_compat::set_supervision_unexpectedly_disabled_metrics();

        match zcashd_compat_result {
            Ok(Ok(())) => {
                warn!(
                    "zcashd-compat supervisor task exited unexpectedly in supervision mode; continuing without zcashd supervision"
                );
            }
            Ok(Err(err)) => {
                warn!(
                    ?err,
                    "zcashd-compat supervisor task failed in supervision mode; continuing without zcashd supervision"
                );
            }
            Err(join_err) => {
                warn!(
                    ?join_err,
                    "zcashd-compat supervisor task panicked in supervision mode; continuing without zcashd supervision"
                );
            }
        }

        false
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

        // `--zcashd-compat` is a one-way override that enables zcashd-compat mode.
        // The actual zcashd-compat guardrails are applied below using
        // `config.zcashd_compat.enabled` so CLI and config-file activation share one path.
        if self.zcashd_compat {
            config.zcashd_compat.enabled = true;
        }

        if config.zcashd_compat.enabled {
            if config.zcashd_compat.listen_addr.is_none() {
                config.zcashd_compat.listen_addr =
                    Some(Self::zcashd_compat_default_rpc_listen_addr());
            }

            if let (Some(rpc_listen_addr), Some(compat_listen_addr)) =
                (config.rpc.listen_addr, config.zcashd_compat.listen_addr)
            {
                if rpc_listen_addr == compat_listen_addr {
                    return Err(std::io::Error::other(format!(
                        "zcashd-compat mode requires different RPC listen addresses: \
                         rpc.listen_addr={rpc_listen_addr} conflicts with \
                         zcashd_compat.listen_addr={compat_listen_addr}"
                    ))
                    .into());
                }
            }

            Self::validate_zcashd_compat_tls_config(&config)
                .map_err(|err| std::io::Error::other(err.to_string()))?;

            if !config.zcashd_compat.enable_cookie_auth && !config.zcashd_compat.tls_enabled() {
                return Err(std::io::Error::other(
                    "zcashd_compat.enable_cookie_auth=false requires TLS on the zcashd-compat RPC listener",
                )
                .into());
            }

            Self::validate_zcashd_compat_sync_batch_response_size(&config)
                .map_err(|err| std::io::Error::other(err.to_string()))?;

            if config.zcashd_compat.manage_zcashd {
                match zcashd_compat::effective_zcashd_source(&config.zcashd_compat) {
                    Ok(zcashd_compat::ZcashdBinarySource::Path(path))
                        if !zcashd_compat::is_command_resolvable(Path::new(&path)) =>
                    {
                        return Err(std::io::Error::other(format!(
                            "zcashd-compat mode could not resolve zcashd_path={}",
                            path.display()
                        ))
                        .into());
                    }
                    Ok(_) => {}
                    Err(err) => return Err(std::io::Error::other(err.to_string()).into()),
                }
            }
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use abscissa_core::config::Override;
    use color_eyre::eyre::eyre;

    use super::StartCmd;
    use crate::components::zcashd_compat;
    use crate::config::ZebradConfig;

    #[test]
    fn zcashd_compat_flag_applies_rpc_guardrails() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = None;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
        assert_eq!(
            config.zcashd_compat.listen_addr,
            Some(StartCmd::zcashd_compat_default_rpc_listen_addr())
        );
        assert_eq!(config.rpc.listen_addr, None);
    }

    #[test]
    fn zcashd_compat_config_applies_rpc_guardrails() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = None;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
        assert_eq!(
            config.zcashd_compat.listen_addr,
            Some(StartCmd::zcashd_compat_default_rpc_listen_addr())
        );
        assert_eq!(config.rpc.listen_addr, None);
    }

    #[test]
    fn zcashd_compat_flag_rejects_conflicting_rpc_listen_addr() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.rpc.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.manage_zcashd = false;

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat should reject overlapping RPC listen addresses");

        assert!(
            error
                .to_string()
                .contains("requires different RPC listen addresses"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_conflicting_rpc_listen_addr() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.rpc.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat should reject overlapping configured RPC listen addresses");

        assert!(
            error
                .to_string()
                .contains("requires different RPC listen addresses"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_rpc_config_uses_dedicated_cookie_and_min_response_size() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.cookie_dir = "/tmp/zcashd-compat-cookie-dir".into();
        config.zcashd_compat.cookie_file_name = ".zcashd-compat.cookie".to_string();
        config.rpc.cookie_dir = "/tmp/standard-rpc-cookie-dir".into();
        config.rpc.cookie_file_name = ".cookie".to_string();
        config.rpc.max_response_body_size = 1024;

        let compat_rpc_config = StartCmd::zcashd_compat_rpc_config(&config);
        assert_eq!(
            compat_rpc_config.listen_addr,
            config.zcashd_compat.listen_addr
        );
        assert_eq!(
            compat_rpc_config.cookie_dir,
            config.zcashd_compat.cookie_dir
        );
        assert_eq!(
            compat_rpc_config.cookie_file_name,
            config.zcashd_compat.cookie_file_name
        );
        assert!(compat_rpc_config.enable_cookie_auth);
        assert_eq!(
            compat_rpc_config.max_response_body_size,
            StartCmd::ZCASHD_COMPAT_MIN_MAX_RESPONSE_BODY_SIZE
        );
    }

    #[test]
    fn zcashd_compat_rpc_config_can_disable_cookie_auth() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.enable_cookie_auth = false;

        let compat_rpc_config = StartCmd::zcashd_compat_rpc_config(&config);

        assert!(!compat_rpc_config.enable_cookie_auth);
    }

    #[test]
    fn zcashd_compat_rpc_url_uses_https_when_tls_enabled() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.listen_addr = Some(StartCmd::zcashd_compat_default_rpc_listen_addr());
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra.crt".into());
        config.zcashd_compat.tls_key_file = Some("/tmp/zebra.key".into());

        let rpc_url =
            StartCmd::zcashd_compat_rpc_url(&config).expect("zcashd-compat RPC URL should format");

        assert!(rpc_url.starts_with("https://"));
    }

    #[test]
    fn zcashd_compat_config_rejects_non_loopback_listen_addr_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());

        let error = cmd
            .override_config(config)
            .expect_err("non-loopback zcashd-compat RPC should require TLS");

        assert!(
            error
                .to_string()
                .contains("listen_addr=192.0.2.1:28232 is non-loopback and requires TLS"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_loopback_listen_addr_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([127, 0, 0, 1], 28232).into());

        cmd.override_config(config)
            .expect("loopback zcashd-compat RPC should allow plain HTTP");
    }

    #[test]
    fn zcashd_compat_config_allows_non_loopback_listen_addr_with_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        cmd.override_config(config)
            .expect("non-loopback zcashd-compat RPC should be allowed with TLS");
    }

    #[test]
    fn zcashd_compat_config_allows_non_loopback_listen_addr_with_unsafe_override() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.listen_addr = Some(([192, 0, 2, 1], 28232).into());
        config.zcashd_compat.unsafe_allow_remote_http = true;

        cmd.override_config(config).expect(
            "non-loopback zcashd-compat RPC should be allowed with the explicit unsafe override",
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_no_cookie_auth_without_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.enable_cookie_auth = false;

        let error = cmd
            .override_config(config)
            .expect_err("no-cookie zcashd-compat RPC should require TLS");

        assert!(
            error
                .to_string()
                .contains("enable_cookie_auth=false requires TLS"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_no_cookie_auth_with_tls() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.enable_cookie_auth = false;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        cmd.override_config(config)
            .expect("no-cookie zcashd-compat RPC should be allowed with TLS");
    }

    #[test]
    fn zcashd_compat_config_rejects_missing_tls_files() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra-missing.crt".into());
        config.zcashd_compat.tls_key_file = Some("/tmp/zebra-missing.key".into());

        let error = cmd
            .override_config(config)
            .expect_err("missing TLS files should be rejected");
        assert!(
            error.to_string().contains("could not read"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_tls_ca_without_tls_listener() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let ca_file = tempdir.path().join("ca.pem");
        std::fs::write(&ca_file, "placeholder ca").expect("CA file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_ca_file = Some(ca_file);

        let error = cmd
            .override_config(config)
            .expect_err("CA file should require an enabled TLS listener");
        assert!(
            error.to_string().contains("tls_ca_file requires"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_managed_tls_without_ca_file() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);

        let error = cmd
            .override_config(config)
            .expect_err("managed TLS should require a CA file");
        assert!(
            error.to_string().contains("tls_ca_file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_managed_tls_with_ca_file() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let tempdir = tempfile::tempdir().expect("tempdir should be created");
        let cert_file = tempdir.path().join("zebra.crt");
        let key_file = tempdir.path().join("zebra.key");
        let ca_file = tempdir.path().join("ca.pem");
        std::fs::write(&cert_file, "placeholder cert").expect("cert file should be writable");
        std::fs::write(&key_file, "placeholder key").expect("key file should be writable");
        std::fs::write(&ca_file, "placeholder ca").expect("CA file should be writable");
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.tls_cert_file = Some(cert_file);
        config.zcashd_compat.tls_key_file = Some(key_file);
        config.zcashd_compat.tls_ca_file = Some(ca_file);

        cmd.override_config(config)
            .expect("managed zcashd-compat RPC should accept TLS with a CA file");
    }

    #[test]
    fn zcashd_compat_config_rejects_incomplete_tls_pair() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;
        config.zcashd_compat.tls_cert_file = Some("/tmp/zebra.crt".into());

        let error = cmd
            .override_config(config)
            .expect_err("TLS should require both cert and key files");

        assert!(
            error.to_string().contains("tls_cert_file")
                && error.to_string().contains("tls_key_file"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_large_sync_batch_without_matching_rpc_response_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=80".to_string()];

        let error = cmd
            .override_config(config)
            .expect_err("large zcashd sync batches should require a matching RPC response limit");

        assert!(
            error
                .to_string()
                .contains("rpc.max_response_body_size = 335544320"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_allows_large_sync_batch_with_matching_rpc_response_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args = vec![
            "-zebra-compat-sync-batch-size=80".to_string(),
            "-zebra-compat-sync-response-budget-mb=320".to_string(),
        ];
        config.rpc.max_response_body_size = 320 * 1024 * 1024;

        cmd.override_config(config)
            .expect("large zcashd sync batches should allow a matching RPC response limit");
    }

    #[test]
    fn zcashd_compat_config_rejects_large_sync_batch_without_matching_zcashd_response_budget() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=80".to_string()];
        config.rpc.max_response_body_size = 320 * 1024 * 1024;

        let error = cmd.override_config(config).expect_err(
            "large zcashd sync batches should require a matching zcashd response budget",
        );

        assert!(
            error
                .to_string()
                .contains("-zebra-compat-sync-response-budget-mb=307"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_invalid_sync_batch_size() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.zcashd_extra_args =
            vec!["-zebra-compat-sync-batch-size=eighty".to_string()];

        let error = cmd
            .override_config(config)
            .expect_err("invalid zcashd sync batch sizes should be rejected");

        assert!(
            error
                .to_string()
                .contains("invalid zebra-compat-sync-batch-size value"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_cookie_path_uses_compat_cookie_dir() {
        let mut config = ZebradConfig::default();
        config.zcashd_compat.cookie_dir = "/tmp/zcashd-compat-cookie-dir".into();
        config.zcashd_compat.cookie_file_name = ".zcashd-compat.cookie".to_string();
        config.rpc.cookie_dir = "/tmp/standard-rpc-cookie-dir".into();
        config.rpc.cookie_file_name = ".cookie".to_string();

        assert_eq!(
            StartCmd::zcashd_compat_cookie_path(&config),
            std::path::PathBuf::from("/tmp/zcashd-compat-cookie-dir/.zcashd-compat.cookie")
        );
    }

    #[test]
    fn zcashd_compat_manage_zcashd_requires_resolvable_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_path = Some("/definitely/missing/zcashd-compat".into());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat override should fail for an unresolvable zcashd path");

        assert!(
            error
                .to_string()
                .contains("zcashd-compat mode could not resolve zcashd_path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_path_source_requires_explicit_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Path;
        config.zcashd_compat.zcashd_path = None;

        let error = cmd
            .override_config(config)
            .expect_err("path source should require explicit zcashd_path");
        assert!(
            error.to_string().contains("zcashd_source=path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_managed_source_allows_missing_local_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Managed;
        config.zcashd_compat.zcashd_path = None;

        cmd.override_config(config)
            .expect("managed source should be validated at runtime, not override-time");
    }

    #[test]
    fn zcashd_compat_config_manage_zcashd_requires_resolvable_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_path = Some("/definitely/missing/zcashd-compat".into());

        let error = cmd
            .override_config(config)
            .expect_err("zcashd-compat config should fail for an unresolvable zcashd path");

        assert!(
            error
                .to_string()
                .contains("zcashd-compat mode could not resolve zcashd_path"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn zcashd_compat_supervisor_shutdown_timeout_matches_config() {
        let mut config = ZebradConfig::default();

        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.shutdown_grace_period = std::time::Duration::from_secs(42);
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            Some(
                std::time::Duration::from_secs(42)
                    + StartCmd::ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN
            ),
            "outer supervisor wait must exceed the child grace period so task \
             abort cannot preempt graceful termination",
        );

        config.zcashd_compat.manage_zcashd = false;
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            None
        );

        config.zcashd_compat.enabled = false;
        config.zcashd_compat.manage_zcashd = true;
        assert_eq!(
            StartCmd::zcashd_compat_supervisor_shutdown_timeout(&config),
            None
        );
    }

    #[test]
    fn zcashd_compat_supervisor_ok_exit_does_not_exit_zebra() {
        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Ok(Ok(()))));
    }

    #[test]
    fn zcashd_compat_supervisor_error_does_not_exit_zebra() {
        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Ok(Err(
            eyre!("simulated zcashd supervisor runtime failure"),
        ))));
    }

    #[tokio::test]
    async fn zcashd_compat_supervisor_panic_does_not_exit_zebra() {
        let join_err = tokio::spawn(async {
            panic!("simulated zcashd supervisor panic");
        })
        .await
        .expect_err("task should panic");

        assert!(!StartCmd::zcashd_compat_supervisor_should_exit(Err(
            join_err
        )));
    }
}
