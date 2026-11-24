//! Tool-only lifecycle helpers for production snapshot serving and bootstrap trials.
//!
//! The source is an ordinary `zebrad` opening a disposable snapshot-born serving view.
//! Trials connect only over QUIC; no external peers, mining or bootstrap can advance A.
//! The comparison database is never opened by this child. The view's committed context
//! tip makes headers available through the production locator path.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    serialization::ZcashDeserialize,
    state_snapshot::StateSnapshotCommitment,
};
use zebra_node_services::rpc_client::RpcRequestClient;
use zebra_rpc::methods::{GetBlockResponse, GetBlockchainInfoResponse};
use zebra_state::BoxError;

/// An owned production node, kept alive until stopped or dropped.
///
/// Startup has a deadline, but the owned child deliberately has no lifetime deadline. Both
/// output streams go directly to regular files, so an idle caller cannot fill a child pipe.
#[derive(Debug)]
pub(super) struct SnapshotNode {
    rpc_addr: SocketAddr,
    /// Loopback version 2 endpoint serving the prepared headers and snapshot objects.
    pub(super) quic_addr: SocketAddr,
    child: NodeProcess,
}

enum Launch {
    Source { height: u32, trials: u32 },
    Bootstrap(StateSnapshotCommitment, SocketAddr),
}

impl SnapshotNode {
    /// Opens a persistent serving view with the production `zebrad` executable.
    ///
    /// `workspace` retains `source.toml`, `source.stdout.log`, and `source.stderr.log`, including
    /// on failure. Use a fresh workspace for each source launch; logs are never overwritten.
    /// The caller must release all database handles before starting the node and must never
    /// connect a trial over legacy TCP or provide another source of block bodies above `height`.
    /// Any startup failure or cancelled startup drops the child and performs bounded cleanup.
    /// Allows two connections per trial plus one diagnostic client in the per-IP rate window.
    pub(super) async fn start(
        zebrad: &Path,
        network: &Network,
        source: &zebra_state::Config,
        workspace: &Path,
        height: u32,
        trials: u32,
        timeout: Duration,
    ) -> Result<Self, BoxError> {
        Self::launch(
            zebrad,
            network,
            source,
            workspace,
            Launch::Source { height, trials },
            timeout,
        )
        .await
    }

    /// Bootstraps a fresh pruned database in `workspace/cache`, using only A's QUIC endpoint.
    ///
    /// An existing cache directory is refused, including an empty one. Readiness requires
    /// production installation and state initialization, not merely an RPC listener.
    pub(super) async fn start_bootstrap(
        zebrad: &Path,
        network: &Network,
        workspace: &Path,
        commitment: StateSnapshotCommitment,
        peer: SocketAddr,
        timeout: Duration,
    ) -> Result<Self, BoxError> {
        fs::create_dir_all(workspace)?;
        let state = zebra_state::Config {
            cache_dir: workspace.join("cache"),
            prune_blocks_older_than: Some(zebra_state::MAX_BLOCK_REORG_HEIGHT + 1),
            ..Default::default()
        };
        fs::create_dir(&state.cache_dir)?;
        Self::launch(
            zebrad,
            network,
            &state,
            workspace,
            Launch::Bootstrap(commitment, peer),
            timeout,
        )
        .await
    }

    async fn launch(
        zebrad: &Path,
        network: &Network,
        source: &zebra_state::Config,
        workspace: &Path,
        launch: Launch,
        timeout: Duration,
    ) -> Result<Self, BoxError> {
        if source.ephemeral {
            return Err("snapshot trial state must be persistent".into());
        }
        fs::create_dir_all(workspace)?;
        let workspace = fs::canonicalize(workspace)?;
        let mut state = source.clone();
        state.cache_dir = fs::canonicalize(&state.cache_dir)?;
        state.debug_stop_at_height = None;
        state.delete_old_database = false;
        state.should_backup_non_finalized_state = false;
        let (name, height, commitment, peer, max_connections_per_ip) = match launch {
            Launch::Source { height, trials } => (
                "source",
                height,
                None,
                None,
                usize::try_from(u64::from(trials) * 2 + 1)?.max(4),
            ),
            Launch::Bootstrap(commitment, peer) => (
                "bootstrap",
                commitment.height.0,
                Some(commitment),
                Some(peer),
                4,
            ),
        };

        // Keep the reservations together so the two TCP listeners cannot select the same port.
        // As in the e2e helpers, releasing before spawn leaves a small, unavoidable bind race;
        // startup readiness reports a losing bind rather than using another node's RPC alone.
        let rpc_port = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let legacy_port = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let quic_port = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let rpc_addr = rpc_port.local_addr()?;
        let quic_addr = quic_port.local_addr()?;
        let config = NodeConfig {
            network: zebra_network::Config {
                network: network.clone(),
                listen_addr: legacy_port.local_addr()?,
                v2_listen_addr: Some(quic_addr),
                initial_mainnet_peers: Default::default(),
                initial_testnet_peers: Default::default(),
                initial_v2_peers: peer.iter().map(ToString::to_string).collect(),
                cache_dir: false.into(),
                v2_peer_discovery: false,
                // The production limiter counts recent attempts, even after a B exits.
                // Account for this run's header/object connections sharing one loopback IP.
                max_connections_per_ip,
                ..Default::default()
            },
            state,
            rpc: RpcConfig {
                listen_addr: rpc_addr,
                enable_cookie_auth: false,
            },
            sync: SyncConfig {
                headers_first: commitment.is_some(),
                v2_checkpoint_sync: false,
                state_snapshot_sync: commitment.is_some(),
                serve_state_snapshots: commitment.is_none(),
                state_snapshot: commitment,
                state_snapshot_peers: peer.iter().map(ToString::to_string).collect(),
                state_snapshot_fallback: "error",
            },
            tracing: TracingConfig {
                filter: "info",
                use_color: false,
                force_use_color: false,
            },
        };
        let config_path = workspace.join(format!("{name}.toml"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config_path)?
            .write_all(toml::to_string(&config)?.as_bytes())?;
        let stdout_path = workspace.join(format!("{name}.stdout.log"));
        let stdout = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout_path)?;
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(workspace.join(format!("{name}.stderr.log")))?;
        let mut logs = File::open(&stdout_path)?;
        let mut command = Command::new(zebrad);
        command
            .arg("--config")
            .arg(&config_path)
            .arg("start")
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        // Environment overrides outrank TOML. Do not let the operator's normal node settings
        // enable external peers, mining, or telemetry in these isolated trials.
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("ZEBRA_")
                || name.to_string_lossy().starts_with("OTEL_")
            {
                command.env_remove(name);
            }
        }
        drop((rpc_port, legacy_port, quic_port));
        let mut child = NodeProcess {
            child: command.spawn()?,
            workspace,
        };
        let client = RpcRequestClient::new_with_timeout(rpc_addr, timeout);
        let readiness = tokio::time::timeout(timeout, async {
            let messages: &[&[u8]; 2] = if commitment.is_some() {
                &[
                    b"state snapshot stage: state renamed into place",
                    b"state snapshot stage: state initialized",
                ]
            } else {
                &[
                    b"opened experimental version 2 (QUIC) Zcash protocol endpoint",
                    b"scanned the state snapshot store",
                ]
            };
            wait_for_logs(&mut child, &mut logs, messages).await?;
            let height_ready = async {
                loop {
                    match client
                        .json_result_from_call::<u32>("getblockcount", "[]")
                        .await
                    {
                        Ok(count) if count >= height => break count,
                        _ => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
            };
            tokio::pin!(height_ready);
            let actual = loop {
                tokio::select! {
                    count = &mut height_ready => break count,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        child.require_running()?;
                    }
                }
            };
            if actual != height {
                return Err(format!(
                    "{name} has unexpected tip {actual}, expected exactly {height}"
                )
                .into());
            }
            child.require_running()?;
            Ok::<(), BoxError>(())
        })
        .await;
        match readiness {
            Ok(result) => result?,
            Err(_) => {
                return Err(format!(
                    "{name} readiness timed out after {timeout:?}; see {}",
                    child.workspace.display()
                )
                .into());
            }
        }
        Ok(Self {
            rpc_addr,
            quic_addr,
            child,
        })
    }

    /// Checks the installed tip and retained snapshot body while both nodes are alive.
    pub(super) async fn check_liveness(
        &mut self,
        source: &mut Self,
        expected_tip: (Height, block::Hash),
        snapshot_height: Height,
        timeout: Duration,
    ) -> Result<(), BoxError> {
        let client = RpcRequestClient::new_with_timeout(self.rpc_addr, timeout);
        let check = tokio::time::timeout(
            timeout,
            check_snapshot_state(&client, expected_tip, snapshot_height),
        );
        tokio::pin!(check);
        loop {
            tokio::select! {
                result = &mut check => {
                    result.map_err(|_| "installed-state liveness timed out")??;
                    self.require_running()?;
                    return source.require_running();
                }
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    self.require_running()?;
                    source.require_running()?;
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) async fn request_stop(&self, timeout: Duration) -> Result<(), BoxError> {
        let client = RpcRequestClient::new_with_timeout(self.rpc_addr, timeout);
        let _: String = client.json_result_from_call("stop", "[]").await?;
        Ok(())
    }

    /// Returns the owned child's process ID for diagnostics, not external process management.
    pub(super) fn id(&self) -> u32 {
        self.child.child.id()
    }

    /// Fails if the owned node has exited, rather than mistaking another RPC server for it.
    pub(super) fn require_running(&mut self) -> Result<(), BoxError> {
        self.child.require_running()
    }

    /// Stops and reaps the node within the cleanup deadline.
    ///
    /// An already-exited child is a failure, even if it exited successfully. The final exit
    /// status must also show the intentional kill, catching natural exits racing with shutdown.
    pub(super) fn stop(mut self) -> Result<(), BoxError> {
        self.child.require_running()?;
        let status = self.child.kill_and_reap()?;
        #[cfg(unix)]
        let killed = {
            use std::os::unix::process::ExitStatusExt;
            const SIGKILL: i32 = 9;
            status.signal() == Some(SIGKILL)
        };
        #[cfg(not(unix))]
        let killed = status.code() == Some(1);

        if killed {
            Ok(())
        } else {
            Err(self.child.unexpected_exit(status))
        }
    }
}

async fn check_snapshot_state(
    client: &RpcRequestClient,
    expected_tip: (Height, block::Hash),
    snapshot_height: Height,
) -> Result<(), BoxError> {
    let info: GetBlockchainInfoResponse = client
        .json_result_from_call("getblockchaininfo", "[]")
        .await?;
    let actual_tip = (info.blocks(), info.best_block_hash());
    if actual_tip != expected_tip {
        return Err(format!(
            "unexpected installed tip: expected {expected_tip:?}, got {actual_tip:?}"
        )
        .into());
    }
    if !info.pruned() || info.prune_height() != Some(snapshot_height) {
        return Err(format!(
            "expected pruned state with retained floor {snapshot_height:?}, got pruned={}, pruneheight={:?}",
            info.pruned(),
            info.prune_height(),
        )
        .into());
    }
    let response = client
        .json_result_from_call("getblock", format!(r#"["{}", 0]"#, snapshot_height.0))
        .await
        .map_err(|error| format!("retained block at {snapshot_height:?}: {error}"))?;
    let GetBlockResponse::Raw(raw) = response else {
        return Err(format!("expected raw retained block at {snapshot_height:?}").into());
    };
    let retained = block::Block::zcash_deserialize(raw.as_ref())?;
    if retained.coinbase_height() != Some(snapshot_height) {
        return Err(
            format!("retained block at {snapshot_height:?} has the wrong coinbase height").into(),
        );
    }
    Ok(())
}

/// Owns one zebrad process with its output redirected to files. Unlike the test
/// harness, cleanup must not write to the trial command's report stream.
#[derive(Debug)]
struct NodeProcess {
    child: Child,
    workspace: PathBuf,
}

impl NodeProcess {
    fn unexpected_exit(&self, status: ExitStatus) -> BoxError {
        format!(
            "snapshot trial node exited unexpectedly ({status}); see its stdout/stderr logs in {}",
            self.workspace.display()
        )
        .into()
    }

    fn require_running(&mut self) -> Result<(), BoxError> {
        match self.child.try_wait()? {
            Some(status) => Err(self.unexpected_exit(status)),
            None => Ok(()),
        }
    }

    fn kill_and_reap(&mut self) -> Result<ExitStatus, BoxError> {
        const CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
        if let Some(status) = self.child.try_wait()? {
            return Ok(status);
        }
        self.child.kill()?;
        let deadline = Instant::now() + CLEANUP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "timed out reaping snapshot trial node; see its stdout/stderr logs in {}",
                    self.workspace.display()
                )
                .into());
            }
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    }
}

impl Drop for NodeProcess {
    #[allow(clippy::print_stderr)] // Drop cannot return cleanup diagnostics to the caller.
    fn drop(&mut self) {
        if let Err(error) = self.kill_and_reap() {
            eprintln!("{error}");
        }
    }
}

/// Observes existing production readiness messages without owning a pipe or retaining the log.
async fn wait_for_logs(
    child: &mut NodeProcess,
    logs: &mut File,
    messages: &[&[u8]; 2],
) -> Result<(), BoxError> {
    let overlap = messages
        .iter()
        .map(|message| message.len())
        .max()
        .unwrap_or(1)
        - 1;
    let mut buffer = [0; 8192];
    let mut retained = 0;
    let mut seen = [false; 2];
    while seen.iter().any(|seen| !seen) {
        child.require_running()?;
        let read = logs.read(&mut buffer[retained..])?;
        let length = retained + read;
        for (seen, message) in seen.iter_mut().zip(messages) {
            *seen |= buffer[..length]
                .windows(message.len())
                .any(|window| window == *message);
        }
        retained = length.min(overlap);
        buffer.copy_within(length - retained..length, 0);
        if read == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        } else {
            // Keep cancellation and the enclosing startup deadline live even for a busy log.
            tokio::task::yield_now().await;
        }
    }
    Ok(())
}

// Serialize only the sections this tool controls. All other options use ordinary zebrad
// defaults; depending on ZebradConfig here would create a zebrad -> zebra-utils -> zebrad cycle.
#[derive(Serialize)]
struct NodeConfig {
    network: zebra_network::Config,
    state: zebra_state::Config,
    rpc: RpcConfig,
    sync: SyncConfig,
    tracing: TracingConfig,
}

#[derive(Serialize)]
struct RpcConfig {
    listen_addr: SocketAddr,
    enable_cookie_auth: bool,
}

#[derive(Serialize)]
struct SyncConfig {
    headers_first: bool,
    v2_checkpoint_sync: bool,
    state_snapshot_sync: bool,
    serve_state_snapshots: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_snapshot: Option<StateSnapshotCommitment>,
    state_snapshot_peers: Vec<String>,
    state_snapshot_fallback: &'static str,
}

#[derive(Serialize)]
struct TracingConfig {
    filter: &'static str,
    use_color: bool,
    force_use_color: bool,
}
