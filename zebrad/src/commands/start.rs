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
//!    * gossips committed tips and authenticated pending submissions to peers
//!  * Block Notify Task
//!    * if the user has configured a `notify.block_notify_command`, runs that command
//!      whenever the best chain tip changes (Zebra's equivalent of zcashd's `-blocknotify`)
//!  * ZMQ Hashblock Task
//!    * publishes synced best-tip hashes when `notify.zmqpubhashblock` is configured
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

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use abscissa_core::{config, Command, FrameworkError};
use color_eyre::eyre::{eyre, Report};
use futures::FutureExt;
use tokio::{
    pin, select,
    sync::{oneshot, watch},
};
use tower::{builder::ServiceBuilder, util::BoxService, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{genesis::regtest_genesis_block, Height},
    chain_tip::ChainTip,
    parameters::known_hashes::KnownHashListSpec,
};
use zebra_consensus::router::BackgroundTaskHandles;
use zebra_rpc::{methods::RpcImpl, server::RpcServer, SubmitBlockChannel, SubmittedBlockCache};

use crate::{
    application::{build_version, user_agent, LAST_WARN_ERROR_LOG_SENDER},
    components::{
        checkpoint_sync,
        header_sync::HeaderSync,
        health,
        inbound::{self, InboundSetupData, MAX_INBOUND_RESPONSE_TIME},
        mempool::{self, Mempool},
        notify::{self, BlockNotifyError},
        resources,
        state_snapshot::{
            bootstrap,
            decision::{self, Decision},
            progress::{self, RuntimeStages, Stage},
            serving,
        },
        sync::{
            self, record_tip_block_delay, show_block_chain_progress,
            VERIFICATION_PIPELINE_SCALING_MULTIPLIER,
        },
        tokio::{RuntimeRun, TokioComponent},
        zcashd_compat, ChainSync, Inbound,
    },
    config::ZebradConfig,
    prelude::*,
};

#[cfg(feature = "internal-miner")]
use crate::components;

mod snapshot_headers;
mod spentness_hints;

/// Start the application (default command)
#[derive(Command, Debug, Default, clap::Parser)]
pub struct StartCmd {
    /// Filter strings which override the config file and defaults
    #[clap(help = "tracing filters which override the zebrad.toml config")]
    pub(super) filters: Vec<String>,

    /// Enable zcashd-compat mode.
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

// A syncing pruned node keeps at least as many recent bodies as limited service promises.

impl StartCmd {
    /// Extra time Zebra waits for the zcashd-compat supervisor task beyond the
    /// child's `shutdown_grace_period`. The supervisor's `terminate_child` waits
    /// the full grace period before its SIGKILL last resort, so the outer wait
    /// must be strictly longer or aborting the task races the graceful path.
    const ZCASHD_COMPAT_SHUTDOWN_TIMEOUT_MARGIN: std::time::Duration =
        std::time::Duration::from_secs(30);

    /// Applies the configured fallback to a snapshot rejected by an earlier process.
    fn handle_rejected_snapshot(config: &ZebradConfig, decision: &Decision) -> Result<(), Report> {
        if !matches!(decision, Decision::Skip(decision::SkipReason::Rejected)) {
            return Ok(());
        }

        let fallback = config.sync.state_snapshot_fallback;
        if fallback.exits(config.sync.state_snapshot.is_some(), true) {
            Err(eyre!(
                "the state snapshot was rejected by an earlier bootstrap, and \
                 sync.state_snapshot_fallback is \"{}\", so Zebra exits without opening the \
                 state or syncing from genesis. Use a different trusted commitment, or investigate \
                 the rejection and remove its marker before retrying it. Set \
                 sync.state_snapshot_fallback = \"genesis\" to sync from genesis without retrying \
                 the rejected snapshot",
                fallback.name()
            ))
        } else {
            warn!(
                fallback = fallback.name(),
                "the state snapshot was rejected by an earlier bootstrap: syncing from genesis \
                 without retrying the rejected snapshot"
            );
            Ok(())
        }
    }

    /// Returns the Zebra P2P address supervised zcashd should `-connect` to.
    ///
    /// Uses `zcashd_compat.p2p_connect_addr` when set, otherwise Zebra's bound
    /// P2P listener, substituting loopback for unspecified addresses so
    /// zcashd gets a dialable target on the same host.
    fn zcashd_compat_p2p_connect_addr(
        config: &ZebradConfig,
        local_listener: SocketAddr,
    ) -> SocketAddr {
        if let Some(addr) = config.zcashd_compat.p2p_connect_addr {
            return addr;
        }

        if local_listener.ip().is_unspecified() {
            // Substitute the loopback address of the same IP family: an
            // IPv6-only listener is not reachable via 127.0.0.1.
            match local_listener.ip() {
                IpAddr::V4(_) => SocketAddr::from(([127, 0, 0, 1], local_listener.port())),
                IpAddr::V6(_) => {
                    SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), local_listener.port())
                }
            }
        } else {
            local_listener
        }
    }

    /// Returns the default inbound peer IPs that always receive block gossip in
    /// zcashd-compat mode.
    fn zcashd_compat_default_block_gossip_peer_ips() -> Vec<IpAddr> {
        vec![
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ]
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

    /// Returns `false` so Zebra keeps running if zcashd-compat supervision exits unexpectedly.
    fn zcashd_compat_supervisor_should_exit(
        zcashd_compat_result: Result<Result<(), Report>, tokio::task::JoinError>,
    ) -> bool {
        zcashd_compat::set_supervision_unexpectedly_disabled_metrics();

        match zcashd_compat_result {
            Ok(Ok(())) => {
                warn!(
                    "zcashd-compat supervisor task exited unexpectedly in supervision mode; \
                     continuing without zcashd supervision"
                );
            }
            Ok(Err(err)) => {
                warn!(
                    ?err,
                    "zcashd-compat supervisor task failed in supervision mode; \
                     continuing without zcashd supervision"
                );
            }
            Err(join_err) => {
                warn!(
                    ?join_err,
                    "zcashd-compat supervisor task panicked in supervision mode; \
                     continuing without zcashd supervision"
                );
            }
        }

        false
    }

    /// Keeps Zebra running after key preload succeeds, but propagates a failed startup task.
    pub(super) fn halo2_key_preload_should_exit(
        result: Result<(), tokio::task::JoinError>,
    ) -> bool {
        result.expect("Halo2 verifying key construction must succeed");
        info!("finished Halo2 verifying key preload");
        false
    }

    /// Spawns the checkpoint sync engine's supervisor, which streams
    /// checkpointed blocks from version 2 `peers` beside the syncer, and
    /// records which of them owns block download in `sync.v2.owner`.
    ///
    /// Without `peers` and `ownership`, which `start` creates only when
    /// `sync.v2_checkpoint_sync` is set, the returned task never finishes.
    ///
    /// The engine anchors its downloads at the verified entries of
    /// `known_hashes` where it can, and at checkpoints elsewhere.
    ///
    /// `hints_gate` is `Some` while spentness hints may apply: the engine then
    /// commits nothing above genesis until the hints task opens it.
    ///
    /// With a memory budget, `memory_plan` bounds the engine's lookahead and
    /// receive windows (see [`resources::checkpoint_sync_engine_config`]).
    /// The engine publishes its lookahead charge in `charged`, which the
    /// memory watchdog reads.
    #[allow(clippy::too_many_arguments)]
    fn spawn_checkpoint_sync_engine<ZS, ZSTip>(
        config: &ZebradConfig,
        memory_plan: &resources::MemoryPlan,
        charged: checkpoint_sync::engine::Charged,
        peers: Option<zebra_network::SyncPeers>,
        ownership: Option<checkpoint_sync::EngineOwnership>,
        state: ZS,
        latest_chain_tip: ZSTip,
        known_hashes: Option<KnownHashListSpec>,
        hints_gate: Option<watch::Receiver<bool>>,
        header_tip: Option<watch::Receiver<Option<zebra_chain::block::Height>>>,
    ) -> tokio::task::JoinHandle<Result<(), Report>>
    where
        ZS: tower::Service<
                zebra_state::Request,
                Response = zebra_state::Response,
                Error = zebra_chain::BoxError,
            > + Send
            + Clone
            + 'static,
        ZS::Future: Send,
        ZSTip: zebra_chain::chain_tip::ChainTip + Clone + Send + 'static,
    {
        let (Some(peers), Some(ownership)) = (peers, ownership) else {
            return tokio::spawn(std::future::pending().in_current_span());
        };

        if config.network.v2_listen_addr.is_none() {
            warn!(
                "sync.v2_checkpoint_sync is set, but network.v2_listen_addr is not, \
                 so the checkpoint sync engine has no version 2 peers"
            );
        }

        if !config.network.v2_peer_discovery && config.network.initial_v2_peers.len() < 2 {
            warn!(
                initial_v2_peers = config.network.initial_v2_peers.len(),
                "sync.v2_checkpoint_sync is set, but network.v2_peer_discovery is off and \
                 fewer than two network.initial_v2_peers are listed, so the checkpoint sync \
                 engine only takes over once two version 2 peers that serve block ranges \
                 connect to this node"
            );
        }

        info!(
            known_hash_list_max_height = ?known_hashes.as_ref().map(|list| list.max_height),
            "spawning checkpoint sync engine"
        );
        let owner = ownership.owner.subscribe();
        let supervisor = checkpoint_sync::CheckpointSync::new(
            resources::checkpoint_sync_engine_config(config.sync.v2_engine_config(), memory_plan),
            config.network.network.clone(),
            peers,
            state,
            latest_chain_tip,
            ownership,
            hints_gate,
            known_hashes,
        )
        .with_charged(charged);
        let supervisor = match header_tip {
            Some(header_tip) => supervisor.with_headers_first(header_tip),
            None => supervisor,
        };

        tokio::spawn(
            async move {
                select! {
                    biased;
                    result = supervisor.run() => result,
                    never = record_sync_owner(owner) => match never {},
                }
            }
            .in_current_span(),
        )
    }

    /// Returns base services, disabling archive service and sidecars when pruning may occur.
    ///
    /// A pruned node that retains at least `NODE_NETWORK_LIMITED_BLOCKS` recent blocks advertises
    /// limited service instead, which only version 2 peers are told about. Pruning keeps at least
    /// the configured retention of recent bodies, so the advertisement holds while the node syncs.
    ///
    /// A node bootstrapped from a state snapshot has no bodies below the snapshot height, so it
    /// starts without limited service, and [`Self::limited_service_from`] says when it gains it.
    pub(super) fn network_services(
        config: &ZebradConfig,
        history: zebra_state::HistoryCapabilities,
    ) -> Result<zebra_network::types::PeerServices, Report> {
        let mut services = zebra_network::types::PeerServices::NODE_NETWORK;
        if config.state.prune_blocks_older_than.is_some() || history.is_pruned() {
            if config.zcashd_compat.enabled {
                return Err(eyre!(
                    "zcashd-compat requires an archive node; pruning is enabled or history is missing"
                ));
            }
            services.remove(zebra_network::types::PeerServices::NODE_NETWORK);
            if Self::retains_limited_blocks(config) && history.snapshot_base.is_none() {
                services.insert(zebra_network::types::PeerServices::NODE_NETWORK_LIMITED);
            }
        }
        Ok(services)
    }

    /// Returns whether the configured pruning retains at least the recent blocks that limited
    /// service promises.
    fn retains_limited_blocks(config: &ZebradConfig) -> bool {
        config
            .state
            .prune_blocks_older_than
            .is_some_and(|retention| {
                retention >= zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS
            })
    }

    /// Returns the best chain tip height from which a node bootstrapped from a state snapshot
    /// advertises limited service to version 2 peers, or `None` for any other node.
    ///
    /// The node has every body from the snapshot height `H`, so it holds the recent blocks
    /// limited service promises once its tip is `NODE_NETWORK_LIMITED_BLOCKS` above `H`. It waits
    /// until those blocks are finalized, `MAX_BLOCK_REORG_HEIGHT` more, because non-finalized
    /// blocks can be reorged away.
    pub(super) fn limited_service_from(
        config: &ZebradConfig,
        history: zebra_state::HistoryCapabilities,
    ) -> Option<Height> {
        let base = history.snapshot_base?;
        if !Self::retains_limited_blocks(config) {
            return None;
        }
        let blocks = zebra_network::constants::NODE_NETWORK_LIMITED_BLOCKS
            .checked_add(zebra_state::MAX_BLOCK_REORG_HEIGHT)?;
        base.height.0.checked_add(blocks).map(Height)
    }

    /// Starts the node, rejecting incompatible pruning settings before opening writable state.
    pub(super) async fn start(&self, config: Arc<ZebradConfig>) -> Result<(), Report> {
        check_tcp_slow_start_after_idle();

        // State initialization can prune immediately. Check the pruning config before starting
        // services; check persisted history separately once the state is open.
        Self::network_services(&config, zebra_state::HistoryCapabilities::default())?;
        config.notify.validate()?;
        // Refuses configured known-hash pins that the network does not take, or that are
        // inconsistent, before anything starts.
        let known_hashes = checkpoint_sync::known_hashes::known_hash_list(
            &config.network.network,
            config.sync.v2_known_hash_pins.as_ref(),
        )
        .map_err(|error| eyre!(error).wrap_err("invalid sync.v2_known_hash_pins"))?;

        #[cfg(feature = "zmq")]
        let zmq_socket = if let Some(endpoint) = config.notify.zmqpubhashblock.clone() {
            Some(notify::zmq::bind(endpoint).await?)
        } else {
            None
        };
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

        let zcashd_compat_block_gossip_peer_ips = if config.zcashd_compat.enabled {
            if config.zcashd_compat.block_gossip_peer_ips.is_empty() {
                // The sidecar privileges (pinned gossip, reserved slot, stall
                // exemption) match on the sidecar's *source* IP. In
                // cross-container/cross-host topologies that source is not
                // loopback, so the default list would silently strip the
                // sidecar of everything this mode provides.
                if config
                    .zcashd_compat
                    .p2p_connect_addr
                    .is_some_and(|addr| !addr.ip().is_loopback())
                {
                    warn!(
                        p2p_connect_addr = ?config.zcashd_compat.p2p_connect_addr,
                        "zcashd_compat.p2p_connect_addr is not loopback, but \
                         zcashd_compat.block_gossip_peer_ips defaults to loopback only; \
                         if the sidecar connects from a non-loopback IP, set \
                         block_gossip_peer_ips to that IP or it will not receive \
                         pinned block gossip"
                    );
                }

                Self::zcashd_compat_default_block_gossip_peer_ips()
            } else {
                config.zcashd_compat.block_gossip_peer_ips.clone()
            }
        } else {
            Vec::new()
        };

        if config.zcashd_compat.enabled {
            // Preflight does blocking filesystem and /proc reads, and can hash
            // the cached zcashd binary, so keep it off the async runtime.
            let preflight_config = config.clone();
            let unsafe_low_specs = self.unsafe_low_specs;
            tokio::task::spawn_blocking(move || {
                zcashd_compat::run_preflight(&preflight_config, unsafe_low_specs)
            })
            .await
            .map_err(|err| eyre!("failed to join zcashd-compat preflight task: {err}"))??;
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

        // Derive the memory plan before snapshot networking and the database open, because both
        // use its limits. An "auto" budget reads /proc and /sys/fs/cgroup on this blocking task.
        let resources_config = config.clone();
        let unsafe_low_specs = self.unsafe_low_specs;
        let resources::MemoryResources {
            settings: memory_settings,
            plan_sender: memory_plan_sender,
        } = tokio::task::spawn_blocking(move || {
            resources::init(&resources_config, unsafe_low_specs)
        })
        .await
        .map_err(|err| eyre!("failed to join the memory budget task: {err}"))??;
        let mut memory_plan = memory_plan_sender.borrow().clone();
        let snapshot_memory = crate::components::state_snapshot::memory::MemoryLimits::from_plan(
            &memory_plan,
            resources::node_cores(&config),
        );

        // A pruned node that bootstraps from a state snapshot starts like any other node: it opens
        // its state, starts the network, and syncs and verifies headers from genesis. Then it
        // checks the snapshot against those headers, downloads it, and replaces its header-only
        // state with it, keeping the headers (see `snapshot_headers`). It commits no block above
        // genesis before that, so it downloads no body it won't keep.
        let snapshot_store = zebra_state::state_snapshot::store::ArtifactStore::new(
            &config.state,
            &config.network.network,
        );
        let snapshot_decision = decision::should_bootstrap(&config, &snapshot_store);
        Self::handle_rejected_snapshot(&config, &snapshot_decision)?;
        let header_sync_marker =
            decision::header_sync_marker(&config.state, &config.network.network);
        let mut snapshot_plan = match snapshot_decision {
            Decision::Bootstrap { commitment, peers } => Some((commitment, peers)),
            Decision::Skip(_) => None,
        };
        // The marker is written before a new state is created, so a later start can still
        // bootstrap if this one stops before it bootstraps or gives up, and is deleted before
        // normal sync commits blocks.
        Self::set_header_sync_marker(&header_sync_marker, snapshot_plan.is_some())?;

        // With any hints mode, a node with version 2 enabled serves the saved
        // spentness-hint artifact to its version 2 peers. The state snapshot store's
        // objects are added to the same index, which the network serves `get-object`
        // requests from.
        let (served_objects, object_source) = spentness_hints::object_source(&config.network)
            .or_else(|| {
                config.sync.serve_state_snapshots.then(|| {
                    zebra_network::ObjectSource::channel(
                        Arc::new(zebra_network::ObjectIndex::new()),
                    )
                })
            })
            .unzip();

        info!("initializing node state");
        let (checkpoint_list, max_checkpoint_height) =
            zebra_consensus::router::init_checkpoint_list(
                config.consensus.clone(),
                &config.network.network,
            );

        info!("opening database, this may take a few minutes");

        // With spentness hints, the state's writer can receive a verified artifact
        // after the network starts.
        let spentness_hints_mode = spentness_hints::effective_mode(
            config.sync.v2_spentness_hints,
            config.sync.v2_checkpoint_sync,
        )?;
        let (
            mut state_hints,
            state_service,
            mut read_only_state_service,
            mut latest_chain_tip,
            mut chain_tip_change,
        ) = Self::open_state(
            &config,
            max_checkpoint_height,
            spentness_hints_mode,
            memory_plan.state,
        )
        .await;

        // A state with a block above genesis is not header-only: an earlier start installed a
        // snapshot or gave up on it, but stopped before it deleted the marker.
        if snapshot_plan.is_some()
            && latest_chain_tip
                .best_tip_height()
                .is_some_and(|height| height > Height(0))
        {
            info!(
                "the state database is not header-only, so the state snapshot bootstrap does not \
                 run"
            );
            snapshot_plan = None;
            Self::set_header_sync_marker(&header_sync_marker, false)?;
        }

        // A tip-only budget cannot sync to the last checkpoint, and a node already past it
        // needs no initial sync share (memory page, "How the budget is split").
        if let Some(settings) = &memory_settings {
            let (profile, warning) = settings.profile_after_open(
                memory_plan.profile,
                latest_chain_tip.best_tip_height(),
                max_checkpoint_height,
            )?;
            if let Some(warning) = warning {
                warn!("{warning}");
            }

            if profile != memory_plan.profile {
                let (plan, _) = settings.plan(profile)?;
                info!(
                    "memory profile changed: {} → {profile}: the state is at the last checkpoint",
                    memory_plan.profile,
                );
                memory_plan_sender.send_replace(plan.clone());
                read_only_state_service
                    .set_non_finalized_resident_bytes(plan.state.non_finalized_resident_bytes);
                read_only_state_service.set_queued_blocks_bytes(plan.state.queued_blocks_bytes);
                if tokio::time::timeout(
                    resources::watchdog::BLOCKING_TIMEOUT,
                    read_only_state_service.spawn_apply_memory_limits(plan.state.rocksdb),
                )
                .await
                .is_err()
                {
                    warn!("resizing the state database took too long, continuing startup");
                }
                memory_plan = plan;
            }
        }

        // The network starts before the bootstrap, so a node that plans to bootstrap advertises
        // the services of the snapshot node it will be, including if it gives up and syncs
        // normally.
        let history = match &snapshot_plan {
            Some((commitment, _)) => Self::planned_snapshot_history(commitment),
            None => Self::history_capabilities(&read_only_state_service).await?,
        };
        let local_services = Self::network_services(&config, history)?;
        let limited_service_from = Self::limited_service_from(&config, history);

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
        //
        // The inbound service gets the read-only state in its setup data, because a bootstrap
        // replaces the state the network starts with.
        let (setup_tx, setup_rx) = oneshot::channel();
        let submitted_blocks = SubmittedBlockCache::default();
        let inbound = ServiceBuilder::new()
            .load_shed()
            .buffer(inbound::downloads::MAX_INBOUND_CONCURRENCY)
            .timeout(MAX_INBOUND_RESPONSE_TIME)
            .service(
                Inbound::new(config.sync.full_verify_concurrency_limit, setup_rx)
                    .with_submitted_blocks(submitted_blocks.clone()),
            );

        // The checkpoint sync engine streams blocks from the version 2 connections
        // that this registry collects, and the bootstrap finds peers that serve state snapshots
        // in it.
        let (v2_sync_peers, sync_peers) =
            if config.sync.v2_checkpoint_sync || snapshot_plan.is_some() {
                let (sender, receiver) = zebra_network::SyncPeersSender::spawn();
                (Some(sender), Some(receiver))
            } else {
                (None, None)
            };
        let network_config = zebra_network::Config {
            v2_sync_peers,
            object_source,
            v2_limited_service_from: limited_service_from,
            ..config.network.clone()
        };

        // The network follows the chain tip of the state that is open.
        let (network_tip, network_chain_tip) = snapshot_headers::ReopenableChainTip::new(
            latest_chain_tip.clone(),
            snapshot_plan.is_some(),
        );
        let (peer_set, address_book, misbehavior_sender, block_advertiser) =
            zebra_network::init_with_limits(
                network_config,
                local_services,
                inbound,
                network_chain_tip,
                user_agent(),
                zcashd_compat_block_gossip_peer_ips,
                memory_plan.network,
            )
            .await;

        let mut state = ServiceBuilder::new()
            .buffer(Self::state_buffer_bound(&config))
            .service(state_service);

        // Headers-first sync stores the verified header chain ahead of the bodies, and runs for a
        // bootstrap even without `sync.headers_first`.
        let mut header_sync = (config.sync.headers_first || snapshot_plan.is_some())
            .then(|| Self::spawn_header_sync(&config, peer_set.clone(), state.clone()));

        let mut bootstrapped_height = None;
        let mut bootstrapped_successor = None;
        if let Some((commitment, peers)) = snapshot_plan {
            let (_, header_tip) = header_sync
                .as_ref()
                .expect("headers-first sync runs for a bootstrap");
            let staged = snapshot_headers::sync_headers_and_stage(
                &config,
                commitment,
                peers,
                peer_set.clone(),
                state.clone(),
                read_only_state_service.clone(),
                latest_chain_tip.clone(),
                header_tip.clone(),
                sync_peers
                    .clone()
                    .expect("the peer registry is spawned for a bootstrap"),
                &snapshot_memory,
            )
            .await;

            let installed = match staged {
                Ok(staged) => {
                    // The header-only state is closed, so the installation can carry its headers
                    // into the snapshot and replace it, and then the node opens the snapshot.
                    if let Some((header_sync_task, _)) = header_sync.take() {
                        header_sync_task.abort();
                        let _ = header_sync_task.await;
                    }
                    Self::close_state((
                        state_hints,
                        state,
                        read_only_state_service,
                        latest_chain_tip,
                        chain_tip_change,
                    ))
                    .await;

                    let installed = staged.install().await;

                    let state_service;
                    (
                        state_hints,
                        state_service,
                        read_only_state_service,
                        latest_chain_tip,
                        chain_tip_change,
                    ) = Self::open_state(
                        &config,
                        max_checkpoint_height,
                        spentness_hints_mode,
                        memory_plan.state,
                    )
                    .await;
                    state = ServiceBuilder::new()
                        .buffer(Self::state_buffer_bound(&config))
                        .service(state_service);
                    network_tip.reopen(latest_chain_tip.clone());
                    header_sync = config
                        .sync
                        .headers_first
                        .then(|| Self::spawn_header_sync(&config, peer_set.clone(), state.clone()));

                    installed
                }
                Err(error) => Err(error),
            };

            match installed {
                Ok(bootstrapped) => {
                    bootstrapped_height = Some(commitment.height);
                    bootstrapped_successor = bootstrapped.successor;
                    Self::set_header_sync_marker(&header_sync_marker, false)?;
                }
                Err(error) => {
                    let fallback = config.sync.state_snapshot_fallback;
                    let rejected = matches!(error, bootstrap::BootstrapError::Rejected(_));
                    if fallback.exits(config.sync.state_snapshot.is_some(), rejected) {
                        return Err(Report::new(error).wrap_err(format!(
                            "the state snapshot bootstrap failed and sync.state_snapshot_fallback \
                             is \"{}\", so Zebra exits instead of continuing with normal \
                             sync. Check sync.state_snapshot_peers and the commitment. An aborted \
                             download is retried on the next start; a rejected snapshot needs a \
                             different trusted commitment, or investigation and removal of its \
                             rejection marker before it is retried. Set \
                             sync.state_snapshot_fallback = \"genesis\" to sync from genesis, or \
                             sync.state_snapshot_sync = false to turn snapshot sync off",
                            fallback.name()
                        )));
                    }
                    warn!(
                        %error,
                        fallback = fallback.name(),
                        "the state snapshot bootstrap failed: syncing normally from the synced \
                         headers instead"
                    );
                    Self::set_header_sync_marker(&header_sync_marker, false)?;
                }
            }
        }
        network_tip.end_bootstrap();
        // Headers-first sync only ran for the bootstrap.
        if !config.sync.headers_first {
            if let Some((header_sync_task, _)) = header_sync.take() {
                header_sync_task.abort();
            }
        }

        // Serving scans the artifact store on a blocking thread, and publishes its objects in the
        // index once the scan finishes. The network does not wait for it: every node with an
        // object source advertises `NODE_SYNC_ARTIFACTS`, and answers the objects it does not hold
        // yet as not found. The scan starts after the bootstrap, so a snapshot downloaded just
        // now is in the index.
        if let Some(served_objects) = served_objects
            .clone()
            .filter(|_| config.sync.serve_state_snapshots)
        {
            info!(store = %snapshot_store.root().display(), "scanning the state snapshot store to serve");
            serving::object_index(
                snapshot_store.clone(),
                config.network.network.clone(),
                served_objects,
            );
        }

        // The bootstrap checked block `H + 1` against the headers to the next checkpoint and the
        // snapshot's history tree, so commit it before the block verifier and sync start: they
        // then start above it, and nothing downloads it again.
        if let Some(successor) = bootstrapped_successor {
            Self::commit_snapshot_successor(&mut state, successor).await;
        }
        let node_hints = state_hints.and_then(|state_hints| {
            state_hints.for_state(
                &latest_chain_tip,
                read_only_state_service.has_incomplete_spentness_hints(),
                read_only_state_service.incomplete_spentness_commitment(),
            )
        });

        let state_tip_height = latest_chain_tip
            .best_tip_height()
            .unwrap_or(zebra_chain::block::Height(0));
        if memory_plan.sync.pipeline_bytes.is_some()
            && config.consensus.checkpoint_sync
            && state_tip_height < max_checkpoint_height
        {
            sync::check_checkpoint_gaps(&checkpoint_list, max_checkpoint_height, state_tip_height)?;
        }

        let history = Self::history_capabilities(&read_only_state_service).await?;
        let snapshot_height = history
            .snapshot_base
            .map(|base| base.height)
            .or(bootstrapped_height);
        if let Some(snapshot_height) = snapshot_height {
            info!(
                snapshot_height = snapshot_height.0,
                tip_height = ?latest_chain_tip.best_tip_height().map(|height| height.0),
                "{}",
                Stage::StateInitialized
            );
        }
        if let Some(height) = limited_service_from {
            info!(
                height = height.0,
                "advertising limited service to version 2 peers from this chain tip height, once \
                 the recent blocks above the snapshot height are committed"
            );
        }

        info!("logging database metrics on startup");
        read_only_state_service.log_db_metrics();

        let early_blocks = if config.network.experimental_early_block_relay {
            match read_only_state_service
                .clone()
                .oneshot(zebra_state::ReadRequest::EarlyRelayCache)
                .await
                .map_err(|error| eyre!(error))?
            {
                zebra_state::ReadResponse::EarlyRelayCache(cache) => Some(cache),
                _ => unreachable!("state returns the early-relay cache for its cache request"),
            }
        } else {
            None
        };

        // Spentness hints need the checkpoint sync engine (`effective_mode`), so they
        // share its peer registry, and the engine commits nothing above genesis until
        // the hints task opens the gate.
        let hints_gate = node_hints.as_ref().map(spentness_hints::NodeHints::gate);
        let legacy_hints_gate = node_hints
            .as_ref()
            .map(spentness_hints::NodeHints::legacy_gate);
        let mut spentness_hints_task_handle = match (node_hints, sync_peers.clone()) {
            (Some(node_hints), Some(peers)) => node_hints.spawn(
                config.network.network.clone(),
                peers,
                config.state.cache_dir.clone(),
                served_objects,
            ),
            _ => {
                // Without a hints task, a short task serves the saved artifact.
                if let Some(served_objects) = served_objects {
                    let _serving_task = spentness_hints::spawn_serving(
                        config.network.network.clone(),
                        config.state.cache_dir.clone(),
                        served_objects,
                    );
                }
                tokio::spawn(std::future::pending().in_current_span())
            }
        };

        // Start health server if configured (after sync_status is available)

        // The checkpoint sync engine and the syncer share block download through
        // these channels. Without the engine, the syncer always owns it, and the
        // checkpoint verifier's external tip never changes.
        let (engine_ownership, legacy_ownership, checkpoint_tip, _inert_checkpoint_tip) =
            if config.sync.v2_checkpoint_sync {
                let (engine, legacy, checkpoint_tip) = checkpoint_sync::ownership_channels();
                (Some(engine), Some(legacy), checkpoint_tip, None)
            } else {
                let (sender, receiver) = watch::channel(None);
                (None, None, receiver, Some(sender))
            };

        info!("initializing verifiers");
        let (tx_verifier_setup_tx, tx_verifier_setup_rx) = oneshot::channel();
        let (block_verifier_setup_tx, block_verifier_setup_rx) = oneshot::channel();
        let (
            block_verifier_router,
            tx_verifier,
            consensus_task_handles,
            max_checkpoint_height,
            prepared_blocks,
        ) = zebra_consensus::router::init_with_limits(
            config.consensus.clone(),
            &config.network.network,
            state.clone(),
            read_only_state_service.clone(),
            tx_verifier_setup_rx,
            Some(block_verifier_setup_rx),
            memory_plan.proposal_cache_bytes,
            Some(checkpoint_tip),
        )
        .await;

        let engine_charged = checkpoint_sync::engine::Charged::default();
        // Headers-first sync publishes its header chain tip, and the checkpoint
        // sync engine downloads no body above it.
        let (header_sync_task_handle, header_tip) = match header_sync {
            Some((header_sync_task, header_tip)) => (header_sync_task, Some(header_tip)),
            None => (tokio::spawn(std::future::pending().in_current_span()), None),
        };

        let mut checkpoint_sync_task_handle = Self::spawn_checkpoint_sync_engine(
            &config,
            &memory_plan,
            engine_charged.clone(),
            sync_peers,
            engine_ownership,
            state.clone(),
            latest_chain_tip.clone(),
            known_hashes,
            hints_gate,
            header_tip,
        );

        info!("initializing syncer");
        let (syncer, sync_status) = ChainSync::new_with_limits(
            &config,
            max_checkpoint_height,
            peer_set.clone(),
            block_verifier_router.clone(),
            state.clone(),
            read_only_state_service.clone(),
            latest_chain_tip.clone(),
            misbehavior_sender.clone(),
            checkpoint_list,
            memory_plan.sync,
        );
        let syncer = match legacy_ownership {
            Some(ownership) => syncer.with_ownership(ownership),
            None => syncer,
        };
        // The legacy syncer commits nothing above genesis either, until the hints
        // task finishes: the engine claims block download only after genesis.
        let mut syncer = match legacy_hints_gate {
            Some((gate, timeout)) => syncer.with_hints_gate(gate, timeout),
            None => syncer,
        };

        info!("initializing mempool");
        // The mempool reads its cost limit from its config, so a budget's limit replaces it in a
        // copy (memory page, "How the budget is split"); an explicit limit has already won in the
        // plan (memory page, "Settings that win over the budget").
        let mempool_config = memory_plan.mempool.apply_to(&config.mempool);
        let (mempool, mempool_transaction_subscriber, mempool_transaction_verified) = Mempool::new(
            &config.network.network,
            &mempool_config,
            peer_set.background_service(),
            state.clone(),
            tower::util::BoxCloneService::new(read_only_state_service.clone()),
            tx_verifier,
            sync_status.clone(),
            latest_chain_tip.clone(),
            chain_tip_change.clone(),
            misbehavior_sender.clone(),
        );

        // Subscribe as soon as possible to not miss any events
        let mempool_change_receiver = mempool_transaction_subscriber.subscribe();

        let mempool = BoxService::new(mempool);
        let mempool = ServiceBuilder::new()
            .buffer(mempool::downloads::MAX_INBOUND_CONCURRENCY)
            .service(mempool);

        if tx_verifier_setup_tx.send(mempool.clone()).is_err() {
            warn!("error setting up the transaction verifier with a handle to the mempool service");
        };
        if block_verifier_setup_tx
            .send(tower::util::BoxCloneService::new(mempool.clone()))
            .is_err()
        {
            warn!("error setting up the block verifier with a handle to the mempool service");
        }

        info!("fully initializing inbound peer request handler");
        // Fully start the inbound service as soon as possible
        let setup_data = InboundSetupData {
            address_book: address_book.clone(),
            block_download_peer_set: peer_set.background_service(),
            block_verifier: block_verifier_router.clone(),
            mempool: mempool.clone(),
            state: state.clone(),
            latest_chain_tip: latest_chain_tip.clone(),
            misbehavior_sender,
            pipeline_budget: syncer.pipeline_budget(),
            read_state: Some(inbound::InboundReadState {
                network: config.network.network.clone(),
                read_state: read_only_state_service.clone(),
                early_blocks: early_blocks.clone(),
            }),
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
        let rpc_impl = rpc_impl
            .with_submitted_blocks(submitted_blocks.clone())
            .with_prepared_blocks(prepared_blocks);
        let rpc_impl = rpc_impl.with_end_of_support_height(
            sync::end_of_support::end_of_support_height(&config.network.network),
        );

        // Keep a block template ready for the `getblocktemplate` RPC, if this node is configured
        // for mining, and something can actually ask for a template. Without the RPC server and
        // without the internal miner, nothing can call `getblocktemplate`, so precomputing
        // templates would build coinbase transactions that no one reads.
        #[cfg(feature = "internal-miner")]
        let is_internal_miner_enabled = config.mining.is_internal_miner_enabled();
        #[cfg(not(feature = "internal-miner"))]
        let is_internal_miner_enabled = false;

        let block_template_task_handle =
            if config.rpc.listen_addr.is_some() || is_internal_miner_enabled {
                rpc_impl
                    .spawn_block_template_updater(&mempool_transaction_subscriber)
                    .inspect(|_| info!("spawned block template updater task"))
            } else {
                None
            };

        // Supervise the updater like every other ongoing task: if it exits or panics, the RPC
        // keeps serving the last template it published, and pays the new-tip timeout on every call
        // after the next tip change, so a silent exit has to be visible.
        let block_template_task_handle: tokio::task::JoinHandle<()> = block_template_task_handle
            .unwrap_or_else(|| tokio::spawn(std::future::pending().in_current_span()));

        let rpc_task_handle = if config.rpc.listen_addr.is_some() {
            RpcServer::start(rpc_impl.clone(), config.rpc.clone())
                .await
                .expect("server should start")
        } else {
            tokio::spawn(std::future::pending().in_current_span())
        };

        let zcashd_compat_shutdown_timeout =
            Self::zcashd_compat_supervisor_shutdown_timeout(&config);
        let (zcashd_compat_shutdown_tx, zcashd_compat_shutdown_rx) = watch::channel(false);
        let mut zcashd_compat_task_handle = if let Some(resolved_zcashd_path) = resolved_zcashd_path
        {
            let local_listener = address_book
                .lock()
                .expect("unexpected panic in address book mutex guard")
                .local_listener_socket_addr();
            let supervisor_config = zcashd_compat::SupervisorConfig::new(
                &config.zcashd_compat,
                resolved_zcashd_path,
                &config.state.cache_dir,
                config.network.network.kind(),
                Self::zcashd_compat_p2p_connect_addr(&config, local_listener),
            );

            info!(
                connect = %supervisor_config.zebra_p2p_addr,
                "zcashd-compat mode enabled"
            );

            tokio::spawn(
                zcashd_compat::run_supervisor(supervisor_config, zcashd_compat_shutdown_rx)
                    .in_current_span(),
            )
        } else {
            if config.zcashd_compat.enabled {
                zcashd_compat::set_supervision_config_disabled_metrics();
                info!("zcashd-compat mode enabled: zcashd supervision disabled");
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

        let lightwalletd_rpc_task_handle = {
            if let Some(lightwalletd_listen_addr) = config.rpc.lightwalletd_listen_addr {
                info!("spawning lightwalletd gRPC server");
                let (lightwalletd_rpc_task_handle, _listen_addr) =
                    zebra_rpc::lightwalletd::server::init(
                        lightwalletd_listen_addr,
                        rpc_impl.clone(),
                        read_only_state_service.clone(),
                        mempool.clone(),
                        latest_chain_tip.clone(),
                        mempool_transaction_subscriber.clone(),
                        config.network.network.clone(),
                    )
                    .await
                    .map_err(|err| eyre!(err))?;

                lightwalletd_rpc_task_handle
            } else {
                tokio::spawn(std::future::pending().in_current_span())
            }
        };

        // Start concurrent tasks which don't add load to other tasks
        info!("spawning block gossip task");
        let committed_gossip = sync::gossip_best_tip_block_hashes(
            sync_status.clone(),
            chain_tip_change.clone(),
            block_advertiser.clone(),
            Some(submit_block_channel.receiver()),
            submitted_blocks,
        );
        let early_gossip = sync::gossip_early_blocks(early_blocks, block_advertiser);
        let block_gossip_task_handle = tokio::spawn(
            async move {
                tokio::try_join!(committed_gossip, early_gossip)?;
                Ok::<_, sync::BlockGossipError>(())
            }
            .in_current_span(),
        );

        info!("spawning block notify task");
        let block_notify_task_handle: tokio::task::JoinHandle<Result<(), BlockNotifyError>> =
            if let Some(command) = config.notify.block_notify_command.clone() {
                tokio::spawn(
                    notify::run_block_notify(
                        command,
                        sync_status.clone(),
                        chain_tip_change.clone(),
                    )
                    .in_current_span(),
                )
            } else {
                tokio::spawn(std::future::pending().in_current_span())
            };

        let zmq_task_handle: tokio::task::JoinHandle<Result<(), Report>> = {
            #[cfg(feature = "zmq")]
            {
                if let Some(socket) = zmq_socket {
                    tokio::spawn(
                        notify::zmq::run(socket, sync_status.clone(), chain_tip_change.clone())
                            .in_current_span(),
                    )
                } else {
                    tokio::spawn(std::future::pending().in_current_span())
                }
            }
            #[cfg(not(feature = "zmq"))]
            tokio::spawn(std::future::pending().in_current_span())
        };

        info!("spawning mempool queue checker task");
        let mempool_queue_checker_task_handle =
            mempool::QueueChecker::spawn(mempool.clone(), mempool_transaction_verified);

        info!("spawning mempool transaction gossip task");
        let tx_gossip_task_handle = tokio::spawn(
            mempool::gossip_mempool_transaction_id(mempool_change_receiver, peer_set.clone())
                .in_current_span(),
        );

        info!("spawning memory watchdog task");
        // Nothing reads the memory pressure yet: pausing downloads at critical pressure is phase
        // 2 (memory page, "How the budget is split").
        let (memory_pressure_sender, _) =
            watch::channel(resources::watchdog::MemoryPressure::default());
        let sampled_state = read_only_state_service.clone();
        let resized_state = read_only_state_service.clone();
        let memory_task_handle = tokio::spawn(
            resources::watchdog::run(
                resources::watchdog::Watchdog::new(
                    memory_settings,
                    memory_plan_sender,
                    max_checkpoint_height,
                )
                .with_pipeline_budget(syncer.pipeline_budget())
                .with_engine_charged(engine_charged),
                memory_pressure_sender,
                latest_chain_tip.clone(),
                move || resources::watchdog::StateMemorySample {
                    rocksdb: sampled_state.memory_sample(),
                    non_finalized_resident_bytes: sampled_state.non_finalized_resident_bytes(),
                },
                move |limits| {
                    resized_state
                        .set_non_finalized_resident_bytes(limits.non_finalized_resident_bytes);
                    resized_state.set_queued_blocks_bytes(limits.queued_blocks_bytes);
                    let resized_state = resized_state.clone();
                    async move {
                        // A failure is logged, and the database keeps its current limits.
                        let _ = resized_state
                            .spawn_apply_memory_limits(limits.rocksdb)
                            .await;
                    }
                },
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

        // Like the health endpoint, this task only reports metrics, so Zebra doesn't stop when it
        // exits.
        info!("spawning tip block delay metrics task");
        let tip_delay_task_handle =
            tokio::spawn(record_tip_block_delay(chain_tip_change.clone()).in_current_span());

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

        // On a snapshot state, log the sync stages above the snapshot height.
        let snapshot_stages_task_handle = if let Some(snapshot_height) = snapshot_height {
            let stages = RuntimeStages::new(
                snapshot_height,
                max_checkpoint_height,
                config.state.debug_stop_at_height.map(Height),
            );
            tokio::spawn(
                progress::log_runtime_stages(stages, chain_tip_change.clone()).in_current_span(),
            )
        } else {
            tokio::spawn(std::future::pending().in_current_span())
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
        pin!(indexer_rpc_task_handle);
        pin!(lightwalletd_rpc_task_handle);
        pin!(syncer_task_handle);
        pin!(header_sync_task_handle);
        pin!(block_gossip_task_handle);
        pin!(block_notify_task_handle);
        pin!(zmq_task_handle);
        pin!(mempool_crawler_task_handle);
        pin!(mempool_queue_checker_task_handle);
        pin!(tx_gossip_task_handle);
        pin!(progress_task_handle);
        pin!(end_of_support_task_handle);
        pin!(miner_task_handle);
        pin!(block_template_task_handle);
        pin!(memory_task_handle);

        // startup tasks
        let BackgroundTaskHandles {
            mut state_checkpoint_verify_handle,
            mut halo2_key_preload_handle,
        } = consensus_task_handles;

        let state_checkpoint_verify_handle_fused = (&mut state_checkpoint_verify_handle).fuse();
        pin!(state_checkpoint_verify_handle_fused);

        let halo2_key_preload_handle_fused = (&mut halo2_key_preload_handle).fuse();
        pin!(halo2_key_preload_handle_fused);

        let old_databases_task_handle_fused = (&mut old_databases_task_handle).fuse();
        pin!(old_databases_task_handle_fused);

        // Spentness-hint acquisition finishes while Zebra is running, so its handle
        // must be fused.
        let spentness_hints_task_handle_fused = (&mut spentness_hints_task_handle).fuse();
        pin!(spentness_hints_task_handle_fused);

        // The checkpoint sync engine can exit while Zebra keeps running, so its handle must be
        // fused.
        let checkpoint_sync_task_handle_fused = (&mut checkpoint_sync_task_handle).fuse();
        pin!(checkpoint_sync_task_handle_fused);

        // The zcashd-compat supervisor exits when supervision is disabled or fails,
        // but Zebra keeps running, so its handle must be fused.
        let mut zcashd_compat_task_finished = false;
        let zcashd_compat_task_handle_fused = (&mut zcashd_compat_task_handle).fuse();
        pin!(zcashd_compat_task_handle_fused);

        // Wait for tasks to finish
        let exit_status = loop {
            let mut exit_when_task_finishes = true;

            let result = select! {
                rpc_join_result = &mut rpc_task_handle => {
                    let rpc_server_result = rpc_join_result
                        .expect("unexpected panic in the rpc task");
                    info!(?rpc_server_result, "rpc task exited");
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

                lightwalletd_rpc_join_result = &mut lightwalletd_rpc_task_handle => {
                    let lightwalletd_rpc_server_result = lightwalletd_rpc_join_result
                        .expect("unexpected panic in the lightwalletd gRPC task");
                    info!(?lightwalletd_rpc_server_result, "lightwalletd gRPC task exited");
                    Ok(())
                }

                sync_result = &mut syncer_task_handle => sync_result
                    .expect("unexpected panic in the syncer task")
                    .map(|_| info!("syncer task exited")),

                header_sync_result = &mut header_sync_task_handle => header_sync_result
                    .expect("unexpected panic in the headers-first sync task")
                    .map(|_| info!("headers-first sync task exited")),

                // The checkpoint sync engine hands block download back to the syncer
                // before it exits, so Zebra keeps running without it.
                checkpoint_sync_result = &mut checkpoint_sync_task_handle_fused => {
                    match checkpoint_sync_result
                        .expect("unexpected panic in the checkpoint sync task")
                    {
                        Ok(()) => info!("checkpoint sync task exited"),
                        Err(error) => warn!(
                            ?error,
                            "checkpoint sync task failed, the syncer downloads every block"
                        ),
                    }

                    exit_when_task_finishes = false;
                    Ok(())
                }

                block_gossip_result = &mut block_gossip_task_handle => block_gossip_result
                    .expect("unexpected panic in the chain tip block gossip task")
                    .map(|_| info!("chain tip block gossip task exited"))
                    .map_err(|e| eyre!(e)),

                block_notify_result = &mut block_notify_task_handle => block_notify_result
                    .expect("unexpected panic in the block notify task")
                    .map(|_| info!("block notify task exited"))
                    .map_err(|e| eyre!(e)),

                zmq_result = &mut zmq_task_handle => zmq_result
                    .expect("unexpected panic in the ZMQ hashblock task")
                    .map(|_| info!("ZMQ hashblock task exited")),

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

                halo2_key_preload_result = &mut halo2_key_preload_handle_fused => {
                    exit_when_task_finishes =
                        Self::halo2_key_preload_should_exit(halo2_key_preload_result);
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

                // Acquisition ends with or without hints. It fails when hints are
                // required, or when an interrupted hinted sync uses an artifact this
                // release does not pin, which stops Zebra.
                spentness_hints_result = &mut spentness_hints_task_handle_fused => {
                    exit_when_task_finishes = false;
                    spentness_hints_result
                        .expect("unexpected panic in the spentness hints task")
                        .map(|()| info!("spentness hints task finished"))
                }

                block_template_result = &mut block_template_task_handle => {
                    block_template_result
                        .expect("unexpected panic in the block template updater task");
                    info!("block template updater task exited");

                    Ok(())
                }

                miner_result = &mut miner_task_handle => miner_result
                    .expect("unexpected panic in the miner task")
                    .map(|_| info!("miner task exited")),

                memory_result = &mut memory_task_handle => memory_result
                    .expect("unexpected panic in the memory watchdog task")
                    .map(|_| info!("memory watchdog task exited")),

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
        };

        info!("exiting Zebra because an ongoing task exited: asking other tasks to stop");

        // ongoing tasks
        rpc_task_handle.abort();
        rpc_tx_queue_handle.abort();
        block_template_task_handle.abort();
        health_task_handle.abort();
        tip_delay_task_handle.abort();
        syncer_task_handle.abort();
        header_sync_task_handle.abort();
        checkpoint_sync_task_handle.abort();
        block_gossip_task_handle.abort();
        block_notify_task_handle.abort();
        zmq_task_handle.abort();
        mempool_crawler_task_handle.abort();
        mempool_queue_checker_task_handle.abort();
        tx_gossip_task_handle.abort();
        progress_task_handle.abort();
        end_of_support_task_handle.abort();
        miner_task_handle.abort();
        spentness_hints_task_handle.abort();
        snapshot_stages_task_handle.abort();
        memory_task_handle.abort();
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
        // This only cancels queued work; an already running blocking task finishes at shutdown.
        halo2_key_preload_handle.abort();
        old_databases_task_handle.abort();

        info!(
            "exiting Zebra: all tasks have been asked to stop, waiting for remaining tasks to finish"
        );

        exit_status
    }

    /// Commits the checked block after the snapshot height, which the bootstrap fetched, as a
    /// checkpoint-verified block. If the commit fails, block sync downloads the block instead.
    async fn commit_snapshot_successor<S>(state: &mut S, successor: Arc<zebra_chain::block::Block>)
    where
        S: tower::Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        >,
    {
        /// How long the commit of one checked block may take.
        const SUCCESSOR_COMMIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

        let hash = successor.hash();
        let height = successor.coinbase_height().map(|height| height.0);
        let request = zebra_state::Request::CommitCheckpointVerifiedBlock(
            zebra_state::CheckpointVerifiedBlock::from(successor),
        );
        let commit = async { tower::Service::call(state.ready().await?, request).await };
        match tokio::time::timeout(SUCCESSOR_COMMIT_TIMEOUT, commit).await {
            Ok(Ok(_)) => info!(
                ?height,
                %hash,
                "committed the block after the snapshot height that the bootstrap checked"
            ),
            Ok(Err(error)) => warn!(
                ?height,
                %hash,
                %error,
                "could not commit the block after the snapshot height that the bootstrap \
                 checked: block sync downloads it instead"
            ),
            Err(_) => warn!(
                ?height,
                %hash,
                "committing the block after the snapshot height that the bootstrap checked \
                 timed out: block sync downloads it instead"
            ),
        }
    }

    /// Creates the marker that says the state holds only the headers synced for a state snapshot
    /// bootstrap when `header_only` is set, and deletes it otherwise.
    fn set_header_sync_marker(marker: &std::path::Path, header_only: bool) -> Result<(), Report> {
        let result = if header_only {
            marker
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(marker, []))
        } else {
            match std::fs::remove_file(marker) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        };

        result.map_err(|error| {
            eyre!(error).wrap_err(format!(
                "could not update the header-only state marker {}",
                marker.display()
            ))
        })
    }

    /// Returns the history of the state that a bootstrap from `commitment` installs, with every
    /// header from genesis.
    fn planned_snapshot_history(
        commitment: &zebra_chain::state_snapshot::StateSnapshotCommitment,
    ) -> zebra_state::HistoryCapabilities {
        let window_start = zebra_state::state_snapshot::import::window_start(commitment.height);
        zebra_state::HistoryCapabilities {
            lowest_retained_height: commitment.height,
            lowest_known_height: window_start,
            lowest_header_height: Height::MIN,
            snapshot_base: Some(zebra_state::SnapshotBase {
                window_start,
                height: commitment.height,
                block_hash: commitment.block_hash,
                manifest: commitment.manifest,
                deferred_checks: 0,
            }),
        }
    }

    /// Reads the history capabilities of the open state.
    async fn history_capabilities(
        read_state: &zebra_state::ReadStateService,
    ) -> Result<zebra_state::HistoryCapabilities, Report> {
        let zebra_state::ReadResponse::HistoryCapabilities(history) = read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::HistoryCapabilities)
            .await
            .map_err(|error| eyre!(error))?
        else {
            unreachable!("state returns history capabilities for its capabilities request");
        };

        Ok(history)
    }

    /// Spawns headers-first sync, and returns its task and its header chain tip.
    fn spawn_header_sync(
        config: &ZebradConfig,
        peer_set: zebra_network::NetworkService,
        state: tower::buffer::Buffer<
            BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
            zebra_state::Request,
        >,
    ) -> (
        tokio::task::JoinHandle<Result<(), Report>>,
        watch::Receiver<Option<Height>>,
    ) {
        info!("initializing headers-first sync");
        let (tip_sender, header_tip) = watch::channel(None);
        let header_sync = HeaderSync::new(
            &config.network.network,
            peer_set,
            state,
            config.sync.header_sync_concurrency,
        )
        .with_tip_sender(tip_sender);

        (
            tokio::spawn(header_sync.run().in_current_span()),
            header_tip,
        )
    }

    /// Drops the services of a state, on a blocking thread, because the last one waits for the
    /// state's threads to finish and closes the database.
    async fn close_state<T: Send + 'static>(state: T) {
        if let Err(error) = tokio::task::spawn_blocking(move || drop(state)).await {
            warn!(%error, "the task that closes the header-only state failed");
        }
    }

    /// Opens the state, with a spentness hints receiver for `spentness_hints_mode`, and the
    /// memory budget's state `limits`.
    #[allow(clippy::type_complexity)]
    async fn open_state(
        config: &ZebradConfig,
        max_checkpoint_height: Height,
        spentness_hints_mode: zebra_chain::parameters::spentness_hints::Mode,
        limits: zebra_state::MemoryLimits,
    ) -> (
        Option<spentness_hints::StateHints>,
        BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
        zebra_state::ReadStateService,
        zebra_state::LatestChainTip,
        zebra_state::ChainTipChange,
    ) {
        let (state_hints, spentness_hints_receiver) =
            spentness_hints::StateHints::new(spentness_hints_mode).unzip();
        let state_checkpoint_verify_concurrency_limit =
            config.sync.checkpoint_verify_concurrency_limit
                * (VERIFICATION_PIPELINE_SCALING_MULTIPLIER + 1);
        let (state_service, read_only_state_service, latest_chain_tip, chain_tip_change) =
            match spentness_hints_receiver {
                Some(spentness_hints_receiver) => {
                    zebra_state::init_with_spentness(
                        config.state.clone(),
                        &config.network.network,
                        max_checkpoint_height,
                        state_checkpoint_verify_concurrency_limit,
                        spentness_hints_receiver,
                        limits,
                    )
                    .await
                }
                None => {
                    zebra_state::init_with_limits(
                        config.state.clone(),
                        &config.network.network,
                        max_checkpoint_height,
                        state_checkpoint_verify_concurrency_limit,
                        limits,
                    )
                    .await
                }
            };

        (
            state_hints,
            state_service,
            read_only_state_service,
            latest_chain_tip,
            chain_tip_change,
        )
    }

    /// Returns the bound for the state service buffer,
    /// based on the configurations of the services that use the state concurrently.
    fn state_buffer_bound(config: &ZebradConfig) -> usize {
        // Ignore the checkpoint verify limit, because it is very large.
        // Mempool reads bypass the state writer's buffer.
        //
        // TODO: do we also need to account for concurrent use across services?
        //       we could multiply the maximum by 3/2, or add a fixed constant
        [
            config.sync.download_concurrency_limit,
            config.sync.full_verify_concurrency_limit,
            inbound::downloads::MAX_INBOUND_CONCURRENCY,
        ]
        .into_iter()
        .max()
        .unwrap()
    }
}

/// Records which component owns block download in the `sync.v2.owner` gauge:
/// 0 for the syncer, 1 for the checkpoint sync engine. Never returns.
async fn record_sync_owner(
    mut owner: watch::Receiver<checkpoint_sync::SyncOwner>,
) -> std::convert::Infallible {
    loop {
        let engine_owns = !owner.borrow_and_update().is_legacy();
        metrics::gauge!("sync.v2.owner").set(if engine_owns { 1.0 } else { 0.0 });

        if owner.changed().await.is_err() {
            // The supervisor exited, and the task that runs it stops polling this.
            std::future::pending::<()>().await;
        }
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
            .run(self.start(APPLICATION.config()));

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

        if !config.zcashd_compat.enabled && !config.zcashd_compat.block_gossip_peer_ips.is_empty() {
            return Err(std::io::Error::other(
                "zcashd_compat.block_gossip_peer_ips requires zcashd_compat.enabled = true",
            )
            .into());
        }

        if config.zcashd_compat.enabled && config.zcashd_compat.manage_zcashd {
            zcashd_compat::reject_peer_selection_extra_args(
                &config.zcashd_compat.zcashd_extra_args,
            )
            .map_err(|err| std::io::Error::other(err.to_string()))?;

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

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use abscissa_core::config::Override;
    use color_eyre::eyre::eyre;

    use super::StartCmd;
    use crate::components::{
        state_snapshot::decision::{Decision, SkipReason},
        sync::StateSnapshotFallback,
        zcashd_compat,
    };
    use crate::config::ZebradConfig;

    /// A rejection marker left by an earlier process still honors the configured fallback.
    #[test]
    fn rejected_snapshot_restart_honors_fallback() {
        let _init_guard = zebra_test::init();
        let rejected = Decision::Skip(SkipReason::Rejected);
        let mut config = ZebradConfig::default();

        config.sync.state_snapshot_fallback = StateSnapshotFallback::Error;
        let error = StartCmd::handle_rejected_snapshot(&config, &rejected)
            .expect_err("fallback=error must keep rejecting startup");
        let error = error.to_string();
        assert!(
            error.contains("rejected by an earlier bootstrap"),
            "{error}"
        );
        assert!(error.contains("different trusted commitment"), "{error}");

        config.sync.state_snapshot_fallback = StateSnapshotFallback::Genesis;
        StartCmd::handle_rejected_snapshot(&config, &rejected)
            .expect("fallback=genesis continues without the rejected snapshot");

        config.sync.state_snapshot_fallback = StateSnapshotFallback::Error;
        StartCmd::handle_rejected_snapshot(&config, &Decision::Skip(SkipReason::NoPeers))
            .expect("other decision-rule skips keep their existing behavior");
    }

    #[test]
    fn zcashd_compat_flag_enables_mode() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = false;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
    }

    #[test]
    fn zcashd_compat_config_enables_mode() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = false;

        let config = cmd
            .override_config(config)
            .expect("zcashd-compat override config should succeed");

        assert!(config.zcashd_compat.enabled);
    }

    #[test]
    fn block_gossip_peer_ips_require_zcashd_compat() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.block_gossip_peer_ips =
            vec![std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)];

        let error = cmd
            .override_config(config)
            .expect_err("block gossip peers should require zcashd-compat");

        assert!(
            error
                .to_string()
                .contains("zcashd_compat.block_gossip_peer_ips requires"),
            "error should explain the zcashd-compat requirement: {error}"
        );
    }

    #[test]
    fn zcashd_compat_config_rejects_peer_selection_extra_args() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: false,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.enabled = true;
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Embedded;
        config.zcashd_compat.zcashd_extra_args = vec!["-addnode=1.2.3.4".to_string()];

        let error = cmd
            .override_config(config)
            .expect_err("peer-selection extra args should be rejected");
        assert!(
            error.to_string().contains("peer-selection"),
            "unexpected error: {error}"
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
    fn zcashd_compat_embedded_source_allows_missing_local_path() {
        let cmd = StartCmd {
            filters: Vec::new(),
            zcashd_compat: true,
            unsafe_low_specs: false,
        };
        let mut config = ZebradConfig::default();
        config.zcashd_compat.manage_zcashd = true;
        config.zcashd_compat.zcashd_source = zcashd_compat::ConfigZcashdBinarySource::Embedded;
        config.zcashd_compat.zcashd_path = None;

        cmd.override_config(config)
            .expect("embedded source should be validated at runtime, not override-time");
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
