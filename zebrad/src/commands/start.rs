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

mod zakura;

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

use zebra_chain::block::{self, genesis::regtest_genesis_block};
use zebra_consensus::router::BackgroundTaskHandles;
use zebra_network::types::PeerServices;
use zebra_rpc::{methods::RpcImpl, server::RpcServer, SubmitBlockChannel};

use zakura::{
    drive_block_sync_actions, drive_zakura_header_sync_actions, mirror_zakura_full_block_commits,
    query_block_sync_frontiers, zakura_header_sync_driver_startup, BlocksyncThroughputProbe,
    BlocksyncThroughputSummary, ZakuraHeaderSyncDriverHandles,
};

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

fn use_zakura_block_sync(config: &zebra_network::Config) -> bool {
    config.v2_p2p
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

    fn validate_debug_blocksync_throughput_config(config: &ZebradConfig) -> Result<(), Report> {
        let Some(target_height) = config.sync.debug_blocksync_throughput_target_height else {
            return Ok(());
        };

        if !config.network.v2_p2p {
            return Err(eyre!(
                "sync.debug_blocksync_throughput_target_height requires network.v2_p2p = true"
            ));
        }

        if target_height > block::Height::MAX.0 {
            return Err(eyre!(
                "sync.debug_blocksync_throughput_target_height={target_height} exceeds the maximum supported block height {}",
                block::Height::MAX.0
            ));
        }

        Ok(())
    }

    fn log_blocksync_throughput_summary(summary: BlocksyncThroughputSummary) {
        let elapsed_secs = summary.elapsed.as_secs_f64().max(f64::EPSILON);
        let blocks_per_second = summary.completed_blocks as f64 / elapsed_secs;
        let bytes_per_second = summary.completed_bytes as f64 / elapsed_secs;

        info!(
            target_height = ?summary.target_height,
            verified_block_tip = ?summary.verified_block_tip,
            completed_blocks = summary.completed_blocks,
            completed_bytes = summary.completed_bytes,
            elapsed_ms = u64::try_from(summary.elapsed.as_millis()).unwrap_or(u64::MAX),
            blocks_per_second,
            bytes_per_second,
            "Zakura block-sync throughput probe reached target height"
        );
    }

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

        Self::validate_debug_blocksync_throughput_config(&config)?;

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

        // Surface a misconfigured storage mode as a clean startup error, before the
        // (potentially slow) database open panics deep inside the state service.
        config
            .state
            .validate_storage_mode(&config.network.network)
            .map_err(|error| eyre!("invalid state storage configuration: {error}"))?;

        let (_, max_checkpoint_height) = zebra_consensus::router::init_checkpoint_list(
            config.consensus.clone(),
            &config.network.network,
        );

        info!("opening database, this may take a few minutes");

        let mut state_config = config.state.clone();
        state_config.enable_zakura_header_seed_from_committed_blocks = config.network.v2_p2p;

        let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
            zebra_state::init(
                state_config,
                &config.network.network,
                max_checkpoint_height,
                config.sync.checkpoint_verify_concurrency_limit
                    * (VERIFICATION_PIPELINE_SCALING_MULTIPLIER + 1),
            )
            .await;

        info!("logging database metrics on startup");
        read_only_state_service.log_db_metrics();

        let (blocksync_throughput_probe, mut blocksync_throughput_completion_rx) =
            if let Some(target_height) = config.sync.debug_blocksync_throughput_target_height {
                let target_height = block::Height(target_height);
                let initial_frontiers = query_block_sync_frontiers(
                    read_only_state_service.clone(),
                    latest_chain_tip.clone(),
                )
                .await
                .unwrap_or(zebra_network::zakura::BlockSyncFrontiers {
                    finalized_height: block::Height(0),
                    verified_block_tip: block::Height(0),
                    verified_block_hash: config.network.network.genesis_hash(),
                });
                info!(
                    ?target_height,
                    ?initial_frontiers,
                    "Zakura block-sync throughput probe enabled"
                );
                let (probe, completion_rx) =
                    BlocksyncThroughputProbe::new(initial_frontiers, target_height);
                (Some(probe), Some(completion_rx))
            } else {
                (None, None)
            };

        let state = ServiceBuilder::new()
            .buffer(Self::state_buffer_bound())
            .service(state_service);

        let zakura_header_sync_driver_startup = if config.network.v2_p2p {
            Some(
                zakura_header_sync_driver_startup(
                    read_only_state_service.clone(),
                    &config.network.network,
                )
                .await?,
            )
        } else {
            None
        };

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

        // Pruned nodes can still make outbound requests, but they cannot serve
        // arbitrary historical blocks, so they must not advertise full-node service.
        let advertised_services = if config.state.pruning_config().is_some() {
            PeerServices::empty()
        } else {
            PeerServices::NODE_NETWORK
        };

        let (peer_set, address_book, misbehavior_sender, zakura_endpoint) =
            zebra_network::init_with_zakura_header_sync(
                config.network.clone(),
                inbound,
                latest_chain_tip.clone(),
                user_agent(),
                advertised_services,
                zakura_header_sync_driver_startup,
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

        if let Some(endpoint) = zakura_endpoint.clone() {
            let trace = endpoint.trace();
            if let (Some(header_sync), Some(shutdown), Some(actions)) = (
                endpoint.header_sync(),
                endpoint.header_sync_shutdown(),
                endpoint.take_header_sync_actions().await,
            ) {
                let driver_task = tokio::spawn(
                    drive_zakura_header_sync_actions(
                        actions,
                        ZakuraHeaderSyncDriverHandles {
                            endpoint: endpoint.clone(),
                            header_sync: header_sync.clone(),
                        },
                        state.clone(),
                        read_only_state_service.clone(),
                        block_verifier_router.clone(),
                        trace.clone(),
                        shutdown.clone().cancelled_owned(),
                    )
                    .in_current_span(),
                );
                endpoint.push_header_sync_task(driver_task).await;

                if let (Some(block_sync), Some(block_actions)) = (
                    endpoint.block_sync(),
                    endpoint.take_block_sync_actions().await,
                ) {
                    let block_driver_task = tokio::spawn(
                        drive_block_sync_actions(
                            block_actions,
                            endpoint.supervisor(),
                            Some(endpoint.clone()),
                            block_sync.clone(),
                            latest_chain_tip.clone(),
                            read_only_state_service.clone(),
                            block_verifier_router.clone(),
                            max_checkpoint_height,
                            config.sync.checkpoint_verify_concurrency_limit,
                            config.sync.full_verify_concurrency_limit,
                            config.sync.zakura_block_apply_concurrency_limit,
                            trace.clone(),
                            blocksync_throughput_probe.clone(),
                            shutdown.clone().cancelled_owned(),
                        )
                        .in_current_span(),
                    );
                    endpoint.push_block_sync_task(block_driver_task).await;
                }

                let full_block_task = tokio::spawn(
                    mirror_zakura_full_block_commits(
                        chain_tip_change.clone(),
                        latest_chain_tip.clone(),
                        read_only_state_service.clone(),
                        header_sync,
                        endpoint.clone(),
                        trace,
                        shutdown.cancelled_owned(),
                    )
                    .in_current_span(),
                );
                endpoint.push_header_sync_task(full_block_task).await;
            }
        }

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
        //
        // `debug_skip_regtest_genesis_self_seed` opts a node out of this shortcut so it
        // downloads genesis from a peer instead, exercising the production
        // genesis-bootstrap path (e.g. a Zakura-only node fetching genesis over Zakura).
        if is_regtest
            && !config.sync.debug_skip_regtest_genesis_self_seed
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
        let syncer_task_handle = if use_zakura_block_sync(&config.network) {
            info!("Zakura block sync is replacing the legacy ChainSync body downloader");
            tokio::spawn(
                syncer
                    .bootstrap_genesis_then_pause(read_only_state_service.clone())
                    .in_current_span(),
            )
        } else {
            tokio::spawn(syncer.sync().in_current_span())
        };

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

                blocksync_throughput_result = async {
                    blocksync_throughput_completion_rx
                        .as_mut()
                        .expect("throughput completion branch is only enabled when receiver exists")
                        .await
                }, if blocksync_throughput_completion_rx.is_some() => {
                    let summary = blocksync_throughput_result
                        .map_err(|_| eyre!("Zakura block-sync throughput probe completion sender dropped before target height"))?;
                    Self::log_blocksync_throughput_summary(summary);
                    Ok(())
                }

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

        Self::validate_debug_blocksync_throughput_config(&config)
            .map_err(|err| std::io::Error::other(err.to_string()))?;

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
    fn blocksync_throughput_probe_requires_v2_p2p() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.network.v2_p2p = false;
        config.sync.debug_blocksync_throughput_target_height = Some(100);

        let error = cmd
            .override_config(config)
            .expect_err("throughput probe should require v2 P2P");

        assert!(
            error.to_string().contains(
                "sync.debug_blocksync_throughput_target_height requires network.v2_p2p = true"
            ),
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

#[cfg(test)]
mod zakura_header_sync_driver_tests {
    use super::*;
    use std::{
        collections::VecDeque,
        future,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };

    use futures::stream::{FuturesUnordered, StreamExt};
    use tokio::sync::mpsc;
    use tower::{service_fn, util::BoxService, ServiceExt};
    use zebra_chain::block;
    use zebra_chain::serialization::ZcashDeserializeInto;
    use zebra_network::zakura::testkit::{TraceCapture, TraceValue};
    use zebra_network::zakura::{
        commit_state_trace as cs_trace, BlockApplyResult, BlockSizeEstimate, BlockSyncAction,
        BlockSyncBlockMeta, BlockSyncEvent, BlockSyncFrontiers, BlockSyncMisbehavior,
        HeaderSyncCommitFailureKind, HeaderSyncFrontiers, Peer as ZakuraPeer,
        Service as ZakuraService, Stream as ZakuraStream, ZakuraHeaderSyncDriverStartup,
        BLOCK_SYNC_TABLE, COMMIT_STATE_TABLE, DEFAULT_HS_RANGE,
    };
    use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES};

    use super::zakura::{
        apply_block_sync_body, block_apply_class, block_sync_chain_tip_event,
        block_sync_missing_body_window, block_sync_needed_blocks_from_state,
        block_verify_error_is_duplicate, body_sizes_for_served_header_range,
        chain_tip_mirror_frontier_change, coalesce_ready_needed_block_queries,
        coalesce_stale_needed_block_queries, commit_block_sync_body, drive_block_sync_actions,
        drive_zakura_header_sync_actions, header_range_commit_failure_kind,
        notify_block_sync_header_tip, query_block_sync_frontiers, query_block_sync_needed_blocks,
        verified_block_tip_from_state, BlockApplyClass, BlocksyncThroughputProbe,
        ZakuraHeaderSyncDriverHandles, ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL,
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT, ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW,
    };

    fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
        Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
    }

    #[derive(Debug)]
    struct NoopZakuraService;

    impl ZakuraService for NoopZakuraService {
        fn name(&self) -> &'static str {
            "noop"
        }

        fn streams(&self) -> &[ZakuraStream] {
            &[]
        }

        fn add_peer(&self, _peer: ZakuraPeer) {}

        fn remove_peer(&self, _peer: &zebra_network::zakura::ZakuraPeerId) {}
    }

    fn block_sync_startup_for_test() -> zebra_network::zakura::BlockSyncStartup {
        let (tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(0), block::Hash([0; 32])));
        drop(tip_tx);
        zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(0), block::Hash([0; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        )
    }

    #[test]
    fn zakura_block_sync_replaces_chain_sync_when_v2_p2p_is_enabled() {
        let mut config = zebra_network::Config::default();

        assert!(use_zakura_block_sync(&config));

        config.v2_p2p = false;
        assert!(!use_zakura_block_sync(&config));
    }

    #[test]
    fn missing_genesis_anchor_is_local_header_sync_commit_failure() {
        let error = zebra_state::CommitHeaderRangeError::MissingGenesisAnchor {
            anchor: block::Hash([0; 32]),
        };

        assert_eq!(
            header_range_commit_failure_kind(&error),
            HeaderSyncCommitFailureKind::Local
        );
    }

    #[test]
    fn served_header_body_size_hints_align_with_served_heights() {
        let start = block::Height(10);
        let header_heights = [
            block::Height(10),
            block::Height(11),
            block::Height(12),
            block::Height(13),
        ];
        let body_size_hints = [
            (block::Height(10), Some(100)),
            (block::Height(11), None),
            (block::Height(12), Some(300)),
            (block::Height(13), Some(400)),
        ];

        assert_eq!(
            body_sizes_for_served_header_range(start, header_heights, &body_size_hints),
            vec![100, 0, 300, 400],
        );

        assert_eq!(
            body_sizes_for_served_header_range(start, header_heights, &[]),
            vec![0, 0, 0, 0],
        );
    }

    #[test]
    fn block_verify_error_duplicate_classifier_detects_router_and_block_errors() {
        let hash = block::Hash([1; 32]);
        let duplicate_block_error = zebra_consensus::VerifyBlockError::Block {
            source: zebra_consensus::BlockError::AlreadyInChain(
                hash,
                zebra_state::KnownBlock::BestChain,
            ),
        };
        assert!(block_verify_error_is_duplicate(&duplicate_block_error));

        let duplicate_router_error = zebra_consensus::RouterError::Block {
            source: Box::new(zebra_consensus::VerifyBlockError::Block {
                source: zebra_consensus::BlockError::AlreadyInChain(
                    hash,
                    zebra_state::KnownBlock::BestChain,
                ),
            }),
        };
        assert!(block_verify_error_is_duplicate(&duplicate_router_error));

        let invalid_block_error = zebra_consensus::VerifyBlockError::Block {
            source: zebra_consensus::BlockError::NoTransactions,
        };
        assert!(!block_verify_error_is_duplicate(&invalid_block_error));
    }

    #[test]
    fn block_sync_missing_body_window_stays_inside_body_sync_bound() {
        assert_eq!(
            block_sync_missing_body_window(block::Height(10), block::Height(10)),
            None
        );
        assert_eq!(
            block_sync_missing_body_window(block::Height(10), block::Height(12)),
            Some((block::Height(11), 2))
        );
        assert_eq!(
            block_sync_missing_body_window(
                block::Height(10),
                block::Height(10 + ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW + 100)
            ),
            Some((block::Height(11), ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW))
        );
        assert_eq!(
            block_sync_missing_body_window(block::Height(u32::MAX - 1), block::Height(u32::MAX)),
            Some((block::Height(u32::MAX), 1))
        );
        assert_eq!(
            block_sync_missing_body_window(block::Height(u32::MAX), block::Height(u32::MAX)),
            None
        );
    }

    #[test]
    fn block_sync_needed_blocks_align_missing_hashes_and_size_hints() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let headers = vec![
            (block::Height(1), block1.hash(), block1.header.clone()),
            (block::Height(2), block2.hash(), block2.header.clone()),
        ];
        let hints = vec![(block::Height(1), Some(0)), (block::Height(2), Some(42))];

        let needed = block_sync_needed_blocks_from_state(
            vec![block::Height(1), block::Height(2), block::Height(3)],
            headers,
            hints,
        );

        assert_eq!(
            needed,
            vec![
                BlockSyncBlockMeta {
                    height: block::Height(1),
                    hash: block1.hash(),
                    size: BlockSizeEstimate::Unknown,
                },
                BlockSyncBlockMeta {
                    height: block::Height(2),
                    hash: block2.hash(),
                    size: BlockSizeEstimate::Advertised(42),
                },
            ]
        );
    }

    #[tokio::test]
    async fn block_sync_needed_blocks_chunks_state_range_reads() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let header = block.header.clone();
        let hash = block.hash();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let read_state = {
            let requests = Arc::clone(&requests);
            service_fn(move |request| {
                let requests = Arc::clone(&requests);
                let header = Arc::clone(&header);
                async move {
                    match request {
                        zebra_state::ReadRequest::MissingBlockBodies { from, limit } => {
                            requests
                                .lock()
                                .expect("request capture mutex is not poisoned")
                                .push(("missing", from, limit));
                            let heights = (0..limit)
                                .filter_map(|offset| from.0.checked_add(offset).map(block::Height))
                                .collect();
                            Ok::<_, zebra_state::BoxError>(
                                zebra_state::ReadResponse::MissingBlockBodies(heights),
                            )
                        }
                        zebra_state::ReadRequest::HeadersByHeightRange { start, count } => {
                            requests
                                .lock()
                                .expect("request capture mutex is not poisoned")
                                .push(("headers", start, count));
                            let headers = (0..count)
                                .filter_map(|offset| {
                                    start.0.checked_add(offset).map(|height| {
                                        (block::Height(height), hash, Arc::clone(&header))
                                    })
                                })
                                .collect();
                            Ok(zebra_state::ReadResponse::Headers(headers))
                        }
                        zebra_state::ReadRequest::BlockSizeHints { from, count } => {
                            requests
                                .lock()
                                .expect("request capture mutex is not poisoned")
                                .push(("hints", from, count));
                            let hints = (0..count)
                                .filter_map(|offset| {
                                    from.0
                                        .checked_add(offset)
                                        .map(|height| (block::Height(height), Some(32)))
                                })
                                .collect();
                            Ok(zebra_state::ReadResponse::BlockSizeHints(hints))
                        }
                        request => panic!("unexpected read request: {request:?}"),
                    }
                }
            })
        };
        let count = zebra_state::constants::MAX_HEADER_SYNC_HEIGHT_RANGE + 2;

        let needed =
            query_block_sync_needed_blocks(read_state, block::Height(0), block::Height(count))
                .await
                .expect("mock read state succeeds");

        assert_eq!(
            needed.len(),
            usize::try_from(count).expect("test count fits usize")
        );
        assert_eq!(
            requests
                .lock()
                .expect("request capture mutex is not poisoned")
                .as_slice(),
            &[
                (
                    "missing",
                    block::Height(1),
                    zebra_state::constants::MAX_HEADER_SYNC_HEIGHT_RANGE,
                ),
                (
                    "headers",
                    block::Height(1),
                    zebra_state::constants::MAX_HEADER_SYNC_HEIGHT_RANGE,
                ),
                (
                    "hints",
                    block::Height(1),
                    zebra_state::constants::MAX_HEADER_SYNC_HEIGHT_RANGE,
                ),
                ("missing", block::Height(4001), 2),
                ("headers", block::Height(4001), 2),
                ("hints", block::Height(4001), 2),
            ]
        );
    }

    #[test]
    fn block_sync_chain_tip_action_mapping_preserves_reset_vs_grow() {
        let frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: block::Hash([1; 32]),
        };
        let tip_block = zebra_state::ChainTipBlock {
            hash: block::Hash([1; 32]),
            height: block::Height(1),
            time: chrono::Utc::now(),
            transactions: Vec::new(),
            transaction_hashes: Arc::<[zebra_chain::transaction::Hash]>::from([]),
            previous_block_hash: block::Hash([0; 32]),
        };

        assert!(matches!(
            block_sync_chain_tip_event(
                &zebra_state::TipAction::Grow {
                    block: tip_block.clone()
                },
                frontiers
            ),
            BlockSyncEvent::ChainTipGrow(mapped) if mapped == frontiers
        ));
        assert!(matches!(
            block_sync_chain_tip_event(
                &zebra_state::TipAction::Reset {
                    height: block::Height(1),
                    hash: block::Hash([1; 32]),
                },
                frontiers
            ),
            BlockSyncEvent::ChainTipReset(mapped) if mapped == frontiers
        ));
    }

    #[test]
    fn chain_tip_mirror_classifies_forward_reset_as_verified_grow() {
        let tip_block = zebra_state::ChainTipBlock {
            hash: block::Hash([8; 32]),
            height: block::Height(8),
            time: chrono::Utc::now(),
            transactions: Vec::new(),
            transaction_hashes: Arc::<[zebra_chain::transaction::Hash]>::from([]),
            previous_block_hash: block::Hash([7; 32]),
        };
        assert_eq!(
            chain_tip_mirror_frontier_change(
                &zebra_state::TipAction::Grow { block: tip_block },
                block::Height(7),
                block::Height(8),
            ),
            zebra_network::zakura::FrontierChange::VerifiedGrow
        );
        assert_eq!(
            chain_tip_mirror_frontier_change(
                &zebra_state::TipAction::Reset {
                    height: block::Height(8),
                    hash: block::Hash([8; 32]),
                },
                block::Height(7),
                block::Height(8),
            ),
            zebra_network::zakura::FrontierChange::VerifiedGrow
        );
        assert_eq!(
            chain_tip_mirror_frontier_change(
                &zebra_state::TipAction::Reset {
                    height: block::Height(7),
                    hash: block::Hash([77; 32]),
                },
                block::Height(8),
                block::Height(7),
            ),
            zebra_network::zakura::FrontierChange::VerifiedReset
        );
    }

    #[test]
    fn verified_block_tip_from_state_prefers_highest_frontier_with_matching_hash() {
        let empty = (block::Height(0), block::Hash([0; 32]));

        assert_eq!(
            verified_block_tip_from_state(
                Some((block::Height(2800), block::Hash([28; 32]))),
                Some((block::Height(2561), block::Hash([25; 32]))),
                empty,
            ),
            (block::Height(2800), block::Hash([28; 32]))
        );

        assert_eq!(
            verified_block_tip_from_state(
                Some((block::Height(2400), block::Hash([24; 32]))),
                Some((block::Height(2801), block::Hash([29; 32]))),
                empty,
            ),
            (block::Height(2801), block::Hash([29; 32]))
        );

        assert_eq!(
            verified_block_tip_from_state(None, None, empty),
            (block::Height(0), block::Hash([0; 32]))
        );

        assert_eq!(
            verified_block_tip_from_state(
                None,
                Some((block::Height(3), block::Hash([3; 32]))),
                empty
            ),
            (block::Height(3), block::Hash([3; 32]))
        );
    }

    #[test]
    fn block_apply_class_checkpoint_boundary_is_inclusive() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);

        assert_eq!(
            block_apply_class(&block1, block::Height(1)),
            BlockApplyClass::Checkpoint
        );
        assert_eq!(
            block_apply_class(&block2, block::Height(1)),
            BlockApplyClass::Full
        );
    }

    #[test]
    fn zakura_checkpoint_static_limits_cover_checkpoint_gap() {
        const {
            assert!(
                sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT
                    >= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP
            );
        }
        assert!(
            usize::try_from(DEFAULT_HS_RANGE)
                .expect("DEFAULT_HS_RANGE fits usize on supported targets")
                >= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP
        );
        assert!(
            usize::try_from(zebra_state::MAX_BLOCK_REORG_HEIGHT)
                .expect("MAX_BLOCK_REORG_HEIGHT fits usize on supported targets")
                >= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP
        );
    }

    #[tokio::test]
    async fn header_tip_notification_drives_block_sync_needed_query() {
        let startup = block_sync_startup_for_test();
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let header_hash = block::Hash([3; 32]);

        notify_block_sync_header_tip(
            Some(&block_sync),
            block::Height(3),
            header_hash,
            &zebra_network::zakura::ZakuraTrace::noop(),
        )
        .await;

        // The reactor emits a startup needed-block query (header tip 0) before the
        // notification is processed, so drain until the query that reflects the new
        // header tip arrives rather than asserting on the first action.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let action = reactor_actions
                    .recv()
                    .await
                    .expect("reactor action channel remains open");
                if matches!(
                    action,
                    BlockSyncAction::QueryNeededBlocks {
                        verified_block_tip: block::Height(0),
                        best_header_tip: block::Height(3),
                    }
                ) {
                    break;
                }
            }
        })
        .await
        .expect("reactor emits a needed-block query reflecting the new header tip");

        reactor_task.abort();
    }

    #[tokio::test]
    async fn header_sync_driver_header_advanced_updates_exchange_header_only() {
        let network = zebra_chain::parameters::Network::Mainnet;
        let genesis_hash = network.genesis_hash();
        let mut config = zebra_network::Config {
            network: network.clone(),
            ..zebra_network::Config::default()
        };
        config.zakura.listen_addr = None;
        let endpoint = zebra_network::zakura::spawn_zakura_endpoint_with_header_sync_driver(
            &config,
            |_supervisor, _trace| Arc::new(NoopZakuraService) as Arc<dyn ZakuraService>,
            Some(ZakuraHeaderSyncDriverStartup {
                frontiers: HeaderSyncFrontiers {
                    finalized_height: block::Height(0),
                    verified_block_tip: block::Height(0),
                    verified_block_hash: genesis_hash,
                },
                best_header_tip: Some((block::Height(0), genesis_hash)),
                verified_block_tip_hash: genesis_hash,
            }),
        )
        .await
        .expect("Zakura endpoint starts")
        .expect("v2_p2p starts an endpoint");

        let initial = endpoint
            .current_sync_frontier()
            .expect("driver startup initializes exchange");
        assert_eq!(initial.frontier.verified_body.height, block::Height(0));
        assert_eq!(initial.frontier.best_header.height, block::Height(0));

        let (action_tx, action_rx) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let handles = ZakuraHeaderSyncDriverHandles {
            endpoint: endpoint.clone(),
            header_sync: endpoint
                .header_sync()
                .expect("driver startup starts header sync"),
        };
        let state = service_fn(|request: zebra_state::Request| async move {
            panic!("unexpected state request from HeaderAdvanced: {request:?}");
            #[allow(unreachable_code)]
            Ok::<_, zebra_state::BoxError>(zebra_state::Response::Committed(block::Hash([0; 32])))
        });
        let read_state = service_fn(|request: zebra_state::ReadRequest| async move {
            panic!("unexpected read request from HeaderAdvanced: {request:?}");
            #[allow(unreachable_code)]
            Ok::<_, zebra_state::BoxError>(zebra_state::ReadResponse::Tip(None))
        });
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            panic!("unexpected verifier request from HeaderAdvanced: {request:?}");
            #[allow(unreachable_code)]
            Ok::<_, zebra_consensus::BoxError>(block::Hash([0; 32]))
        });
        let driver = tokio::spawn(drive_zakura_header_sync_actions(
            action_rx,
            handles,
            state,
            read_state,
            verifier,
            zebra_network::zakura::ZakuraTrace::noop(),
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let advanced_hash = block::Hash([5; 32]);
        action_tx
            .send(zebra_network::zakura::HeaderSyncAction::HeaderAdvanced {
                height: block::Height(5),
                hash: advanced_hash,
            })
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let update = endpoint
                    .current_sync_frontier()
                    .expect("exchange remains available");
                if update.frontier.best_header.height == block::Height(5) {
                    assert_eq!(
                        update.change,
                        zebra_network::zakura::FrontierChange::HeaderAdvanced
                    );
                    assert_eq!(update.frontier.best_header.hash, advanced_hash);
                    assert_eq!(
                        update.frontier.verified_body,
                        initial.frontier.verified_body
                    );
                    assert_eq!(update.frontier.finalized, initial.frontier.finalized);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("HeaderAdvanced publishes to the exchange");

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        endpoint.shutdown().await;
    }

    #[tokio::test]
    async fn block_sync_driver_coalesces_stale_needed_queries() {
        let (action_tx, mut action_rx) = mpsc::channel(8);
        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(1),
            })
            .await
            .expect("first query queues");
        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(2),
            })
            .await
            .expect("stale query queues");
        let deferred_peer =
            zebra_network::zakura::ZakuraPeerId::new(vec![7; 32]).expect("test peer id is valid");
        action_tx
            .send(BlockSyncAction::Misbehavior {
                peer: deferred_peer.clone(),
                reason: BlockSyncMisbehavior::StatusSpam,
            })
            .await
            .expect("non-query action queues");
        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(8),
            })
            .await
            .expect("latest query queues");

        let first = action_rx.recv().await.expect("first action remains queued");
        let mut deferred_actions = VecDeque::new();
        let action =
            coalesce_stale_needed_block_queries(first, &mut action_rx, &mut deferred_actions);

        assert!(matches!(
            action,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(8),
            }
        ));
        assert!(matches!(
            deferred_actions.pop_front(),
            Some(BlockSyncAction::Misbehavior {
                peer,
                reason: BlockSyncMisbehavior::StatusSpam,
            }) if peer == deferred_peer
        ));
        assert!(deferred_actions.is_empty());
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn block_sync_driver_prioritizes_ready_needed_query_over_submit() {
        let (action_tx, mut action_rx) = mpsc::channel(8);
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        action_tx
            .send(BlockSyncAction::SubmitBlock { token: 7, block })
            .await
            .expect("submit action queues");
        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(8),
            })
            .await
            .expect("query action queues");

        let mut deferred_actions = VecDeque::new();
        let action = coalesce_ready_needed_block_queries(&mut action_rx, &mut deferred_actions)
            .expect("ready query is prioritized");

        assert!(matches!(
            action,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(8),
            }
        ));
        assert!(matches!(
            deferred_actions.pop_front(),
            Some(BlockSyncAction::SubmitBlock { token: 7, .. })
        ));
        assert!(deferred_actions.is_empty());
        assert!(action_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn block_sync_driver_answers_needed_block_queries_from_state() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);

        let expected_headers = Arc::new(vec![
            (block::Height(1), block1.hash(), block1.header.clone()),
            (block::Height(2), block2.hash(), block2.header.clone()),
        ]);
        let expected_hints = Arc::new(vec![
            (block::Height(1), Some(0)),
            (block::Height(2), Some(42)),
        ]);
        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let read_requests_for_service = read_requests.clone();
        let read_headers = expected_headers.clone();
        let read_hints = expected_hints.clone();
        let block1_hash = block1.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_requests = read_requests_for_service.clone();
            let read_headers = read_headers.clone();
            let read_hints = read_hints.clone();
            async move {
                read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .push(request.clone());
                match request {
                    zebra_state::ReadRequest::MissingBlockBodies { from, limit } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(limit, 2);
                        Ok(zebra_state::ReadResponse::MissingBlockBodies(vec![
                            block::Height(1),
                            block::Height(2),
                        ]))
                    }
                    zebra_state::ReadRequest::HeadersByHeightRange { start, count } => {
                        assert_eq!(start, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::Headers((*read_headers).clone()))
                    }
                    zebra_state::ReadRequest::BlockSizeHints { from, count } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::BlockSizeHints(
                            (*read_hints).clone(),
                        ))
                    }
                    zebra_state::ReadRequest::FinalizedTip => {
                        Ok(zebra_state::ReadResponse::FinalizedTip(None))
                    }
                    zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                        block::Height(1),
                        block1_hash,
                    )))),
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });

        let (commit_tx, mut commit_rx) = mpsc::channel(1);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok::<_, zebra_consensus::BoxError>(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(2),
            })
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .len()
                    >= 3
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("driver answers QueryNeededBlocks through state reads");

        assert_eq!(
            read_requests
                .lock()
                .expect("test read request log is not poisoned")
                .as_slice(),
            &[
                zebra_state::ReadRequest::MissingBlockBodies {
                    from: block::Height(1),
                    limit: 2,
                },
                zebra_state::ReadRequest::HeadersByHeightRange {
                    start: block::Height(1),
                    count: 2,
                },
                zebra_state::ReadRequest::BlockSizeHints {
                    from: block::Height(1),
                    count: 2,
                },
            ]
        );

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("commit arrives after query"),
            Some(block1.hash())
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_throughput_probe_advances_without_consensus_commit() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let (_tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(3), block::Hash([3; 32])));
        let mut capture =
            TraceCapture::for_test("block_sync_driver_throughput_probe_advances").unwrap();
        let trace = zebra_network::zakura::ZakuraTrace::new(capture.tracer(), "01");
        let initial_frontiers = BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        };
        let mut startup = zebra_network::zakura::BlockSyncStartup::new(
            initial_frontiers,
            (block::Height(3), block::Hash([3; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        startup.trace = trace.clone();
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (probe, mut completion_rx) =
            BlocksyncThroughputProbe::new(initial_frontiers, block::Height(2));

        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let read_requests_for_service = read_requests.clone();
        let read_block1 = block1.clone();
        let read_block2 = block2.clone();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_requests = read_requests_for_service.clone();
            let read_block1 = read_block1.clone();
            let read_block2 = read_block2.clone();
            async move {
                read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .push(request.clone());
                match request {
                    zebra_state::ReadRequest::MissingBlockBodies { from, limit } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(limit, 3);
                        Ok(zebra_state::ReadResponse::MissingBlockBodies(vec![
                            block::Height(1),
                            block::Height(2),
                        ]))
                    }
                    zebra_state::ReadRequest::HeadersByHeightRange { start, count } => {
                        assert_eq!(start, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::Headers(vec![
                            (
                                block::Height(1),
                                read_block1.hash(),
                                read_block1.header.clone(),
                            ),
                            (
                                block::Height(2),
                                read_block2.hash(),
                                read_block2.header.clone(),
                            ),
                        ]))
                    }
                    zebra_state::ReadRequest::BlockSizeHints { from, count } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(count, 2);
                        Ok(zebra_state::ReadResponse::BlockSizeHints(Vec::new()))
                    }
                    request => panic!("unexpected read request in throughput probe: {request:?}"),
                }
            }
        });

        let commit_count = Arc::new(AtomicUsize::new(0));
        let verifier_count = commit_count.clone();
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let verifier_count = verifier_count.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        verifier_count.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, zebra_consensus::BoxError>(block.hash())
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            trace.clone(),
            Some(probe),
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let startup_action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits startup body query")
            .expect("reactor action channel stays open");
        action_tx
            .send(startup_action)
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .len()
                    >= 3
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("driver records expected throughput metadata from state");

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");

        let summary = tokio::time::timeout(Duration::from_secs(1), &mut completion_rx)
            .await
            .expect("throughput probe completes after second body")
            .expect("completion sender remains live");
        assert_eq!(summary.verified_block_tip, block::Height(2));
        assert_eq!(commit_count.load(Ordering::SeqCst), 0);

        capture.flush().await;
        let reader = capture.reader().unwrap();
        let block_sync_trace = reader.table(BLOCK_SYNC_TABLE.table());
        let chain_tip_grow_events = block_sync_trace
            .rows()
            .into_iter()
            .filter(|row| {
                row.get("event").and_then(serde_json::Value::as_str) == Some("block_event_received")
                    && row.get("kind").and_then(serde_json::Value::as_str) == Some("chain_tip_grow")
            })
            .count();
        let apply_finished_events = block_sync_trace
            .rows()
            .into_iter()
            .filter(|row| {
                row.get("event").and_then(serde_json::Value::as_str) == Some("block_event_received")
                    && row.get("kind").and_then(serde_json::Value::as_str)
                        == Some("block_apply_finished")
            })
            .count();
        assert_eq!(
            chain_tip_grow_events, 0,
            "throughput probe should advance via apply-finished local frontier only"
        );
        assert!(
            apply_finished_events >= 2,
            "throughput probe should still send apply-finished events"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        let _ = capture.finish().await.unwrap();
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_commits_parent_first_and_ignores_outbound_actions() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok::<_, zebra_consensus::BoxError>(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let block2_hash = block2.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                    zebra_state::ReadResponse::FinalizedTip(Some((block::Height(2), block2_hash))),
                ),
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block::Height(2),
                    block2_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let peer =
            zebra_network::zakura::ZakuraPeerId::new(vec![8; 32]).expect("test peer id is valid");
        // A non-`SubmitBlock` action the driver consumes without affecting the
        // commit pipeline: the driver must process it and keep its action channel
        // open, then still commit the following `SubmitBlock`s parent-first. A soft
        // misbehavior is consumed (logged) without disconnecting the peer.
        action_tx
            .send(BlockSyncAction::Misbehavior {
                peer,
                reason: zebra_network::zakura::BlockSyncMisbehavior::SizeMismatch,
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first commit arrives"),
            Some(block1.hash())
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second commit arrives"),
            Some(block2.hash())
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_submits_checkpoint_blocks_without_waiting_for_first_commit() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let release_first = Arc::new(tokio::sync::Notify::new());
        let verifier = {
            let release_first = release_first.clone();
            service_fn(move |request: zebra_consensus::Request| {
                let commit_tx = commit_tx.clone();
                let release_first = release_first.clone();
                async move {
                    match request {
                        zebra_consensus::Request::Commit(block) => {
                            let height = block.coinbase_height().expect("test block has height");
                            let hash = block.hash();
                            commit_tx
                                .send(height)
                                .await
                                .expect("test commit receiver stays open");
                            if height == block::Height(1) {
                                release_first.notified().await;
                            }
                            Ok::<_, zebra_consensus::BoxError>(hash)
                        }
                        request => panic!("unexpected consensus request: {request:?}"),
                    }
                }
            })
        };
        let block2_hash = block2.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                    zebra_state::ReadResponse::FinalizedTip(Some((block::Height(2), block2_hash))),
                ),
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block::Height(2),
                    block2_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height(2),
            2,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first checkpoint commit starts"),
            Some(block::Height(1))
        );

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second checkpoint commit starts while first is pending"),
            Some(block::Height(2))
        );

        release_first.notify_waiters();
        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_combined_apply_limit_preserves_checkpoint_window() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let release_first = Arc::new(tokio::sync::Notify::new());
        let verifier = {
            let release_first = release_first.clone();
            service_fn(move |request: zebra_consensus::Request| {
                let commit_tx = commit_tx.clone();
                let release_first = release_first.clone();
                async move {
                    match request {
                        zebra_consensus::Request::Commit(block) => {
                            let height = block.coinbase_height().expect("test block has height");
                            let hash = block.hash();
                            commit_tx
                                .send(height)
                                .await
                                .expect("test commit receiver stays open");
                            if height == block::Height(1) {
                                release_first.notified().await;
                            }
                            Ok::<_, zebra_consensus::BoxError>(hash)
                        }
                        request => panic!("unexpected consensus request: {request:?}"),
                    }
                }
            })
        };
        let block2_hash = block2.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                    zebra_state::ReadResponse::FinalizedTip(Some((block::Height(2), block2_hash))),
                ),
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block::Height(2),
                    block2_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height(2),
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            1,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first checkpoint commit starts"),
            Some(block::Height(1))
        );

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second checkpoint commit starts despite the smaller combined cap"),
            Some(block::Height(2)),
            "checkpoint applies need enough depth to complete a checkpoint verifier window",
        );

        release_first.notify_waiters();
        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_combined_apply_limit_binds_full_applies() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let release_first = Arc::new(tokio::sync::Notify::new());
        let verifier = {
            let release_first = release_first.clone();
            service_fn(move |request: zebra_consensus::Request| {
                let commit_tx = commit_tx.clone();
                let release_first = release_first.clone();
                async move {
                    match request {
                        zebra_consensus::Request::Commit(block) => {
                            let height = block.coinbase_height().expect("test block has height");
                            let hash = block.hash();
                            commit_tx
                                .send(height)
                                .await
                                .expect("test commit receiver stays open");
                            if height == block::Height(1) {
                                release_first.notified().await;
                            }
                            Ok::<_, zebra_consensus::BoxError>(hash)
                        }
                        request => panic!("unexpected consensus request: {request:?}"),
                    }
                }
            })
        };
        let block2_hash = block2.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                    zebra_state::ReadResponse::FinalizedTip(Some((block::Height(2), block2_hash))),
                ),
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block::Height(2),
                    block2_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height(0),
            2,
            2,
            1,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("first checkpoint commit starts"),
            Some(block::Height(1))
        );

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2.clone(),
            })
            .await
            .expect("driver action channel stays open");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), commit_rx.recv())
                .await
                .is_err(),
            "combined apply cap must bind full applies",
        );

        release_first.notify_one();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second checkpoint commit starts after combined cap has room"),
            Some(block::Height(2)),
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    /// A checkpoint-class commit must wait for the checkpoint verifier to
    /// assemble a full contiguous range and must never be torn down by the
    /// driver timeout, while a full (post-checkpoint) commit still times out.
    ///
    /// Regression test for the from-scratch mainnet stall: the checkpoint
    /// verifier buffers every body below the first checkpoint (height 400) until
    /// the whole range arrives, so a per-block commit timeout fired before
    /// height 400 was reached and rolled the partial range back, freezing sync
    /// at genesis.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn checkpoint_commit_waits_past_driver_timeout_unlike_full_commit() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);

        // A verifier that never answers a commit, mimicking the checkpoint
        // verifier buffering a block until its range completes.
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(_) => {
                    std::future::pending::<Result<block::Hash, zebra_consensus::BoxError>>().await
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });

        // A full-class commit gives up after the driver timeout. (`verifier` is
        // a capture-free `service_fn`, so it is `Copy` and reused below as-is.)
        assert_eq!(
            commit_block_sync_body(verifier, block.clone(), BlockApplyClass::Full).await,
            BlockApplyResult::TimedOut,
            "full commit should time out when the verifier never answers"
        );

        // A checkpoint-class commit keeps waiting: an outer timeout several times
        // longer than the driver timeout must elapse with the commit still
        // unresolved. If the driver timeout still applied to checkpoint commits,
        // this would instead resolve to Ok(TimedOut) long before the outer
        // timeout fired.
        let waited = tokio::time::timeout(
            ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT * 4,
            commit_block_sync_body(verifier, block, BlockApplyClass::Checkpoint),
        )
        .await;
        assert!(
            waited.is_err(),
            "checkpoint commit must keep waiting past the driver timeout, got {waited:?}"
        );
    }

    #[tokio::test]
    async fn unmatched_checkpoint_commit_success_does_not_refresh_block_sync_frontiers() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block_hash = block.hash();
        let (tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(10), block::Hash([10; 32])));
        let _tip_tx = tip_tx;
        let startup = zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(10), block::Hash([10; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);

        let startup_action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits startup action")
            .expect("reactor action channel remains open");
        assert!(
            matches!(
                startup_action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(0),
                    best_header_tip: block::Height(10),
                }
            ),
            "test setup should start with an initial body query, got {startup_action:?}"
        );

        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(block) => {
                    Ok::<_, zebra_consensus::BoxError>(block.hash())
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => {
                    Ok::<_, zebra_state::BoxError>(zebra_state::ReadResponse::FinalizedTip(Some((
                        block::Height(2),
                        block::Hash([2; 32]),
                    ))))
                }
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block::Height(1),
                    block_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });

        apply_block_sync_body(
            verifier,
            zebra_chain::chain_tip::NoChainTip,
            None,
            read_state,
            block_sync.clone(),
            1,
            block,
            BlockApplyClass::Checkpoint,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
        )
        .await;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), reactor_actions.recv())
                .await
                .is_err(),
            "a synthetic commit completion without a matching reactor apply must not refresh frontiers"
        );

        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_apply_emits_commit_state_trace_rows() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block_hash = block.hash();
        let block_height = block.coinbase_height().expect("test block has height");
        let mut capture =
            TraceCapture::for_test("block_sync_apply_emits_commit_state_trace_rows").unwrap();
        let trace = zebra_network::zakura::ZakuraTrace::new(capture.tracer(), "01");

        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(block) => {
                    Ok::<_, zebra_consensus::BoxError>(block.hash())
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => {
                    Ok::<_, zebra_state::BoxError>(zebra_state::ReadResponse::FinalizedTip(None))
                }
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block_height,
                    block_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });

        apply_block_sync_body(
            verifier,
            zebra_chain::chain_tip::NoChainTip,
            None,
            read_state,
            block_sync,
            77,
            block,
            BlockApplyClass::Full,
            trace,
            None,
        )
        .await;

        capture.flush().await;
        let reader = capture.reader().unwrap();
        let commit_state = reader.table(COMMIT_STATE_TABLE.table());
        let hash_label = format!("{block_hash}");
        let common = [
            (cs_trace::APPLY_TOKEN, TraceValue::U64(77)),
            (cs_trace::HEIGHT, TraceValue::U64(u64::from(block_height.0))),
            (cs_trace::HASH, TraceValue::Str(&hash_label)),
        ];
        commit_state.assert_row(cs_trace::COMMIT_START, &common);
        commit_state.assert_row(
            cs_trace::COMMIT_FINISH,
            &[
                (cs_trace::APPLY_TOKEN, TraceValue::U64(77)),
                (cs_trace::RESULT, TraceValue::Str("committed")),
            ],
        );
        commit_state.assert_row(
            cs_trace::REACTOR_EVENT_SENT,
            &[
                (cs_trace::APPLY_TOKEN, TraceValue::U64(77)),
                (cs_trace::RESULT, TraceValue::Str("committed")),
            ],
        );

        let _ = capture.finish().await.unwrap();
        reactor_task.abort();
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn block_sync_pending_checkpoint_apply_emits_stalled_trace_without_finishing() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block_hash = block.hash();
        let block_height = block.coinbase_height().expect("test block has height");
        let mut capture = TraceCapture::for_test(
            "block_sync_pending_checkpoint_apply_emits_stalled_trace_without_finishing",
        )
        .unwrap();
        let trace = zebra_network::zakura::ZakuraTrace::new(capture.tracer(), "01");

        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(_block) => {
                    future::pending::<Result<block::Hash, zebra_consensus::BoxError>>().await
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            match request {
                zebra_state::ReadRequest::FinalizedTip => {
                    Ok::<_, zebra_state::BoxError>(zebra_state::ReadResponse::FinalizedTip(None))
                }
                zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                    block_height,
                    block_hash,
                )))),
                request => panic!("unexpected read request: {request:?}"),
            }
        });

        let apply_task = tokio::spawn(apply_block_sync_body(
            verifier,
            zebra_chain::chain_tip::NoChainTip,
            None,
            read_state,
            block_sync,
            88,
            block,
            BlockApplyClass::Checkpoint,
            trace,
            None,
        ));

        tokio::task::yield_now().await;
        tokio::time::advance(ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT).await;
        tokio::task::yield_now().await;

        capture.flush().await;
        let reader = capture.reader().unwrap();
        let commit_state = reader.table(COMMIT_STATE_TABLE.table());
        commit_state.assert_row(
            cs_trace::COMMIT_START,
            &[(cs_trace::APPLY_TOKEN, TraceValue::U64(88))],
        );
        commit_state.assert_row(
            cs_trace::COMMIT_STALLED,
            &[(cs_trace::APPLY_TOKEN, TraceValue::U64(88))],
        );
        assert_eq!(
            commit_state.count(cs_trace::COMMIT_FINISH),
            0,
            "pending checkpoint verifier must not produce a finish row before it resolves"
        );

        apply_task.abort();
        let _ = capture.finish().await.unwrap();
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_pending_checkpoint_apply_does_not_block_control_plane_actions() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(_block) => {
                    future::pending::<Result<block::Hash, zebra_consensus::BoxError>>().await
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let (query_seen_tx, query_seen_rx) = oneshot::channel();
        let query_seen_tx = Arc::new(Mutex::new(Some(query_seen_tx)));
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let query_seen_tx = query_seen_tx.clone();
            async move {
                match request {
                    zebra_state::ReadRequest::MissingBlockBodies { from, limit } => {
                        assert_eq!(from, block::Height(1));
                        assert_eq!(limit, 1);
                        if let Some(query_seen_tx) = query_seen_tx
                            .lock()
                            .expect("query seen sender mutex is not poisoned")
                            .take()
                        {
                            let _ = query_seen_tx.send(());
                        }
                        Ok::<_, zebra_state::BoxError>(
                            zebra_state::ReadResponse::MissingBlockBodies(Vec::new()),
                        )
                    }
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock { token: 1, block })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(1),
            })
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), query_seen_rx)
            .await
            .expect("driver processes unrelated query while checkpoint apply is pending")
            .expect("read service reports query");

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_checkpoint_apply_limit_allows_two_checkpoint_gaps() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let two_checkpoint_gaps = zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP.saturating_mul(2);
        let (action_tx, action_rx) = mpsc::channel(two_checkpoint_gaps + 8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let commit_count = Arc::new(AtomicUsize::new(0));
        let verifier_count = commit_count.clone();
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let verifier_count = verifier_count.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(_block) => {
                        verifier_count.fetch_add(1, Ordering::SeqCst);
                        future::pending::<Result<block::Hash, zebra_consensus::BoxError>>().await
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let read_state = service_fn(move |request: zebra_state::ReadRequest| async move {
            panic!("unexpected read request while checkpoint applies are pending: {request:?}");
            #[allow(unreachable_code)]
            Ok::<_, zebra_state::BoxError>(zebra_state::ReadResponse::Tip(None))
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            two_checkpoint_gaps,
            sync::MIN_CONCURRENCY_LIMIT,
            zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        for token in 0..=two_checkpoint_gaps {
            action_tx
                .send(BlockSyncAction::SubmitBlock {
                    token: u64::try_from(token).expect("test token fits in u64"),
                    block: block.clone(),
                })
                .await
                .expect("driver action channel stays open");
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            while commit_count.load(Ordering::SeqCst) < two_checkpoint_gaps {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("driver starts exactly two checkpoint gaps of applies");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            commit_count.load(Ordering::SeqCst),
            two_checkpoint_gaps,
            "driver must not submit a third checkpoint range before earlier ranges complete"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn delayed_checkpoint_frontier_refresh_sends_committed_height() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block_hash = block.hash();
        let (_tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(10), block::Hash([10; 32])));
        let startup = zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(10), block::Hash([10; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);

        let startup_action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits startup action")
            .expect("reactor action channel remains open");
        assert!(
            matches!(
                startup_action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(0),
                    best_header_tip: block::Height(10),
                }
            ),
            "test setup should start with an initial body query, got {startup_action:?}"
        );

        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(block) => {
                    Ok::<_, zebra_consensus::BoxError>(block.hash())
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let (mut tip_sender, latest_chain_tip, _tip_change) =
            zebra_state::ChainTipSender::new(None, &zebra_chain::parameters::Network::Mainnet);
        let read_count = Arc::new(AtomicUsize::new(0));
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_index = read_count.fetch_add(1, Ordering::SeqCst);
            async move {
                let visible_tip = if read_index < 2 {
                    None
                } else {
                    Some((block::Height(1), block_hash))
                };

                match request {
                    zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                        zebra_state::ReadResponse::FinalizedTip(visible_tip),
                    ),
                    zebra_state::ReadRequest::Tip => {
                        Ok(zebra_state::ReadResponse::Tip(visible_tip))
                    }
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });

        let (action_tx, action_rx) = mpsc::channel(8);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync.clone(),
            latest_chain_tip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock { token: 1, block })
            .await
            .expect("driver action channel stays open");
        tokio::task::yield_now().await;

        assert!(
            tokio::time::timeout(Duration::from_millis(1), reactor_actions.recv())
                .await
                .is_err(),
            "the immediate refresh should not advance before state exposes the checkpoint"
        );

        tip_sender.set_finalized_tip(zebra_state::ChainTipBlock {
            hash: block_hash,
            height: block::Height(1),
            time: chrono::Utc::now(),
            transactions: Vec::new(),
            transaction_hashes: Arc::from([]),
            previous_block_hash: block::Hash([0; 32]),
        });
        tokio::time::advance(ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL).await;

        let action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits action after delayed checkpoint frontier")
            .expect("reactor action channel remains open");
        assert!(
            matches!(
                action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(1),
                    best_header_tip: block::Height(10),
                }
            ),
            "delayed checkpoint refresh must send the committed height once state catches up, got {action:?}"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn delayed_checkpoint_frontier_refresh_is_coalesced_across_commits() {
        let block1 = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let block2 = mainnet_block(&BLOCK_MAINNET_2_BYTES);
        let block2_hash = block2.hash();
        let (_tip_tx, tip_rx) =
            tokio::sync::watch::channel((block::Height(10), block::Hash([10; 32])));
        let startup = zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(10), block::Hash([10; 32])),
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        let (block_sync, mut reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let _startup_action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits startup action")
            .expect("reactor action channel remains open");

        let mut capture = TraceCapture::for_test(
            "delayed_checkpoint_frontier_refresh_is_coalesced_across_commits",
        )
        .unwrap();
        let trace = zebra_network::zakura::ZakuraTrace::new(capture.tracer(), "01");
        let visible = Arc::new(AtomicBool::new(false));
        let visible_for_reads = visible.clone();
        let read_count = Arc::new(AtomicUsize::new(0));
        let read_count_for_service = read_count.clone();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let visible = visible_for_reads.load(Ordering::SeqCst);
            let read_count = read_count_for_service.clone();
            async move {
                read_count.fetch_add(1, Ordering::SeqCst);
                let visible_tip = visible.then_some((block::Height(2), block2_hash));
                match request {
                    zebra_state::ReadRequest::FinalizedTip => Ok::<_, zebra_state::BoxError>(
                        zebra_state::ReadResponse::FinalizedTip(visible_tip),
                    ),
                    zebra_state::ReadRequest::Tip => {
                        Ok(zebra_state::ReadResponse::Tip(visible_tip))
                    }
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });
        let verifier = service_fn(|request: zebra_consensus::Request| async move {
            match request {
                zebra_consensus::Request::Commit(block) => {
                    Ok::<_, zebra_consensus::BoxError>(block.hash())
                }
                request => panic!("unexpected consensus request: {request:?}"),
            }
        });
        let (action_tx, action_rx) = mpsc::channel(8);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync.clone(),
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            trace,
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block1,
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block2,
            })
            .await
            .expect("driver action channel stays open");

        tokio::time::timeout(Duration::from_secs(1), async {
            while read_count.load(Ordering::SeqCst) < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("both immediate post-commit frontier reads complete");

        visible.store(true, Ordering::SeqCst);
        tokio::time::advance(ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL).await;

        let action = tokio::time::timeout(Duration::from_secs(1), reactor_actions.recv())
            .await
            .expect("reactor emits action after coalesced delayed checkpoint frontier")
            .expect("reactor action channel remains open");
        assert!(
            matches!(
                action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(2),
                    best_header_tip: block::Height(10),
                }
            ),
            "coalesced delayed checkpoint refresh must send the committed height once state catches up, got {action:?}"
        );

        capture.flush().await;
        let reader = capture.reader().unwrap();
        let commit_state = reader.table(COMMIT_STATE_TABLE.table());
        assert_eq!(
            commit_state.count(cs_trace::CHECKPOINT_REFRESH_ATTEMPT),
            1,
            "multiple committed checkpoint bodies should share one delayed frontier refresh attempt"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        let _ = capture.finish().await.unwrap();
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_driver_treats_duplicate_commit_as_idempotent_and_keeps_draining() {
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let (action_tx, action_rx) = mpsc::channel(8);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let attempts = Arc::new(AtomicUsize::new(0));
        let verifier_attempts = attempts.clone();
        let (commit_tx, mut commit_rx) = mpsc::channel(8);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let attempts = verifier_attempts.clone();
            let commit_tx = commit_tx.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        let hash = block.hash();
                        if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                            return Err(zebra_consensus::RouterError::Block {
                                source: Box::new(zebra_consensus::VerifyBlockError::Block {
                                    source: zebra_consensus::BlockError::AlreadyInChain(
                                        hash,
                                        zebra_state::KnownBlock::BestChain,
                                    ),
                                }),
                            });
                        }
                        commit_tx
                            .send(hash)
                            .await
                            .expect("test commit receiver stays open");
                        Ok(hash)
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });
        let read_requests = Arc::new(Mutex::new(Vec::new()));
        let read_requests_for_service = read_requests.clone();
        let block_hash = block.hash();
        let read_state = service_fn(move |request: zebra_state::ReadRequest| {
            let read_requests = read_requests_for_service.clone();
            async move {
                read_requests
                    .lock()
                    .expect("test read request log is not poisoned")
                    .push(request.clone());
                match request {
                    zebra_state::ReadRequest::FinalizedTip => {
                        Ok(zebra_state::ReadResponse::FinalizedTip(None))
                    }
                    zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some((
                        block::Height(1),
                        block_hash,
                    )))),
                    request => panic!("unexpected read request: {request:?}"),
                }
            }
        });
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            zebra_chain::chain_tip::NoChainTip,
            read_state,
            verifier,
            block::Height::MAX,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 1,
                block: block.clone(),
            })
            .await
            .expect("driver action channel stays open");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: 2,
                block: block.clone(),
            })
            .await
            .expect("driver action channel stays open");

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), commit_rx.recv())
                .await
                .expect("second commit arrives after duplicate"),
            Some(block.hash())
        );
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert!(
            read_requests
                .lock()
                .expect("test read request log is not poisoned")
                .iter()
                .any(|request| matches!(request, zebra_state::ReadRequest::Tip)),
            "duplicate commit should refresh block-sync frontiers from state"
        );

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    /// Drives the block-sync apply loop against the *real* checkpoint verifier and a *real*
    /// ephemeral state, reproducing the checkpoint-range batch-commit that Regtest (genesis
    /// checkpoint only) cannot exercise.
    ///
    /// A checkpoint is placed at height 10 so an 11-block range covers a full checkpoint gap
    /// without 400 real blocks. The whole range is submitted except one mid-range body, which
    /// the verifier holds the entire range for (it commits nothing until the range is
    /// contiguous to the next checkpoint). Delivering the withheld body must let the whole
    /// range commit — i.e. a transiently-missing body recovers instead of wedging the floor,
    /// which is the production "drop-through" failure mode.
    #[tokio::test]
    async fn block_sync_driver_recovers_checkpoint_range_after_withheld_body() {
        const CHECKPOINT_HEIGHT: u32 = 10;
        const WITHHELD: u32 = 5;

        // Real, contiguous mainnet blocks 0..=10 (valid PoW, merkle roots, and parent
        // linkage), so the real checkpoint verifier accepts them with no synthesis.
        let chain: Vec<(block::Height, Arc<block::Block>)> = (0..=CHECKPOINT_HEIGHT)
            .map(|height| {
                let bytes: &[u8] = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                    .get(&height)
                    .copied()
                    .expect("a contiguous mainnet block vector exists for heights 0..=10");
                let block = mainnet_block(bytes);
                assert_eq!(
                    block.coinbase_height(),
                    Some(block::Height(height)),
                    "mainnet block vector height matches its coinbase height",
                );
                (block::Height(height), block)
            })
            .collect();
        let genesis_hash = chain[0].1.hash();
        let checkpoint_hash = chain[CHECKPOINT_HEIGHT as usize].1.hash();

        let network = zebra_chain::parameters::Network::Mainnet;
        let (write_state, read_state, _latest_tip, _tip_change) =
            zebra_state::init_test_services(&network).await;

        // A low checkpoint at height 10 turns the 11-block range into one checkpoint batch.
        let checkpoint_verifier = zebra_consensus::CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis_hash),
                (block::Height(CHECKPOINT_HEIGHT), checkpoint_hash),
            ],
            &network,
            None,
            write_state,
        )
        .expect("a checkpoint list with genesis and one mid-chain checkpoint is valid");

        // Adapt the checkpoint verifier (`Service<Arc<Block>>`) to the driver's
        // `Service<zebra_consensus::Request, Response = block::Hash>` bound.
        let checkpoint_verifier =
            tower::buffer::Buffer::new(BoxService::new(checkpoint_verifier), 16);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let checkpoint_verifier = checkpoint_verifier.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        checkpoint_verifier.oneshot(block).await
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });

        let (action_tx, action_rx) = mpsc::channel(64);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            _latest_tip,
            read_state.clone(),
            verifier,
            // Every block 0..=10 is at or below the checkpoint, so all are Checkpoint-class
            // (indefinite-wait) commits — the path that wedges in production.
            block::Height(CHECKPOINT_HEIGHT),
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        let finalized_tip = || {
            let read_state = read_state.clone();
            async move {
                match read_state
                    .oneshot(zebra_state::ReadRequest::FinalizedTip)
                    .await
                    .expect("finalized tip read succeeds")
                {
                    zebra_state::ReadResponse::FinalizedTip(tip) => {
                        tip.map(|(height, _hash)| height)
                    }
                    response => panic!("unexpected FinalizedTip response: {response:?}"),
                }
            }
        };

        // Submit the whole checkpoint range except the withheld mid-range body.
        for (height, block) in &chain {
            if height.0 == WITHHELD {
                continue;
            }
            action_tx
                .send(BlockSyncAction::SubmitBlock {
                    token: u64::from(height.0),
                    block: block.clone(),
                })
                .await
                .expect("driver action channel stays open");
        }

        // While the body is missing the range cannot commit, and the driver must keep the
        // checkpoint-class commits pending (not time them out): the tip never reaches the
        // checkpoint.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_ne!(
            finalized_tip().await,
            Some(block::Height(CHECKPOINT_HEIGHT)),
            "checkpoint range must not commit while a mid-range body is missing",
        );

        // Deliver the withheld body; the verifier can now commit the whole range.
        let (withheld_height, withheld_block) = chain
            .iter()
            .find(|(height, _)| height.0 == WITHHELD)
            .expect("withheld block is part of the test chain");
        action_tx
            .send(BlockSyncAction::SubmitBlock {
                token: u64::from(withheld_height.0),
                block: withheld_block.clone(),
            })
            .await
            .expect("driver action channel stays open");

        // Recovery: the entire range commits, so the finalized tip reaches the checkpoint.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if finalized_tip().await == Some(block::Height(CHECKPOINT_HEIGHT)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("delivering the withheld body must let the checkpoint range commit to the tip");

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    /// Drives a from-scratch body sync across two synthetic checkpoint boundaries using
    /// real contiguous mainnet blocks, real ephemeral state, the real checkpoint verifier,
    /// the block-sync reactor, and the block-sync driver apply loop.
    #[tokio::test]
    async fn block_sync_driver_finalizes_across_two_checkpoint_boundaries() {
        const FIRST_CHECKPOINT_HEIGHT: u32 = 5;
        const SECOND_CHECKPOINT_HEIGHT: u32 = 10;

        let chain: Vec<(block::Height, Arc<block::Block>)> = (0..=SECOND_CHECKPOINT_HEIGHT)
            .map(|height| {
                let bytes: &[u8] = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                    .get(&height)
                    .copied()
                    .expect("a contiguous mainnet block vector exists for heights 0..=10");
                let block = mainnet_block(bytes);
                assert_eq!(
                    block.coinbase_height(),
                    Some(block::Height(height)),
                    "mainnet block vector height matches its coinbase height",
                );
                (block::Height(height), block)
            })
            .collect();
        let genesis_hash = chain[0].1.hash();
        let first_checkpoint_hash = chain[FIRST_CHECKPOINT_HEIGHT as usize].1.hash();
        let second_checkpoint_height = block::Height(SECOND_CHECKPOINT_HEIGHT);
        let second_checkpoint_hash = chain[SECOND_CHECKPOINT_HEIGHT as usize].1.hash();

        let network = zebra_chain::parameters::Network::Mainnet;
        let (write_state, read_state, latest_tip, _tip_change) =
            zebra_state::init_test_services(&network).await;

        let checkpoint_verifier = zebra_consensus::CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis_hash),
                (
                    block::Height(FIRST_CHECKPOINT_HEIGHT),
                    first_checkpoint_hash,
                ),
                (second_checkpoint_height, second_checkpoint_hash),
            ],
            &network,
            None,
            write_state,
        )
        .expect("a checkpoint list with two low checkpoint boundaries is valid");
        let checkpoint_verifier =
            tower::buffer::Buffer::new(BoxService::new(checkpoint_verifier), 32);
        let verifier = service_fn(move |request: zebra_consensus::Request| {
            let checkpoint_verifier = checkpoint_verifier.clone();
            async move {
                match request {
                    zebra_consensus::Request::Commit(block) => {
                        checkpoint_verifier.oneshot(block).await
                    }
                    request => panic!("unexpected consensus request: {request:?}"),
                }
            }
        });

        let (action_tx, action_rx) = mpsc::channel(64);
        let startup = block_sync_startup_for_test();
        let (block_sync, _reactor_actions, reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let driver = tokio::spawn(drive_block_sync_actions(
            action_rx,
            zebra_network::zakura::ZakuraSupervisorHandle::new(1),
            None,
            block_sync,
            latest_tip,
            read_state.clone(),
            verifier,
            second_checkpoint_height,
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
            sync::MIN_CONCURRENCY_LIMIT,
            sync::DEFAULT_ZAKURA_BLOCK_APPLY_CONCURRENCY_LIMIT,
            zebra_network::zakura::ZakuraTrace::noop(),
            None,
            async move {
                let _ = shutdown_rx.await;
            },
        ));

        for (height, block) in &chain {
            action_tx
                .send(BlockSyncAction::SubmitBlock {
                    token: u64::from(height.0),
                    block: block.clone(),
                })
                .await
                .expect("driver action channel stays open");
        }

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let finalized_tip = match read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::FinalizedTip)
                    .await
                    .expect("finalized tip read succeeds")
                {
                    zebra_state::ReadResponse::FinalizedTip(tip) => tip,
                    response => panic!("unexpected FinalizedTip response: {response:?}"),
                };

                if finalized_tip == Some((second_checkpoint_height, second_checkpoint_hash)) {
                    break;
                }

                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("driver must finalize through both checkpoint boundaries");

        let _ = shutdown_tx.send(());
        driver.await.expect("driver task exits cleanly");
        reactor_task.abort();
    }

    #[tokio::test]
    async fn block_sync_restart_reloads_checkpoint_frontier_after_missed_live_update() {
        const CHECKPOINT_HEIGHT: u32 = 10;

        // Real, contiguous mainnet blocks 0..=10, matching the low-checkpoint
        // setup in the withheld-body regression above.
        let chain: Vec<(block::Height, Arc<block::Block>)> = (0..=CHECKPOINT_HEIGHT)
            .map(|height| {
                let bytes: &[u8] = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                    .get(&height)
                    .copied()
                    .expect("a contiguous mainnet block vector exists for heights 0..=10");
                let block = mainnet_block(bytes);
                assert_eq!(
                    block.coinbase_height(),
                    Some(block::Height(height)),
                    "mainnet block vector height matches its coinbase height",
                );
                (block::Height(height), block)
            })
            .collect();
        let genesis_hash = chain[0].1.hash();
        let checkpoint_height = block::Height(CHECKPOINT_HEIGHT);
        let checkpoint_hash = chain[CHECKPOINT_HEIGHT as usize].1.hash();
        let best_header_tip = (block::Height(20), block::Hash([20; 32]));

        let network = zebra_chain::parameters::Network::Mainnet;
        let (write_state, read_state, _latest_tip, _tip_change) =
            zebra_state::init_test_services(&network).await;

        // Start a live reactor from the stale genesis frontier, with headers
        // already above the checkpoint.
        let (_tip_tx, tip_rx) = tokio::sync::watch::channel(best_header_tip);
        let startup = zebra_network::zakura::BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: genesis_hash,
            },
            best_header_tip,
            tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        let (stale_block_sync, mut stale_actions, stale_reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(startup);

        let startup_action = tokio::time::timeout(Duration::from_secs(1), stale_actions.recv())
            .await
            .expect("stale reactor emits startup action")
            .expect("stale reactor action channel remains open");
        assert!(
            matches!(
                startup_action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(0),
                    best_header_tip: block::Height(20),
                }
            ),
            "stale reactor should start querying from genesis, got {startup_action:?}"
        );

        // Commit the low checkpoint range through the real checkpoint verifier,
        // but intentionally do not notify the live block-sync reactor.
        let checkpoint_verifier = zebra_consensus::CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis_hash),
                (checkpoint_height, checkpoint_hash),
            ],
            &network,
            None,
            write_state,
        )
        .expect("a checkpoint list with genesis and one mid-chain checkpoint is valid");
        let checkpoint_verifier =
            tower::buffer::Buffer::new(BoxService::new(checkpoint_verifier), 16);
        let mut commits = FuturesUnordered::new();
        for (_height, block) in chain {
            let checkpoint_verifier = checkpoint_verifier.clone();
            commits.push(async move { checkpoint_verifier.oneshot(block).await });
        }
        while let Some(result) = commits.next().await {
            result.expect("checkpoint verifier commits the contiguous range");
        }

        let finalized_tip = || {
            let read_state = read_state.clone();
            async move {
                match read_state
                    .oneshot(zebra_state::ReadRequest::FinalizedTip)
                    .await
                    .expect("finalized tip read succeeds")
                {
                    zebra_state::ReadResponse::FinalizedTip(tip) => tip,
                    response => panic!("unexpected FinalizedTip response: {response:?}"),
                }
            }
        };

        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if finalized_tip()
                    .await
                    .is_some_and(|(height, _hash)| height == checkpoint_height)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("checkpoint range must reach durable finalized state");

        assert_eq!(
            stale_block_sync.local_status().servable_high,
            block::Height(0),
            "without a live frontier event, the old reactor remains stale"
        );
        notify_block_sync_header_tip(
            Some(&stale_block_sync),
            best_header_tip.0,
            best_header_tip.1,
            &zebra_network::zakura::ZakuraTrace::noop(),
        )
        .await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), stale_actions.recv())
                .await
                .is_err(),
            "the old reactor remains stale, but the duplicate pending query is suppressed"
        );

        stale_reactor_task.abort();

        let restart_read_state = {
            let read_state = read_state.clone();
            service_fn(move |request: zebra_state::ReadRequest| {
                let read_state = read_state.clone();
                async move {
                    match request {
                        zebra_state::ReadRequest::FinalizedTip => read_state.oneshot(request).await,
                        zebra_state::ReadRequest::Tip => Ok(zebra_state::ReadResponse::Tip(Some(
                            (block::Height(0), genesis_hash),
                        ))),
                        request => panic!("unexpected restart read request: {request:?}"),
                    }
                }
            })
        };
        let restart_frontiers =
            query_block_sync_frontiers(restart_read_state, zebra_chain::chain_tip::NoChainTip)
                .await
                .expect("restart reads block-sync frontiers from durable state");
        assert_eq!(restart_frontiers.finalized_height, checkpoint_height);
        assert_eq!(restart_frontiers.verified_block_tip, checkpoint_height);
        assert_eq!(restart_frontiers.verified_block_hash, checkpoint_hash);

        let (_restart_tip_tx, restart_tip_rx) = tokio::sync::watch::channel(best_header_tip);
        let restart_startup = zebra_network::zakura::BlockSyncStartup::new(
            restart_frontiers,
            best_header_tip,
            restart_tip_rx,
            zebra_network::zakura::ZakuraBlockSyncConfig::default(),
        );
        let (_fresh_block_sync, mut fresh_actions, fresh_reactor_task) =
            zebra_network::zakura::spawn_block_sync_reactor(restart_startup);
        let restart_action = tokio::time::timeout(Duration::from_secs(1), fresh_actions.recv())
            .await
            .expect("fresh reactor emits startup action")
            .expect("fresh reactor action channel remains open");
        assert!(
            matches!(
                restart_action,
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip: block::Height(10),
                    best_header_tip: block::Height(20),
                }
            ),
            "fresh reactor should query from the durable checkpoint frontier, got {restart_action:?}"
        );

        fresh_reactor_task.abort();
    }
}
