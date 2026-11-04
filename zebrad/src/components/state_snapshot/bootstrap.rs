//! The bootstrap flow: dial the bootstrap peers, fetch and check the manifest,
//! run the chain checks, download and verify the pieces, import them, and clean up.
//!
//! A failure the snapshot caused marks its manifest as rejected; any failure
//! deletes the temporary import directory, and the caller follows
//! `[sync] state_snapshot_fallback`.
//!
//! The network-facing steps are generic over the peer service, so tests drive
//! them with mock peers; `zebrad start` runs them over the clients the
//! version 2 bootstrap dialer returns.

use std::{
    collections::{HashMap, HashSet},
    fs,
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;
use tokio::{
    sync::{mpsc, Notify, Semaphore},
    task::{JoinHandle, JoinSet},
    time::{sleep_until, timeout},
};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::{
    block::{
        self, merkle::AuthDataRoot, Block, ChainHistoryBlockTxAuthCommitmentHash,
        ChainHistoryMmrRootHash, Header, Height,
    },
    parameters::Network,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};
use zebra_network::{self as zn, InventoryResponse, PeerSocketAddr};
use zebra_state::{
    state_snapshot::{
        format::{ComponentId, Manifest, PieceManifest, MAX_MANIFEST_BYTES, MAX_PIECE_BYTES},
        import::{
            build_piece_sst, decode_history_tree, import_dir, import_state_snapshot, sst_path,
        },
        store::ArtifactStore,
        SnapshotError,
    },
    SnapshotBase,
};

use crate::{application::user_agent, config::ZebradConfig, BoxError};

use super::{
    progress::Stage,
    scheduler::{
        PieceDescriptor, PieceStore, Scheduler, SchedulerConfig, SchedulerError, VerifiedPiece,
    },
};

#[cfg(test)]
mod tests;

/// The manifest must be fetched and verified within this long of the bootstrap
/// peers connecting.
pub const MANIFEST_DEADLINE: Duration = Duration::from_secs(60);

/// The history tree piece, fetched directly before the chain checks, gets the
/// same budget as the manifest.
const HISTORY_TREE_DEADLINE: Duration = Duration::from_secs(60);

/// How long one `host:port` string may take to resolve.
const PEER_RESOLVE_TIMEOUT: Duration = Duration::from_secs(10);

/// The most bootstrap peers dialled.
const MAX_BOOTSTRAP_PEERS: usize = 8;

/// How many requests may queue on one bootstrap peer: the scheduler's two
/// sub-ranges in flight, plus the chain checks' requests.
const PEER_BUFFER_BOUND: usize = 8;

/// The most commitment-valid copies of block `H + 1` retained, from distinct peers.
const SUCCESSOR_COPIES: usize = 3;

/// The most headers one version 2 `get-headers` response carries. The wire
/// constant is private to `zebra-network`; this bounds the rounds of the
/// header chain fetch.
const MAX_HEADERS_PER_RESPONSE: usize = 160;

/// The first backoff after a bootstrap peer refuses a direct object request.
const MIN_OBJECT_REFUSED_BACKOFF: Duration = Duration::from_secs(1);

/// The longest backoff between direct object requests to one peer.
const MAX_OBJECT_REFUSED_BACKOFF: Duration = Duration::from_secs(30);

/// How long a peer is left between direct requests for an object it did not
/// serve, matching the scheduler's not-found backoff.
const OBJECT_SKIP_BACKOFF: Duration = Duration::from_secs(30);

/// Distinguishes same-process workspaces even when their clock readings match.
static NEXT_IMPORT_NONCE: AtomicU64 = AtomicU64::new(0);

/// The shutdown bit in [`INSTALL_LIFECYCLE`]. The other bits count installations that have passed
/// their final shutdown check and must finish before process shutdown cancels the bootstrap.
const INSTALL_SHUTTING_DOWN: usize = 1 << (usize::BITS - 1);

/// Whether shutdown has started, and how many final snapshot installations are still active.
static INSTALL_LIFECYCLE: AtomicUsize = AtomicUsize::new(0);

/// Notifies shutdown when the last active installation finishes.
static INSTALLS_QUIESCED: Notify = Notify::const_new();

/// How many verified pieces may be running or waiting for an SST build before
/// the verified-piece channel is allowed to fill and hold the scheduler back.
const VERIFIED_PIECE_CHANNEL_BOUND: usize = 64;

/// How many peer blames the scheduler may report before it is held back.
const BLAME_CHANNEL_BOUND: usize = 64;

/// The bootstrap peers as the flow uses them: the version 2 clients, each
/// behind a [`Buffer`] so the chain checks and the scheduler can share them.
pub type PeerService = Buffer<zn::Client, zn::Request>;

/// Why a bootstrap did not install a snapshot.
#[derive(Debug, Error)]
pub enum BootstrapError {
    /// The snapshot's contents failed a check of the trust model, which is
    /// the snapshot's fault: its manifest is marked as rejected before the
    /// fallback applies.
    #[error("state snapshot rejected: {0}")]
    Rejected(String),

    /// The download did not complete: the manifest deadline passed, no piece
    /// verified for the stall deadline, or every peer was excluded. Nothing
    /// is marked, and the fallback applies.
    #[error("state snapshot bootstrap aborted: {0}")]
    Aborted(String),

    /// The piece scheduler failed.
    #[error(transparent)]
    Scheduler(#[from] SchedulerError),

    /// Storing, building, or importing the snapshot failed.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
}

/// A final installation that shutdown must let finish before cancelling the bootstrap.
#[must_use = "dropping the guard releases shutdown"]
struct SnapshotInstall;

impl SnapshotInstall {
    /// Starts the final installation, unless shutdown won the lifecycle race first.
    fn begin() -> Result<Self, BootstrapError> {
        let mut lifecycle = INSTALL_LIFECYCLE.load(Ordering::SeqCst);
        loop {
            if lifecycle & INSTALL_SHUTTING_DOWN != 0 || zebra_chain::shutdown::is_shutting_down() {
                return Err(shutdown_before_install());
            }

            let active = lifecycle
                .checked_add(1)
                .expect("the process can not run usize::MAX snapshot installations");
            match INSTALL_LIFECYCLE.compare_exchange_weak(
                lifecycle,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Ok(SnapshotInstall),
                Err(current) => lifecycle = current,
            }
        }
    }
}

impl Drop for SnapshotInstall {
    fn drop(&mut self) {
        let previous = INSTALL_LIFECYCLE.fetch_sub(1, Ordering::SeqCst);
        let active = previous & !INSTALL_SHUTTING_DOWN;
        debug_assert!(active > 0, "every guard owns one active installation");
        if active == 1 && previous & INSTALL_SHUTTING_DOWN != 0 {
            INSTALLS_QUIESCED.notify_one();
        }
    }
}

/// Prevents new final installations and records process shutdown in lifecycle order.
pub(crate) fn begin_snapshot_shutdown() {
    INSTALL_LIFECYCLE.fetch_or(INSTALL_SHUTTING_DOWN, Ordering::SeqCst);
    zebra_chain::shutdown::set_shutting_down();
}

/// Waits for any installation that won its final shutdown check to finish.
pub(crate) async fn wait_for_snapshot_install() {
    wait_for_snapshot_install_with_hook(|| {}).await;
}

async fn wait_for_snapshot_install_with_hook<F>(mut after_lifecycle_load: F)
where
    F: FnMut(),
{
    loop {
        let notified = INSTALLS_QUIESCED.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if INSTALL_LIFECYCLE.load(Ordering::SeqCst) & !INSTALL_SHUTTING_DOWN == 0 {
            return;
        }
        after_lifecycle_load();
        notified.await;
    }
}

fn shutdown_before_install() -> BootstrapError {
    BootstrapError::Aborted(
        "Zebra is shutting down; the staged state snapshot was not installed".to_string(),
    )
}

/// Maps a snapshot error to rejection only when the snapshot's contents are
/// at fault; local I/O, unsupported configuration, and caller errors stay
/// separate so they are not marked as a rejected manifest.
fn snapshot_fault(error: SnapshotError) -> BootstrapError {
    if error.rejects_snapshot() {
        BootstrapError::Rejected(error.to_string())
    } else {
        BootstrapError::Snapshot(error)
    }
}

/// The bootstrap peers that are still trusted, in dial order.
///
/// A peer that misbehaves is blamed and removed, which drops this flow's
/// handle on its connection.
pub(crate) struct Peers<S> {
    clients: Vec<(PeerSocketAddr, S)>,
}

impl<S: Clone> Peers<S> {
    /// Returns the peers `clients`, in order.
    pub(crate) fn new(clients: Vec<(PeerSocketAddr, S)>) -> Self {
        Peers { clients }
    }

    /// Returns the remaining peers' addresses, in order.
    pub(crate) fn addrs(&self) -> Vec<PeerSocketAddr> {
        self.clients.iter().map(|(addr, _)| *addr).collect()
    }

    /// Returns a copy of the remaining peers, to iterate while blaming some.
    fn snapshot(&self) -> Vec<(PeerSocketAddr, S)> {
        self.clients.clone()
    }

    /// Returns the remaining peers for the scheduler.
    fn into_clients(self) -> Vec<(PeerSocketAddr, S)> {
        self.clients
    }

    fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    /// Excludes `peer` for the rest of the bootstrap.
    fn blame(&mut self, peer: PeerSocketAddr, reason: &str) {
        warn!(%peer, reason, "excluding a bootstrap peer");
        self.clients.retain(|(addr, _)| *addr != peer);
    }
}

impl Peers<PeerService> {
    /// Wraps the dialled `clients` so the flow can share each one.
    fn buffered(clients: Vec<(PeerSocketAddr, zn::Client)>) -> Self {
        Peers::new(
            clients
                .into_iter()
                .map(|(addr, client)| (addr, Buffer::new(client, PEER_BUFFER_BOUND)))
                .collect(),
        )
    }
}

/// Why a fetch from one peer did not produce the object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FetchError {
    /// The peer misbehaved: it is blamed and excluded.
    Peer(String),

    /// The peer refused the request: retry it with exponential backoff.
    Refused(String),

    /// The peer could not serve the object in time: the next peer is tried.
    Skip(String),
}

/// Bootstraps the state from the snapshot `commitment` names, downloading
/// from `peers`, and returns once the imported database has been renamed into
/// place.
pub async fn bootstrap(
    config: &ZebradConfig,
    commitment: StateSnapshotCommitment,
    peers: Vec<String>,
) -> Result<(), BootstrapError> {
    let network = config.network.network.clone();
    let store = ArtifactStore::new(&config.state, &network);
    let import_root = import_dir(&config.state);

    // 1. Each attempt gets its own workspace, and holds a lock on it while it runs. Another
    // bootstrap may still be using a sibling directory, so the shared import root is never
    // removed, and only workspaces that no attempt holds are swept.

    // 2. Dial the bootstrap peers.
    let addrs = resolve_peers(&peers).await;
    if addrs.is_empty() {
        return Err(BootstrapError::Aborted(format!(
            "none of the {} configured bootstrap peers resolved to an address",
            peers.len()
        )));
    }

    let mut bootstrap_peers = zn::v2_bootstrap_clients(&config.network, &addrs, user_agent())
        .await
        .map_err(|error| {
            BootstrapError::Aborted(format!("could not connect to a bootstrap peer: {error}"))
        })?;
    let clients = std::mem::take(&mut bootstrap_peers.clients);
    if clients.is_empty() {
        bootstrap_peers.close().await;
        return Err(BootstrapError::Aborted(
            "no bootstrap peer completed a version 2 handshake".to_string(),
        ));
    }
    let connected_at = Instant::now();
    let peers = Peers::buffered(clients);
    info!(
        peers = peers.clients.len(),
        addrs = ?peers.addrs(),
        "{}",
        Stage::PeersReady
    );

    let result = async {
        let workspace = create_import_workspace(&import_root).await?;
        let temp_root = workspace.path.clone();
        let result = run(
            config,
            &network,
            &store,
            &temp_root,
            commitment,
            peers,
            connected_at,
        )
        .await;

        match &result {
            Ok(()) => {}
            Err(BootstrapError::Rejected(reason)) => {
                error!(
                    manifest = %commitment.manifest,
                    reason,
                    "the state snapshot failed a check of its contents: marking it as rejected"
                );
                if let Err(error) = store.mark_rejected(commitment.manifest) {
                    warn!(?error, "could not mark the state snapshot as rejected");
                }
            }
            Err(error) => {
                error!(%error, "the state snapshot bootstrap did not finish");
            }
        }
        if result.is_err() {
            if let Err(error) = remove_dir(&temp_root).await {
                warn!(
                    ?error,
                    "could not delete this attempt's state snapshot import workspace"
                );
            }
        }
        // A workspace that could not be deleted is left to the next attempt's sweep.
        workspace.release().await;
        result
    }
    .await;

    // Every exit after dialling, including workspace creation failure, closes and drains the
    // short-lived endpoints before the caller can start genesis fallback.
    close_bootstrap_after(result, bootstrap_peers.close()).await
}

async fn close_bootstrap_after<T>(
    result: Result<T, BootstrapError>,
    close: impl Future<Output = ()>,
) -> Result<T, BootstrapError> {
    close.await;
    result
}

/// How many times a piece write retries after another attempt removes the shared directory entry
/// before this attempt can create its lease.
const OBJECT_LEASE_ATTEMPTS: usize = 16;

/// A shared artifact store with an attempt-owned lease for every object this attempt imports.
#[derive(Clone, Debug)]
struct AttemptStore {
    /// The content-addressed objects shared with other attempts and snapshot serving.
    shared: ArtifactStore,

    /// This attempt's hard links to the shared objects, inside its unique import workspace.
    leases: PathBuf,
}

impl AttemptStore {
    fn new(shared: &ArtifactStore, temp_root: &Path) -> Self {
        AttemptStore {
            shared: shared.clone(),
            leases: temp_root.join("leased-objects"),
        }
    }

    /// Stores a verified object and returns a path another attempt's cleanup can not invalidate.
    fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError> {
        let lease = self.leases.join(hash.to_string());
        if lease.exists() {
            return Ok(lease);
        }
        fs::create_dir_all(&self.leases).map_err(|source| SnapshotError::Io {
            path: self.leases.clone(),
            source,
        })?;

        for _ in 0..OBJECT_LEASE_ATTEMPTS {
            let shared = self.shared.put(hash, object)?;
            match fs::hard_link(&shared, &lease) {
                Ok(()) => return Ok(lease),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(lease),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(SnapshotError::Io {
                        path: lease,
                        source,
                    });
                }
            }
        }

        Err(SnapshotError::Io {
            path: lease,
            source: io::Error::new(
                io::ErrorKind::NotFound,
                "another snapshot attempt repeatedly removed the shared object before it could be leased",
            ),
        })
    }
}

impl PieceStore for AttemptStore {
    fn put(&self, hash: ObjectHash, object: &[u8]) -> Result<PathBuf, SnapshotError> {
        AttemptStore::put(self, hash, object)
    }
}

/// Steps 3 to 7 of the flow, from the manifest to the renamed database.
async fn run<S>(
    config: &ZebradConfig,
    network: &Network,
    store: &ArtifactStore,
    temp_root: &Path,
    commitment: StateSnapshotCommitment,
    mut peers: Peers<S>,
    connected_at: Instant,
) -> Result<(), BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let attempt_store = AttemptStore::new(store, temp_root);

    // 3. The manifest.
    let (manifest, manifest_bytes) = fetch_manifest(
        &mut peers,
        commitment,
        network,
        connected_at + MANIFEST_DEADLINE,
    )
    .await?;
    put_object(&attempt_store, commitment.manifest, manifest_bytes).await?;
    info!(
        height = manifest.height.0,
        hash = %manifest.block_hash,
        manifest = %commitment.manifest,
        pieces = manifest.pieces().count(),
        bytes = manifest.total_piece_bytes(),
        peers = peers.clients.len(),
        "{}",
        Stage::ManifestVerified
    );

    // The history tree is needed by the chain checks, so its piece is fetched first.
    let tree_piece = history_tree_piece(&manifest)?;
    let tree_bytes = fetch_piece(
        &mut peers,
        &tree_piece,
        Instant::now() + HISTORY_TREE_DEADLINE,
    )
    .await?;
    let tree_path = put_object(&attempt_store, tree_piece.hash, tree_bytes).await?;
    let history_tree = {
        let (tree_path, network) = (tree_path.clone(), network.clone());
        tokio::task::spawn_blocking(move || decode_history_tree(&tree_path, &network))
            .await
            .map_err(|error| {
                BootstrapError::Aborted(format!("the history tree decoding task failed: {error}"))
            })?
            .map_err(snapshot_fault)?
    };
    if history_tree.current_height() != commitment.height {
        return Err(BootstrapError::Rejected(format!(
            "the history tree ends at height {}, not at the snapshot height {}",
            history_tree.current_height().0,
            commitment.height.0
        )));
    }
    let history_root = history_tree.hash();

    // 4. The block-hash and successor-commitment checks.
    let deferred_checks = check_chain(&mut peers, network, commitment, &history_root).await?;
    if peers.is_empty() {
        return Err(BootstrapError::Aborted(
            "every bootstrap peer was excluded during the chain checks".to_string(),
        ));
    }

    // 5. The pieces, each built into an SST file as it verifies.
    let tree_piece = VerifiedPiece {
        descriptor: tree_piece,
        path: tree_path,
    };
    let piece_paths =
        download_and_build(peers, &manifest, attempt_store, temp_root, tree_piece).await?;

    // 6. Import and check the database under this attempt's workspace. The
    // final rename is separate so cancellation can never leave the importer's
    // detached blocking task installing a database after shutdown.
    let major = zebra_state::state_database_format_version_in_code().major;
    let db_path = config
        .state
        .db_path(zebra_state::constants::STATE_DATABASE_KIND, major, network);
    let mut staged_config = config.state.clone();
    staged_config.cache_dir = temp_root.join("staged");
    let staged_db_path =
        staged_config.db_path(zebra_state::constants::STATE_DATABASE_KIND, major, network);
    {
        let (temp_root, manifest, network, pieces) = (
            temp_root.to_path_buf(),
            manifest.clone(),
            network.clone(),
            piece_paths.clone(),
        );
        tokio::task::spawn_blocking(move || {
            import_state_snapshot(
                &temp_root,
                &manifest,
                commitment,
                deferred_checks,
                &network,
                &staged_config,
                &pieces,
            )
        })
        .await
        .map_err(|error| {
            BootstrapError::Aborted(format!("the state snapshot import task failed: {error}"))
        })?
        .map_err(snapshot_fault)?;
    }
    let backup = config.state.non_finalized_state_backup_dir(network);
    install_staged_snapshot(staged_db_path, db_path.clone(), backup).await?;

    info!(path = %db_path.display(), "{}", Stage::SstsIngested);
    info!(path = %db_path.display(), deferred_checks, "{}", Stage::ChecksDone);
    info!(path = %db_path.display(), height = commitment.height.0, "{}", Stage::StateRenamed);

    // 7. Cleanup cannot undo a successful installation or trigger genesis
    // fallback after the database has been renamed into place.
    finish_install(
        piece_paths,
        temp_root,
        store,
        config.sync.serve_state_snapshots,
    )
    .await
}

/// The suffix of the lock file next to each import workspace. The attempt that uses the
/// workspace holds an exclusive lock on it, which the operating system releases if the process
/// dies.
const WORKSPACE_LOCK_SUFFIX: &str = ".lock";

/// One bootstrap attempt's import workspace, and the lock that marks it as in use.
#[derive(Debug)]
struct ImportWorkspace {
    /// The workspace directory.
    path: PathBuf,

    /// The workspace's lock file.
    lock_path: PathBuf,

    /// The open lock file, locked exclusively until the workspace is released.
    lock: fs::File,
}

impl ImportWorkspace {
    /// Releases the lock and deletes the lock file, once the attempt has deleted its workspace
    /// directory or left it to the next sweep.
    async fn release(self) {
        let Self {
            lock_path, lock, ..
        } = self;
        let _ = tokio::task::spawn_blocking(move || {
            drop(lock);
            if let Err(error) = fs::remove_file(&lock_path) {
                if error.kind() != io::ErrorKind::NotFound {
                    warn!(
                        ?error,
                        path = %lock_path.display(),
                        "could not delete a state snapshot import workspace lock"
                    );
                }
            }
        })
        .await;
    }
}

/// Returns the lock file path of the import workspace at `workspace`.
fn workspace_lock_path(workspace: &Path) -> PathBuf {
    let mut path = workspace.as_os_str().to_owned();
    path.push(WORKSPACE_LOCK_SUFFIX);
    PathBuf::from(path)
}

/// Deletes the workspaces under `import_root` that no running attempt holds: the leftovers of
/// attempts that a crash, a kill, or a dropped bootstrap stopped before their cleanup.
///
/// Blocks on the filesystem.
fn sweep_stale_import_workspaces(import_root: &Path) {
    let entries = match fs::read_dir(import_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            warn!(
                ?error,
                path = %import_root.display(),
                "could not list the state snapshot import workspaces"
            );
            return;
        }
    };

    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        let lock_path = workspace_lock_path(&path);
        let lock = match fs::OpenOptions::new().write(true).open(&lock_path) {
            Ok(lock) => match lock.try_lock() {
                Ok(()) => Some(lock),
                // A running attempt holds the workspace.
                Err(fs::TryLockError::WouldBlock) => continue,
                Err(fs::TryLockError::Error(error)) => {
                    warn!(
                        ?error,
                        path = %lock_path.display(),
                        "could not check a state snapshot import workspace lock"
                    );
                    continue;
                }
            },
            // An attempt creates its lock file before its workspace and deletes it after, so a
            // workspace without one is stale.
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                warn!(
                    ?error,
                    path = %lock_path.display(),
                    "could not open a state snapshot import workspace lock"
                );
                continue;
            }
        };

        info!(path = %path.display(), "deleting a stale state snapshot import workspace");
        match fs::remove_dir_all(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                warn!(
                    ?error,
                    path = %path.display(),
                    "could not delete a stale state snapshot import workspace"
                );
                continue;
            }
        }
        if let Some(lock) = lock {
            drop(lock);
            let _ = fs::remove_file(&lock_path);
        }
    }
}

/// Creates a fresh workspace for one bootstrap attempt without disturbing any
/// sibling workspace another attempt may still be using, after deleting the
/// stale ones.
async fn create_import_workspace(import_root: &Path) -> Result<ImportWorkspace, BootstrapError> {
    let import_root = import_root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        fs::create_dir_all(&import_root).map_err(|source| SnapshotError::Io {
            path: import_root.clone(),
            source,
        })?;
        sweep_stale_import_workspaces(&import_root);

        for _ in 0..16 {
            let path = import_root.join(import_nonce());
            let lock_path = workspace_lock_path(&path);
            // The lock is taken before the workspace exists, so a sweep never sees this
            // workspace unlocked.
            let lock = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(lock) => lock,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(SnapshotError::Io {
                        path: lock_path,
                        source,
                    })
                }
            };
            match lock.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => continue,
                Err(fs::TryLockError::Error(source)) => {
                    return Err(SnapshotError::Io {
                        path: lock_path,
                        source,
                    })
                }
            }
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(ImportWorkspace {
                        path,
                        lock_path,
                        lock,
                    })
                }
                Err(error) => {
                    drop(lock);
                    let _ = fs::remove_file(&lock_path);
                    if error.kind() != io::ErrorKind::AlreadyExists {
                        return Err(SnapshotError::Io {
                            path,
                            source: error,
                        });
                    }
                }
            }
        }

        Err(SnapshotError::Unsupported(format!(
            "could not allocate a unique state snapshot import workspace under {import_root:?}"
        )))
    })
    .await
    .map_err(|error| {
        BootstrapError::Aborted(format!("an import workspace creation task failed: {error}"))
    })?
    .map_err(BootstrapError::Snapshot)
}

/// Renames an already checked database from `staged` to `target`, unless Zebra
/// has started shutting down. Errors after the rename are cleanup errors and
/// do not turn the successful installation into a bootstrap failure.
async fn install_staged_snapshot(
    staged: PathBuf,
    target: PathBuf,
    backup: Option<PathBuf>,
) -> Result<(), BootstrapError> {
    install_staged_snapshot_with_hook(staged, target, backup, || {}).await
}

async fn install_staged_snapshot_with_hook<F>(
    staged: PathBuf,
    target: PathBuf,
    backup: Option<PathBuf>,
    before_rename: F,
) -> Result<(), BootstrapError>
where
    F: FnOnce() + Send + 'static,
{
    install_staged_snapshot_using(staged, target, backup, before_rename, |from, to| {
        fs::rename(from, to)
    })
    .await
}

#[cfg(test)]
async fn install_staged_snapshot_with_rename_failure(
    staged: PathBuf,
    target: PathBuf,
    backup: Option<PathBuf>,
) -> Result<(), BootstrapError> {
    install_staged_snapshot_using(
        staged,
        target,
        backup,
        || {},
        |_, _| Err(io::Error::other("injected state snapshot rename failure")),
    )
    .await
}

async fn install_staged_snapshot_using<F, R>(
    staged: PathBuf,
    target: PathBuf,
    backup: Option<PathBuf>,
    before_rename: F,
    rename: R,
) -> Result<(), BootstrapError>
where
    F: FnOnce() + Send + 'static,
    R: FnOnce(&Path, &Path) -> io::Result<()> + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        if zebra_chain::shutdown::is_shutting_down() {
            return Err(shutdown_before_install());
        }

        let parent = target.parent().ok_or_else(|| {
            SnapshotError::Unsupported(format!("no directory for the state database {target:?}"))
        })?;
        if target.exists() {
            return Err(SnapshotError::Unsupported(format!(
                "a state database already exists at {target:?}"
            ))
            .into());
        }
        fs::create_dir_all(parent).map_err(|source| SnapshotError::Io {
            path: parent.to_path_buf(),
            source,
        })?;

        if zebra_chain::shutdown::is_shutting_down() {
            return Err(shutdown_before_install());
        }

        // Shutdown either wins before this guard and prevents the rename, or waits for this guard
        // to finish. There is no detached rename left racing runtime teardown after a SIGINT.
        let _install = SnapshotInstall::begin()?;
        let preserved_backup = preserve_backup(backup)?;
        before_rename();

        if let Err(source) = rename(&staged, &target) {
            if let Some((preserved, backup)) = preserved_backup {
                let restore = if backup.exists() {
                    Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "another process created a non-finalized backup during installation",
                    ))
                } else {
                    fs::rename(&preserved, &backup)
                };
                if let Err(restore) = restore {
                    return Err(SnapshotError::Unsupported(format!(
                        "the state snapshot rename failed ({source}); the previous backup could not \
                         be restored to {backup:?} ({restore}), and remains preserved at \
                         {preserved:?}; the staged database remains at {staged:?}"
                    ))
                    .into());
                }
            }
            return Err(SnapshotError::Io {
                path: target.clone(),
                source,
            }
            .into());
        }

        if let Some((preserved, _)) = preserved_backup {
            if let Err(error) = fs::remove_dir_all(&preserved) {
                if error.kind() != io::ErrorKind::NotFound {
                    warn!(
                        ?error,
                        path = %preserved.display(),
                        "the state snapshot was installed, but its preserved backup could not be deleted"
                    );
                }
            }
        }

        if !cfg!(windows) {
            if let Err(error) = fs::File::open(parent).and_then(|directory| directory.sync_all()) {
                warn!(
                    ?error,
                    path = %parent.display(),
                    "the state snapshot was installed, but its parent directory could not be synced"
                );
            }
        }

        Ok(())
    })
    .await
    .map_err(|error| {
        BootstrapError::Aborted(format!("the state snapshot install task failed: {error}"))
    })?
}

/// Moves `backup` to a unique sibling, returning the preserved and original paths.
fn preserve_backup(backup: Option<PathBuf>) -> Result<Option<(PathBuf, PathBuf)>, SnapshotError> {
    let Some(backup) = backup else {
        return Ok(None);
    };
    let parent = backup.parent().ok_or_else(|| {
        SnapshotError::Unsupported(format!("no parent directory for the backup {backup:?}"))
    })?;
    let name = backup.file_name().ok_or_else(|| {
        SnapshotError::Unsupported(format!("no directory name for the backup {backup:?}"))
    })?;
    let preserved = parent.join(format!(
        ".{}.snapshot-install-{}",
        name.to_string_lossy(),
        import_nonce()
    ));

    match fs::rename(&backup, &preserved) {
        Ok(()) => Ok(Some((preserved, backup))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SnapshotError::Io {
            path: backup,
            source,
        }),
    }
}

/// Finishes a successful installation, logging cleanup failures instead of
/// sending the caller through fallback with a database already installed.
async fn finish_install(
    piece_paths: HashMap<ObjectHash, PathBuf>,
    temp_root: &Path,
    store: &ArtifactStore,
    serve_state_snapshots: bool,
) -> Result<(), BootstrapError> {
    let store = store.clone();
    let paths: Vec<(ObjectHash, PathBuf)> = piece_paths.into_iter().collect();
    if let Err(error) = tokio::task::spawn_blocking(move || {
        for (hash, lease) in paths {
            if let Err(error) = fs::remove_file(&lease) {
                if error.kind() != io::ErrorKind::NotFound {
                    warn!(?error, path = %lease.display(), "could not delete this attempt's snapshot piece lease");
                }
            }

            if !serve_state_snapshots {
                let shared = store.object_path(hash);
                if let Err(error) = fs::remove_file(&shared) {
                    if error.kind() != io::ErrorKind::NotFound {
                        warn!(?error, path = %shared.display(), "could not delete a shared snapshot piece");
                    }
                }
            }
        }
    })
    .await
    {
        warn!(
            ?error,
            "the snapshot piece deletion task failed after installation"
        );
    }
    if let Err(error) = remove_dir(temp_root).await {
        warn!(
            ?error,
            "could not delete this attempt's state snapshot import workspace after installation"
        );
    }

    Ok(())
}

/// Resolves the configured `host:port` strings, one address per IP, up to
/// [`MAX_BOOTSTRAP_PEERS`]. Hosts that fail to resolve are skipped with a
/// warning.
async fn resolve_peers(peers: &[String]) -> Vec<PeerSocketAddr> {
    let mut resolved = Vec::new();

    for host in peers {
        match timeout(PEER_RESOLVE_TIMEOUT, tokio::net::lookup_host(host.as_str())).await {
            Ok(Ok(addrs)) => {
                resolved.push(addrs.map(PeerSocketAddr::from).collect());
            }
            Ok(Err(error)) => {
                warn!(?error, host, "could not resolve a state snapshot peer");
            }
            Err(_) => {
                warn!(
                    host,
                    timeout = ?PEER_RESOLVE_TIMEOUT,
                    "resolving a state snapshot peer timed out"
                );
            }
        }
    }

    interleave_resolved_peers(resolved)
}

/// Selects resolved addresses round-robin by host, keeping only the first
/// address for each IP, up to [`MAX_BOOTSTRAP_PEERS`]. One hostname with many
/// answers therefore cannot eclipse the other configured peers.
fn interleave_resolved_peers(hosts: Vec<Vec<PeerSocketAddr>>) -> Vec<PeerSocketAddr> {
    let mut selected = Vec::new();
    let mut seen_ips = HashSet::new();
    let mut next = vec![0; hosts.len()];

    while selected.len() < MAX_BOOTSTRAP_PEERS {
        let mut progressed = false;
        for (host, addrs) in hosts.iter().enumerate() {
            let Some(addr) = addrs.get(next[host]).copied() else {
                continue;
            };
            next[host] += 1;
            progressed = true;
            if seen_ips.insert(addr.ip()) {
                selected.push(addr);
                if selected.len() == MAX_BOOTSTRAP_PEERS {
                    break;
                }
            }
        }
        if !progressed {
            break;
        }
    }

    selected
}

/// Fetches the manifest from the first peer that serves it with the
/// commitment's hash, decodes it, and checks it against the commitment and the
/// network.
pub(crate) async fn fetch_manifest<S>(
    peers: &mut Peers<S>,
    commitment: StateSnapshotCommitment,
    network: &Network,
    deadline: Instant,
) -> Result<(Manifest, Vec<u8>), BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let size_limit = u32::try_from(MAX_MANIFEST_BYTES).expect("the manifest limit fits in u32");
    let description = format!("manifest {}", commitment.manifest);
    let bytes = fetch_object_with_retries(
        peers,
        commitment.manifest,
        size_limit,
        deadline,
        &description,
    )
    .await?;
    let manifest = Manifest::decode(&bytes, network).map_err(|error| {
        BootstrapError::Rejected(format!("the manifest does not decode: {error}"))
    })?;
    check_manifest(&manifest, &commitment, network)?;

    Ok((manifest, bytes))
}

/// Fetches one piece from the first peer that serves it with its manifest
/// hash. The manifest's size is checked only after the object hash has
/// authenticated the bytes: a disagreement is the snapshot's fault, not the
/// peer's.
async fn fetch_piece<S>(
    peers: &mut Peers<S>,
    piece: &PieceDescriptor,
    deadline: Instant,
) -> Result<Vec<u8>, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let size_limit = u32::try_from(MAX_PIECE_BYTES).expect("the piece limit fits in u32");
    let description = format!("{} piece {} ({})", piece.component, piece.index, piece.hash);
    let bytes =
        fetch_object_with_retries(peers, piece.hash, size_limit, deadline, &description).await?;
    let expected = usize::try_from(piece.size).expect("u32 fits in usize");
    if bytes.len() != expected {
        return Err(BootstrapError::Rejected(format!(
            "the authenticated {} is {} bytes, but its manifest entry sizes it at {expected}",
            description,
            bytes.len()
        )));
    }

    Ok(bytes)
}

/// Fetches `hash` until one peer serves it or `deadline` passes. Refusals back
/// off exponentially without excluding or blaming the peer; other peers are
/// tried immediately, and every wait remains inside the overall deadline. One
/// attempt gets at most an equal share of the time left and the peers left in
/// that pass, so a peer that keeps making a little progress cannot consume the
/// shares of the peers after it.
async fn fetch_object_with_retries<S>(
    peers: &mut Peers<S>,
    hash: ObjectHash,
    size_limit: u32,
    deadline: Instant,
    description: &str,
) -> Result<Vec<u8>, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let mut retries: HashMap<PeerSocketAddr, (u32, Instant)> = HashMap::new();

    loop {
        if Instant::now() >= deadline || peers.is_empty() {
            break;
        }

        let snapshot = peers.snapshot();
        let peer_count = snapshot.len();
        for (index, (peer, service)) in snapshot.into_iter().enumerate() {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            if retries
                .get(&peer)
                .is_some_and(|(_, retry_at)| *retry_at > now)
            {
                continue;
            }

            let peers_left = u32::try_from(peer_count - index)
                .expect("the bootstrap has at most eight peers left in each pass");
            let remaining = deadline.saturating_duration_since(now);
            let attempt_deadline = now + remaining / peers_left;
            match fetch_object(service, hash, size_limit, attempt_deadline).await {
                Ok(bytes) => return Ok(bytes),
                Err(FetchError::Peer(reason)) => {
                    retries.remove(&peer);
                    peers.blame(peer, &reason);
                }
                Err(FetchError::Skip(reason)) => {
                    info!(%peer, reason, "a bootstrap peer did not serve an object");
                    retries.insert(peer, (0, Instant::now() + OBJECT_SKIP_BACKOFF));
                }
                Err(FetchError::Refused(reason)) => {
                    let refusals = retries
                        .get(&peer)
                        .map_or(0, |(refusals, _)| *refusals)
                        .saturating_add(1);
                    let doublings = refusals.saturating_sub(1).min(31);
                    let backoff = MIN_OBJECT_REFUSED_BACKOFF
                        .saturating_mul(1 << doublings)
                        .min(MAX_OBJECT_REFUSED_BACKOFF);
                    info!(%peer, reason, ?backoff, "a bootstrap peer refused an object; backing off");
                    retries.insert(peer, (refusals, Instant::now() + backoff));
                }
            }
        }

        let now = Instant::now();
        let Some(retry_at) = peers
            .addrs()
            .into_iter()
            .filter_map(|peer| retries.get(&peer).map(|(_, retry_at)| *retry_at))
            .min()
        else {
            break;
        };
        if retry_at >= deadline {
            break;
        }
        if retry_at > now {
            sleep_until(retry_at.into()).await;
        }
    }

    Err(BootstrapError::Aborted(format!(
        "no bootstrap peer served the {description} before its deadline"
    )))
}

/// Fetches the object `hash` from one peer in `get-object` requests of up to
/// `size_limit` bytes, continuing from the end of each short answer, and
/// checks that the bytes hash to `hash`.
///
/// Returns a peer fault for a size over `size_limit`, a size that changes
/// between answers, data past the size, or a hash mismatch; a refusal for a
/// failed request stream; and a skip for a not-found answer, an empty answer,
/// or the deadline.
pub(crate) async fn fetch_object<S>(
    service: S,
    hash: ObjectHash,
    size_limit: u32,
    deadline: Instant,
) -> Result<Vec<u8>, FetchError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let mut data: Vec<u8> = Vec::new();
    let mut expected_size: Option<u64> = None;

    // Every round adds at least one byte and `data` never exceeds `size_limit`, so the loop runs
    // at most `size_limit` times, each bounded by the request timeout and the deadline.
    loop {
        let remaining_time = deadline.saturating_duration_since(Instant::now());
        if remaining_time.is_zero() {
            return Err(FetchError::Skip("the deadline passed".to_string()));
        }

        // `data.len() <= size_limit`, so both fit in u32 and the subtraction cannot underflow.
        let offset = u64::try_from(data.len()).expect("a buffer length fits in u64");
        let received = u32::try_from(data.len()).expect("the buffer is at most `size_limit`");
        let request = zn::Request::Object {
            hash,
            offset,
            length: size_limit.saturating_sub(received),
        };

        let response = timeout(
            remaining_time.min(zn::constants::REQUEST_TIMEOUT),
            service.clone().oneshot(request),
        )
        .await;

        match response {
            Err(_) => return Err(FetchError::Skip("the request timed out".to_string())),
            Ok(Err(error)) => {
                return Err(FetchError::Refused(format!("the request failed: {error}")));
            }
            Ok(Ok(zn::Response::ObjectNotFound)) => {
                return Err(FetchError::Skip("the object was not found".to_string()));
            }
            Ok(Ok(zn::Response::Object { size, data: chunk })) => {
                if size > u64::from(size_limit) {
                    return Err(FetchError::Peer(format!(
                        "claims the object is {size} bytes, over the {size_limit} byte limit"
                    )));
                }
                match expected_size {
                    None => expected_size = Some(size),
                    Some(expected) if expected != size => {
                        return Err(FetchError::Peer(format!(
                            "changed the object's size from {expected} to {size}"
                        )));
                    }
                    Some(_) => {}
                }
                if chunk.is_empty() {
                    return Err(FetchError::Skip("the answer was empty".to_string()));
                }
                // Both lengths are at most `size_limit`, so the sum fits in u64 without overflow.
                let total = offset.saturating_add(
                    u64::try_from(chunk.len()).expect("a buffer length fits in u64"),
                );
                if total > size {
                    return Err(FetchError::Peer(format!(
                        "served {total} bytes of an object it sizes at {size}"
                    )));
                }
                data.extend_from_slice(&chunk);
                if total == size {
                    break;
                }
            }
            Ok(Ok(other)) => {
                return Err(FetchError::Skip(format!(
                    "the request got an unexpected {other} response"
                )));
            }
        }
    }

    if ObjectHash::of(&data) != hash {
        return Err(FetchError::Peer(format!(
            "served an object that hashes to {}, not {hash}",
            ObjectHash::of(&data)
        )));
    }

    Ok(data)
}

/// Checks the manifest's fields against the commitment, the network, and the
/// running database format.
///
/// A manifest that contradicts the commitment or the network is the
/// snapshot's fault and is rejected. A manifest in another database format
/// major version is a mismatch with this binary, not a fault of the snapshot,
/// so the bootstrap aborts without marking it: another release may import it.
pub(crate) fn check_manifest(
    manifest: &Manifest,
    commitment: &StateSnapshotCommitment,
    network: &Network,
) -> Result<(), BootstrapError> {
    if manifest.height != commitment.height {
        return Err(BootstrapError::Rejected(format!(
            "the manifest is for height {}, the commitment for {}",
            manifest.height.0, commitment.height.0
        )));
    }
    if manifest.block_hash != commitment.block_hash {
        return Err(BootstrapError::Rejected(format!(
            "the manifest names block {}, the commitment {}",
            manifest.block_hash, commitment.block_hash
        )));
    }
    if manifest.genesis_hash != network.genesis_hash() {
        return Err(BootstrapError::Rejected(format!(
            "the manifest is for a chain with genesis {}, not this network's {}",
            manifest.genesis_hash,
            network.genesis_hash()
        )));
    }
    let expected_window_start = window_start(manifest.height);
    if manifest.window_start != expected_window_start {
        return Err(BootstrapError::Rejected(format!(
            "the manifest's window starts at {}, not at {}",
            manifest.window_start.0, expected_window_start.0
        )));
    }
    if manifest.parameters_hash != network.state_snapshot_parameters_hash() {
        return Err(BootstrapError::Rejected(
            "the manifest's parameters hash does not match the network's consensus parameters"
                .to_string(),
        ));
    }
    let running_major = zebra_state::state_database_format_version_in_code().major;
    if u64::from(manifest.db_format_major) != running_major {
        return Err(BootstrapError::Aborted(format!(
            "the snapshot is in database format v{}, but this zebrad runs v{running_major}: \
             use a zebrad release with that format, or a snapshot exported in v{running_major}",
            manifest.db_format_major
        )));
    }

    Ok(())
}

/// Returns the first height with a complete block in a snapshot at `height`:
/// `L = max(1, H - 2303)`.
pub(crate) fn window_start(height: Height) -> Height {
    Height(
        height
            .0
            .saturating_sub(zn::constants::NODE_NETWORK_LIMITED_BLOCKS - 1)
            .max(1),
    )
}

/// Returns the descriptor of the single `history_tree` piece, which holds the
/// one record of that column family.
fn history_tree_piece(manifest: &Manifest) -> Result<PieceDescriptor, BootstrapError> {
    let component = manifest.component(ComponentId::HistoryTree);
    match component.pieces.as_slice() {
        [piece] => Ok(PieceDescriptor {
            component: ComponentId::HistoryTree,
            index: 0,
            size: piece.size,
            hash: piece.hash,
        }),
        pieces => Err(BootstrapError::Rejected(format!(
            "the history tree component has {} pieces, but holds one record",
            pieces.len()
        ))),
    }
}

/// Why a header chain from `hash(H)` is not accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ChainCheckError {
    /// The headers do not chain from `hash(H)` in order, or there are not
    /// exactly as many as heights: the peer is at fault.
    Peer(String),

    /// The headers chain from `hash(H)` but end at `found` instead of the
    /// checkpoint: `hash(H)` is not on the chain the peer serves.
    NotAtCheckpoint {
        /// The hash of the last header.
        found: block::Hash,
    },
}

/// Checks that `headers` are the headers of heights `H + 1..=C`, chaining
/// from `hash_h` to the `checkpoint` `(C, hash(C))`.
pub(crate) fn check_header_chain(
    hash_h: block::Hash,
    h: Height,
    headers: &[Arc<Header>],
    checkpoint: (Height, block::Hash),
) -> Result<(), ChainCheckError> {
    let (checkpoint_height, checkpoint_hash) = checkpoint;
    let expected_len = usize::try_from(checkpoint_height - h)
        .ok()
        .filter(|len| *len > 0)
        .ok_or_else(|| {
            ChainCheckError::Peer("the checkpoint is not above the snapshot height".to_string())
        })?;

    if headers.len() != expected_len {
        return Err(ChainCheckError::Peer(format!(
            "served {} headers for the {expected_len} heights up to the checkpoint",
            headers.len()
        )));
    }

    let mut previous = hash_h;
    for (offset, header) in headers.iter().enumerate() {
        if header.previous_block_hash != previous {
            return Err(ChainCheckError::Peer(format!(
                "header {offset} after the snapshot block does not chain from {previous}"
            )));
        }
        previous = header.hash();
    }

    if previous != checkpoint_hash {
        return Err(ChainCheckError::NotAtCheckpoint { found: previous });
    }

    Ok(())
}

/// Fetches the headers `H + 1..=C` from the first peer that serves a chain
/// from `hash_h` to the `checkpoint`, in rounds of at most
/// [`MAX_HEADERS_PER_RESPONSE`].
///
/// Returns `None` when no peer serves headers after `hash_h`, so the check is
/// deferred. A peer whose headers do not chain is blamed. A peer whose chain
/// ends away from the checkpoint is blamed too, and if no peer's chain reaches
/// it the bootstrap aborts: either `hash(H)` is not on the checkpointed chain
/// or the peers are on a fork, which the operator has to look into.
pub(crate) async fn fetch_header_chain<S>(
    peers: &mut Peers<S>,
    hash_h: block::Hash,
    h: Height,
    checkpoint: (Height, block::Hash),
) -> Result<Option<Vec<Arc<Header>>>, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let expected_len = usize::try_from(checkpoint.0 - h).unwrap_or(0);
    // One extra round covers a peer that serves fewer than the maximum per response.
    let max_rounds = expected_len
        .div_ceil(MAX_HEADERS_PER_RESPONSE)
        .saturating_add(1);
    let mut chains_missed_checkpoint = false;

    for (peer, service) in peers.snapshot() {
        let mut headers: Vec<Arc<Header>> = Vec::with_capacity(expected_len);

        let fetched = async {
            for _round in 0..max_rounds {
                if headers.len() >= expected_len {
                    return Ok(());
                }
                let locator = headers.last().map_or(hash_h, |header| header.hash());
                let request = zn::Request::FindHeaders {
                    known_blocks: vec![locator],
                    stop: Some(checkpoint.1),
                };
                let response = timeout(
                    zn::constants::REQUEST_TIMEOUT,
                    service.clone().oneshot(request),
                )
                .await;
                match response {
                    Err(_) => return Err("the request timed out".to_string()),
                    Ok(Err(error)) => return Err(format!("the request failed: {error}")),
                    Ok(Ok(zn::Response::BlockHeaders(batch))) => {
                        if batch.is_empty() {
                            return Err("no headers after the locator".to_string());
                        }
                        // Headers past the checkpoint are not needed; the count is checked below.
                        headers.extend(
                            batch
                                .into_iter()
                                .map(|counted| counted.header)
                                .take(expected_len.saturating_sub(headers.len())),
                        );
                    }
                    Ok(Ok(other)) => {
                        return Err(format!("the request got an unexpected {other} response"));
                    }
                }
            }
            if headers.len() >= expected_len {
                Ok(())
            } else {
                Err("the headers ran out before the checkpoint".to_string())
            }
        }
        .await;

        if let Err(reason) = fetched {
            info!(%peer, reason, "a bootstrap peer did not serve the headers after the snapshot block");
            continue;
        }

        match check_header_chain(hash_h, h, &headers, checkpoint) {
            Ok(()) => return Ok(Some(headers)),
            Err(ChainCheckError::Peer(reason)) => peers.blame(peer, &reason),
            Err(ChainCheckError::NotAtCheckpoint { found }) => {
                chains_missed_checkpoint = true;
                peers.blame(
                    peer,
                    &format!(
                        "its headers from the snapshot block reach {found} at height {}, not the \
                         checkpoint {}",
                        checkpoint.0 .0, checkpoint.1
                    ),
                );
            }
        }
    }

    if chains_missed_checkpoint {
        return Err(BootstrapError::Aborted(format!(
            "the headers the bootstrap peers chain from the snapshot block {hash_h} do not reach \
             the checkpoint {} at height {}: either the snapshot's block hash is not on the \
             checkpointed chain, or the peers are on a fork; check the commitment and the peers",
            checkpoint.1, checkpoint.0 .0
        )));
    }

    Ok(None)
}

/// Fetches block `successor_hash` from eligible peers until a body passes both its structural and
/// `hashBlockCommitments` checks, retaining at most `copies` valid bodies. Returns every fetched
/// body with its check result so the caller can blame only peers that served invalid bodies.
pub(crate) async fn fetch_successor_copies<S>(
    peers: &Peers<S>,
    history_root: &ChainHistoryMmrRootHash,
    hash_h: block::Hash,
    successor_hash: block::Hash,
    successor_height: Height,
    copies: usize,
) -> Vec<(PeerSocketAddr, Result<Arc<Block>, String>)>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let mut found = Vec::with_capacity(copies);
    let mut valid_copies = 0;

    for (peer, service) in peers.snapshot() {
        if valid_copies == copies {
            break;
        }

        let request = zn::Request::BlocksByHash([successor_hash].into_iter().collect());
        let response = timeout(
            zn::constants::REQUEST_TIMEOUT,
            service.clone().oneshot(request),
        )
        .await;
        match response {
            Ok(Ok(zn::Response::Blocks(blocks))) => match blocks.into_iter().next() {
                Some(InventoryResponse::Available((block, _))) => {
                    let checked =
                        check_successor_block(hash_h, successor_hash, successor_height, &block)
                            .map(|()| block)
                            .and_then(|block| {
                                if check_successor_commitment(history_root, &block) {
                                    Ok(block)
                                } else {
                                    Err("its copy of the successor's authorizing data does not \
                                         commit to the snapshot's history tree"
                                        .to_string())
                                }
                            });
                    let is_valid = checked.is_ok();
                    if is_valid {
                        valid_copies += 1;
                    }
                    found.push((peer, checked));
                    if is_valid {
                        break;
                    }
                }
                Some(InventoryResponse::Missing(_)) | None => {
                    info!(%peer, "a bootstrap peer does not have the block after the snapshot height");
                }
            },
            Ok(Ok(other)) => {
                info!(%peer, response = %other, "unexpected response to a block request");
            }
            Ok(Err(error)) => {
                info!(%peer, %error, "a block request to a bootstrap peer failed");
            }
            Err(_) => {
                info!(%peer, "a block request to a bootstrap peer timed out");
            }
        }
    }

    found
}

/// Checks that `block` is block `H + 1`: it hashes to the authenticated
/// `successor_hash`, chains from `hash_h`, has the successor's coinbase
/// height, and its merkle root covers its transactions.
pub(crate) fn check_successor_block(
    hash_h: block::Hash,
    successor_hash: block::Hash,
    successor_height: Height,
    block: &Block,
) -> Result<(), String> {
    if block.hash() != successor_hash {
        return Err(format!(
            "the block hashes to {}, not to the successor {successor_hash}",
            block.hash()
        ));
    }
    if block.header.previous_block_hash != hash_h {
        return Err(format!(
            "the block's parent is {}, not the snapshot block {hash_h}",
            block.header.previous_block_hash
        ));
    }
    if block.coinbase_height() != Some(successor_height) {
        return Err(format!(
            "the block's coinbase height is {:?}, not {}",
            block.coinbase_height(),
            successor_height.0
        ));
    }
    let merkle_root: block::merkle::Root = block.transactions.iter().collect();
    if block.header.merkle_root != merkle_root {
        return Err("the block's merkle root does not cover its transactions".to_string());
    }

    Ok(())
}

/// Returns whether `block`'s `hashBlockCommitments` commits to `history_root`
/// as the chain history root of its parent (ZIP 244), which ties the
/// snapshot's history tree, and through it the frontiers, to the chain.
pub(crate) fn check_successor_commitment(
    history_root: &ChainHistoryMmrRootHash,
    block: &Block,
) -> bool {
    let auth_data_root: AuthDataRoot = block.transactions.iter().collect();
    let expected =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(history_root, &auth_data_root);
    block.header.commitment_bytes.0 == <[u8; 32]>::from(expected)
}

/// Runs the chain checks and returns the [`SnapshotBase`] `DEFERRED_*` bits of
/// the checks no peer could serve.
///
/// A snapshot whose `hash(H)` differs from the checkpoint at `H` is rejected.
/// A successor check succeeds when any fetched copy commits to the history
/// tree; invalid copies blame only their peers.
pub(crate) async fn check_chain<S>(
    peers: &mut Peers<S>,
    network: &Network,
    commitment: StateSnapshotCommitment,
    history_root: &ChainHistoryMmrRootHash,
) -> Result<u8, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    let h = commitment.height;
    let hash_h = commitment.block_hash;
    let checkpoints = network.checkpoint_list();
    let mut deferred_checks = 0u8;

    // Check 1, when `H` is a checkpoint height: the binary's checkpoint is the authority.
    let h_is_checkpoint = match checkpoints.hash(h) {
        Some(expected) if expected != hash_h => {
            return Err(BootstrapError::Rejected(format!(
                "the snapshot names block {hash_h} at height {}, but the checkpoint there is \
                 {expected}",
                h.0
            )));
        }
        Some(_) => true,
        None => false,
    };

    // The decision rule requires `H + 1 <= max_checkpoint_height`, so both exist.
    let successor_height = (h + 1).ok_or_else(|| {
        BootstrapError::Aborted("the snapshot height has no successor".to_string())
    })?;
    let checkpoint_height = checkpoints
        .min_height_in_range(successor_height..)
        .ok_or_else(|| {
            BootstrapError::Aborted("no checkpoint above the snapshot height".to_string())
        })?;
    let checkpoint_hash = checkpoints
        .hash(checkpoint_height)
        .expect("a height from the checkpoint list has a hash");
    let checkpoint = (checkpoint_height, checkpoint_hash);

    // The chain to the next checkpoint authenticates `hash(H)` when it is not a checkpoint, and
    // always authenticates the header of `H + 1`, which check 3 needs.
    let Some(headers) = fetch_header_chain(peers, hash_h, h, checkpoint).await? else {
        if !h_is_checkpoint {
            warn!(
                height = h.0,
                hash = %hash_h,
                next_checkpoint = checkpoint_height.0,
                "the snapshot block hash could not be checked against the chain: no bootstrap \
                 peer served the headers from it to the next checkpoint; the checkpoint verifier \
                 anchors that checkpoint back to the snapshot block when the node syncs, and \
                 stalls on a wrong one"
            );
            deferred_checks |= SnapshotBase::DEFERRED_BLOCK_HASH;
        }
        warn!(
            height = successor_height.0,
            "the block after the snapshot height could not be fetched from any bootstrap peer: \
             the first commit above the snapshot height checks the snapshot's history tree and \
             tree roots instead"
        );
        deferred_checks |= SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS;
        return Ok(deferred_checks);
    };

    info!(
        height = h.0,
        hash = %hash_h,
        checkpoint = checkpoint_height.0,
        headers = headers.len(),
        "the snapshot block hash chains to the next checkpoint"
    );

    let successor_hash = headers
        .first()
        .expect("a verified chain to a checkpoint above H has a header")
        .hash();

    // Check 3: one matching copy is enough. Transaction authorizing data is
    // not covered by the txid Merkle root, so a bad copy can have the
    // authenticated header yet fail the commitment check. Every fetched body
    // must be checked before deciding that no peer served the real block.
    let copies = fetch_successor_copies(
        peers,
        history_root,
        hash_h,
        successor_hash,
        successor_height,
        SUCCESSOR_COPIES,
    )
    .await;
    let copy_count = copies.len();
    let mut matching_copy = false;
    for (peer, block) in copies {
        let block = match block {
            Ok(block) => block,
            Err(reason) => {
                peers.blame(peer, &reason);
                continue;
            }
        };

        if check_successor_commitment(history_root, &block) {
            matching_copy = true;
        } else {
            peers.blame(
                peer,
                "its copy of the successor's authorizing data does not commit to the snapshot's \
                 history tree",
            );
        }
    }

    if matching_copy {
        info!(
            height = successor_height.0,
            hash = %successor_hash,
            "a copy of the block after the snapshot height commits to the snapshot's history tree"
        );
        return Ok(deferred_checks);
    }

    if copy_count == 0 {
        warn!(
            height = successor_height.0,
            hash = %successor_hash,
            "the block after the snapshot height could not be fetched from any bootstrap peer: \
             the first commit above the snapshot height checks the snapshot's history tree and \
             tree roots instead"
        );
        deferred_checks |= SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS;
        return Ok(deferred_checks);
    }

    Err(BootstrapError::Aborted(format!(
        "none of the {copy_count} copies of block {successor_hash} at height {} passed every \
         successor check; only the peers that served those copies were excluded",
        successor_height.0
    )))
}

/// Builds SST files, with a notification when a build enters the queue. The
/// notification lets tests observe queue backpressure independently of CPU
/// parallelism.
trait SstBuilder: Clone + Send + Sync + 'static {
    fn queued(&self, _pending: usize) {}

    fn build(
        self,
        piece_path: PathBuf,
        component: ComponentId,
        piece: PieceManifest,
        sst_path: PathBuf,
    ) -> Result<u64, SnapshotError>;
}

impl<F> SstBuilder for F
where
    F: Fn(PathBuf, ComponentId, PieceManifest, PathBuf) -> Result<u64, SnapshotError>
        + Clone
        + Send
        + Sync
        + 'static,
{
    fn build(
        self,
        piece_path: PathBuf,
        component: ComponentId,
        piece: PieceManifest,
        sst_path: PathBuf,
    ) -> Result<u64, SnapshotError> {
        self(piece_path, component, piece, sst_path)
    }
}

/// Downloads every piece but the history tree's with the scheduler, builds
/// each verified piece into an SST file on a bounded blocking pool, and
/// returns the stored pieces by hash.
async fn download_and_build<S>(
    peers: Peers<S>,
    manifest: &Manifest,
    store: AttemptStore,
    temp_root: &Path,
    tree_piece: VerifiedPiece,
) -> Result<HashMap<ObjectHash, PathBuf>, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
{
    download_and_build_with(
        peers,
        manifest,
        store,
        temp_root,
        tree_piece,
        |path: PathBuf, component, piece, sst: PathBuf| {
            build_piece_sst(&path, component, piece, &sst)
        },
    )
    .await
}

async fn download_and_build_with<S, B>(
    peers: Peers<S>,
    manifest: &Manifest,
    store: AttemptStore,
    temp_root: &Path,
    tree_piece: VerifiedPiece,
    builder: B,
) -> Result<HashMap<ObjectHash, PathBuf>, BootstrapError>
where
    S: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    S::Future: Send + 'static,
    B: SstBuilder,
{
    let pieces: Vec<PieceDescriptor> = manifest
        .pieces()
        .filter(|(component, _, _)| *component != ComponentId::HistoryTree)
        .map(|(component, index, piece)| PieceDescriptor {
            component,
            index,
            size: piece.size,
            hash: piece.hash,
        })
        .collect();
    let total_pieces = pieces.len().saturating_add(1);
    let total_bytes = manifest.total_piece_bytes();

    let (verified_sender, mut verified) = mpsc::channel(VERIFIED_PIECE_CHANNEL_BOUND);
    let (blame_sender, mut blames) = mpsc::channel(BLAME_CHANNEL_BOUND);
    let scheduler = Scheduler::new(
        SchedulerConfig::default(),
        pieces,
        peers.into_clients(),
        store.shared.clone(),
        verified_sender,
        blame_sender,
    );
    let mut scheduler = Some(tokio::spawn(scheduler.run_storing_in(store)));
    let mut blame_logger = tokio::spawn(async move {
        while let Some(blame) = blames.recv().await {
            warn!(
                peer = %blame.peer,
                reason = ?blame.reason,
                "the scheduler blamed a bootstrap peer"
            );
        }
    });

    // Half the cores build SST files, so the download and the node's other work keep the rest.
    let parallelism = std::thread::available_parallelism()
        .map_or(1, |cores| cores.get() / 2)
        .max(1);
    let permits = Arc::new(Semaphore::new(parallelism));
    let mut builds: JoinSet<Result<u64, BootstrapError>> = JoinSet::new();
    let temp_root = temp_root.to_path_buf();
    let spawn_build = |builds: &mut JoinSet<_>, piece: &VerifiedPiece| {
        builder.queued(builds.len().saturating_add(1));
        let builder = builder.clone();
        let permits = permits.clone();
        let path = piece.path.clone();
        let component = piece.descriptor.component;
        let piece_manifest = PieceManifest {
            size: piece.descriptor.size,
            hash: piece.descriptor.hash,
        };
        let sst = sst_path(&temp_root, component, piece.descriptor.index);
        builds.spawn(async move {
            let _permit = permits.acquire_owned().await.map_err(|_| {
                BootstrapError::Aborted("the SST build pool was closed".to_string())
            })?;
            tokio::task::spawn_blocking(move || {
                if let Some(parent) = sst.parent() {
                    fs::create_dir_all(parent).map_err(|source| SnapshotError::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                builder.build(path, component, piece_manifest, sst)
            })
            .await
            .map_err(|error| BootstrapError::Aborted(format!("an SST build task failed: {error}")))?
            .map_err(snapshot_fault)
        });
    };

    let mut piece_paths = HashMap::with_capacity(total_pieces);
    let mut verified_count: usize = 1;
    let mut verified_bytes = u64::from(tree_piece.descriptor.size);
    let mut built: usize = 0;
    let mut records: u64 = 0;
    piece_paths.insert(tree_piece.descriptor.hash, tree_piece.path.clone());
    spawn_build(&mut builds, &tree_piece);

    let mut handle_build = |result: Result<Result<u64, BootstrapError>, tokio::task::JoinError>| {
        let piece_records = result.map_err(|error| {
            BootstrapError::Aborted(format!("an SST build task failed: {error}"))
        })??;
        built = built.saturating_add(1);
        records = records.saturating_add(piece_records);
        Ok::<(), BootstrapError>(())
    };

    let outcome: Result<(), BootstrapError> = async {
        loop {
            tokio::select! {
                received = verified.recv(), if builds.len() < VERIFIED_PIECE_CHANNEL_BOUND => {
                    match received {
                        Some(piece) => {
                            if verified_count == 1 {
                                info!(
                                    component = %piece.descriptor.component,
                                    index = piece.descriptor.index,
                                    size = piece.descriptor.size,
                                    hash = %piece.descriptor.hash,
                                    "{}",
                                    Stage::FirstPieceVerified
                                );
                            }
                            verified_count = verified_count.saturating_add(1);
                            verified_bytes = verified_bytes.saturating_add(u64::from(piece.descriptor.size));
                            piece_paths.insert(piece.descriptor.hash, piece.path.clone());
                            spawn_build(&mut builds, &piece);
                        }
                        // The scheduler dropped its sender: it returned.
                        None => break,
                    }
                },
                Some(result) = builds.join_next(), if !builds.is_empty() => {
                    handle_build(result)?;
                }
            }
        }

        scheduler
            .take()
            .expect("the scheduler is joined only once")
            .await
            .map_err(|error| {
                BootstrapError::Aborted(format!("the piece scheduler task failed: {error}"))
            })??;
        if verified_count != total_pieces {
            return Err(BootstrapError::Aborted(format!(
                "the scheduler finished with {verified_count} of {total_pieces} pieces verified"
            )));
        }
        info!(
            pieces = total_pieces,
            bytes = verified_bytes,
            total_bytes,
            "{}",
            Stage::LastPieceVerified
        );

        while let Some(result) = builds.join_next().await {
            handle_build(result)?;
        }
        Ok(())
    }
    .await;

    quiesce_download_tasks(
        &mut verified,
        &mut scheduler,
        &mut blame_logger,
        &mut builds,
    )
    .await;
    outcome?;

    info!(ssts = built, records, "{}", Stage::SstsBuilt);
    Ok(piece_paths)
}

/// Stops reporting and waits for every scheduler and SST build task before the
/// caller removes the import workspace. In particular, draining rather than
/// aborting builds waits for their blocking SST work to finish.
async fn quiesce_download_tasks(
    verified: &mut mpsc::Receiver<VerifiedPiece>,
    scheduler: &mut Option<JoinHandle<Result<(), SchedulerError>>>,
    blame_logger: &mut JoinHandle<()>,
    builds: &mut JoinSet<Result<u64, BootstrapError>>,
) {
    verified.close();
    if let Some(scheduler) = scheduler.take() {
        let _ = scheduler.await;
    }

    blame_logger.abort();
    while let Some(result) = builds.join_next().await {
        drop(result);
    }
    let _ = blame_logger.await;
}

/// Returns a directory name no earlier bootstrap of this node used.
fn import_nonce() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_nanos());
    let sequence = NEXT_IMPORT_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{}-{sequence}", std::process::id())
}

/// Stores `object` as `hash` in `store` on a blocking thread, and returns this attempt's lease.
async fn put_object(
    store: &AttemptStore,
    hash: ObjectHash,
    object: Vec<u8>,
) -> Result<PathBuf, BootstrapError> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.put(hash, &object))
        .await
        .map_err(|error| BootstrapError::Aborted(format!("an object store task failed: {error}")))?
        .map_err(BootstrapError::Snapshot)
}

/// Deletes `dir` and everything under it, if it exists.
async fn remove_dir(dir: &Path) -> Result<(), BootstrapError> {
    let path = dir.to_path_buf();
    tokio::task::spawn_blocking(move || match fs::remove_dir_all(&path) {
        Ok(()) => {
            info!(path = %path.display(), "deleted the state snapshot import directory");
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SnapshotError::Io { path, source }),
    })
    .await
    .map_err(|error| BootstrapError::Aborted(format!("a directory deletion task failed: {error}")))?
    .map_err(BootstrapError::Snapshot)
}
