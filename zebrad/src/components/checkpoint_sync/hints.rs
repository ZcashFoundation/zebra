//! Spentness-hint artifact acquisition, verification and serving.
//!
//! Hints are used only when the state starts empty. The artifact is fetched
//! whole from one `NODE_SYNC_ARTIFACTS` peer with `SyncPeer::get_object`,
//! verified with `ParsedArtifact::verify` against the network's
//! `MaxCheckpoint`, and its bit count checked against
//! `KnownHashes::cumulative_txouts(H)` — all before the first hinted commit.
//! The verified artifact is then sent to the state's
//! `SpentnessHintsReceiver`, saved as a file, and registered in the network's
//! `ObjectIndex` for serving.
//!
//! Ordering with the legacy syncer: with `Mode::Auto` or
//! `Mode::Require` on an empty state, the caller passes a `hints_gate` to
//! `CheckpointSync::new`, so the supervisor claims ownership right after
//! genesis, and the same gate to `ChainSync::with_hints_gate`, so the legacy
//! syncer starts no sync round after genesis before the claim. So nothing
//! commits height 1 before acquisition finishes. With `Auto`, a failed or
//! slow acquisition (5 min) leaves the state's receiver at `None`, opens the
//! gate, and syncs unhinted.
//!
//! # Blame
//!
//! Object bytes are unauthenticated until the whole object is hashed
//! (zips#1344, "`get-object`"). Each candidate peer serves the whole object,
//! so a complete object that does not hash to the pinned `spentness_hash`
//! is that peer's fault, and costs it 100 points. Bytes that do hash to the
//! pin *are* the pinned artifact: if they then fail to parse, to verify, or
//! to match the known-hash output count, the pin itself is defective, every
//! peer would serve the same bytes, and acquisition stops without blaming
//! anyone (zips#1346 S:380-386).
//!
//! # Retry policy
//!
//! - `Auto` gives up after [`AUTO_TIMEOUT`], or after [`AUTO_ROUNDS`] rounds
//!   in which every candidate peer failed, so an unavailable artifact delays
//!   unhinted sync by at most a few minutes.
//! - `Require` retries until it gets the artifact, or until a local or pin
//!   defect makes success impossible.
//! - Each peer must deliver the whole object within a deadline set by
//!   `MIN_RATE`, and keep up that rate from its first answer on, so a peer
//!   that paces short answers is dropped after one of them. Each round starts
//!   at a different peer, and asks peers that failed in earlier rounds last,
//!   so slow peers cannot hold acquisition round after round.
//! - Peers are asked one at a time, because each download holds a buffer of
//!   the whole artifact: a peer that never answers costs [`PIECE_TIMEOUT`]
//!   before the next one is asked.

use std::{
    collections::HashSet,
    future::Future,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::sync::watch;
use tracing::{debug, info, warn};

use zebra_chain::{
    parameters::{
        spentness_hints::{artifact_len, MaxCheckpoint, Mode, ParsedArtifact, VerifiedArtifact},
        Network,
    },
    state_snapshot::ObjectHash,
};
use zebra_network::{
    sync_peer::{ObjectChunk, ObjectEntry, ObjectIndex},
    types::PeerServices,
    SyncPeer, SyncPeerError,
};

#[cfg(test)]
mod tests;

/// How long `Mode::Auto` waits for a verified artifact before syncing
/// unhinted.
pub const AUTO_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How many rounds in which every candidate peer failed `Mode::Auto` makes
/// before syncing unhinted.
pub const AUTO_ROUNDS: usize = 3;

/// How long to wait between rounds over the candidate peers.
const RETRY_INTERVAL: Duration = Duration::from_secs(20);

/// The bytes requested per `get-object` request (8 MiB): a quarter of the
/// protocol's 32 MiB bound, so a slow peer completes one within
/// [`PIECE_TIMEOUT`].
const PIECE_LENGTH: u32 = 8 * 1024 * 1024;

/// How long one `get-object` request may take. `SyncPeer` requests have no
/// timeout of their own.
///
/// A Zebra peer finishes every `get-object` answer, short or not, a few
/// seconds before its 20 second request deadline, so this leaves room for
/// latency, and bounds what a peer that never answers costs.
const PIECE_TIMEOUT: Duration = Duration::from_secs(30);

/// The slowest download rate a peer may average over the whole artifact
/// (256 KiB/s, 2 Mbit/s). It sets each peer's deadline for the whole object,
/// so a peer that answers every request slowly cannot hold acquisition for
/// longer than an honest slow peer needs. After [`RATE_GRACE`], a peer must
/// also have kept up this rate since its first request.
const MIN_RATE: u64 = 256 * 1024;

/// How long a peer's first answers may lag [`MIN_RATE`], for the round trips
/// and the congestion window's growth at the start of a download.
const RATE_GRACE: Duration = Duration::from_secs(5);

/// Requests one peer may spend on the artifact beyond the full-sized pieces
/// it needs, so a peer that dribbles short pieces is dropped.
const EXTRA_REQUESTS: u64 = 16;

/// The misbehavior points for a complete object from one peer that does not
/// hash to the requested hash (zips#1344, "Misbehavior and Banning").
const MISMATCH_PENALTY: u32 = 100;

/// The suffix of a partly written artifact file.
const TEMP_SUFFIX: &str = ".tmp";

/// Why acquisition failed. With `Mode::Auto` the caller syncs unhinted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HintsError {
    /// The network has no pinned `MaxCheckpoint`.
    #[error("no spentness hints are pinned for this network")]
    NotPinned,
    /// No peer had the artifact.
    #[error("no peer serves the spentness-hint artifact")]
    NotFound,
    /// Every candidate artifact failed verification.
    #[error("the spentness-hint artifact failed verification: {0}")]
    Invalid(String),
    /// The bit count differs from the known-hash `txouts` sum.
    #[error(
        "the spentness-hint artifact has {bits} bits, the known-hash list counts {outputs} outputs"
    )]
    OutputCount {
        /// The artifact's `output_count`.
        bits: u64,
        /// `Σ txouts` for `0..=H`.
        outputs: u64,
    },
    /// Local I/O on the artifact file failed.
    #[error("spentness-hint artifact file: {0}")]
    Io(#[from] std::io::Error),
}

/// Loads the artifact from `artifact_dir` if a verified copy is there, or
/// fetches it from `peers`, verifies it (hash, header, and
/// `expected_output_count`, the known-hash `txouts` sum for `0..=H`), and
/// saves it to `artifact_dir`.
///
/// Fetching is whole-object from one peer at a time; a complete object that
/// fails its hash costs that peer 100 points (`SyncPeer::penalize`). Returns
/// `Ok(None)` for `Mode::Off` without doing anything.
///
/// Only the file named by the current pin is ever loaded, and stale artifact
/// files from earlier pins are deleted, so a revoked artifact is never
/// loaded or served. The peers are every connected version 2 peer, whatever
/// dialled it; only those advertising `NODE_SYNC_ARTIFACTS` are asked, and
/// only while the artifact is missing locally.
pub async fn acquire_spentness_hints(
    network: Network,
    mode: Mode,
    peers: zebra_network::SyncPeers,
    expected_output_count: u64,
    artifact_dir: PathBuf,
) -> Result<Option<VerifiedArtifact>, HintsError> {
    if mode == Mode::Off {
        return Ok(None);
    }

    let pin = MaxCheckpoint::for_network(&network).ok_or(HintsError::NotPinned)?;
    let hash = pin.object_hash();

    // The pinned artifact's exact length follows from the known-hash output
    // count, so no peer-claimed size is ever trusted. A count too large for
    // any artifact is a local or pin defect, not a peer's.
    let expected_len = artifact_len(expected_output_count).map_err(|error| {
        HintsError::Invalid(format!(
            "{expected_output_count} known-hash outputs do not fit an artifact: {error}"
        ))
    })?;

    let (bytes, source) = load_or_fetch(mode, hash, expected_len, peers, &artifact_dir).await?;

    // The bytes hash to the pin, so any failure from here on is a defect of
    // the pin, not of the peer that served them.
    let verified = tokio::task::spawn_blocking(move || {
        verify_pinned(&network, &pin, expected_output_count, bytes)
    })
    .await
    .expect("artifact verification does not panic")?;

    if source == Source::Fetched {
        save_artifact(&artifact_dir, hash, verified.bytes().clone()).await?;
    }

    info!(
        height = ?pin.height,
        %hash,
        output_count = verified.header().output_count,
        ?source,
        "acquired the pinned spentness-hint artifact",
    );

    Ok(Some(verified))
}

/// Registers the saved artifact for `get-object` serving: inserts its
/// `ObjectEntry` (file path and size) into the index `objects` publishes
/// (the sender of `zebra_network::Config::object_source`). Never registers
/// an artifact that does not match the current pin.
///
/// The current pin is `artifact.pin()`: a [`VerifiedArtifact`] exists only
/// once it has been authenticated against the network's `MaxCheckpoint`,
/// and it is served under that pin's hash. The file is written first if it
/// is missing or has the wrong size (blocking I/O), so a `get-object` never
/// finds a registered object it cannot read.
pub fn serve_spentness_hints(
    artifact: &VerifiedArtifact,
    artifact_dir: PathBuf,
    objects: &watch::Sender<Arc<ObjectIndex>>,
) -> Result<(), HintsError> {
    let hash = artifact.pin().object_hash();
    let entry = store_for_serving(&artifact_dir, hash, artifact.bytes())?;
    let size = entry.size;

    register_object(objects, hash, entry);

    info!(%hash, size, "serving the pinned spentness-hint artifact");

    Ok(())
}

/// Makes sure the file for `hash` holds `bytes`, and returns its index entry.
fn store_for_serving(
    artifact_dir: &Path,
    hash: ObjectHash,
    bytes: &[u8],
) -> Result<ObjectEntry, HintsError> {
    let path = artifact_path(artifact_dir, hash);
    // The cast is a lossless widening: the artifact length fits u64.
    let size = bytes.len() as u64;

    let on_disk = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.is_file() && metadata.len() == size,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    if !on_disk {
        write_artifact_file(artifact_dir, hash, bytes)?;
    }

    Ok(ObjectEntry { path, size })
}

/// Inserts `entry` under `hash` in the published object index, keeping every
/// other entry.
fn register_object(
    objects: &watch::Sender<Arc<ObjectIndex>>,
    hash: ObjectHash,
    entry: ObjectEntry,
) {
    objects.send_modify(|index| {
        let mut updated = ObjectIndex::clone(index);
        updated.insert(hash, entry);
        *index = Arc::new(updated);
    });
}

/// Parses and authenticates bytes that already hash to `pin`, and checks
/// their bit count against the known-hash `txouts` sum.
fn verify_pinned(
    network: &Network,
    pin: &MaxCheckpoint,
    expected_output_count: u64,
    bytes: Arc<[u8]>,
) -> Result<VerifiedArtifact, HintsError> {
    let verified = ParsedArtifact::parse(bytes)
        .and_then(|parsed| parsed.verify(network, pin))
        .map_err(|error| {
            HintsError::Invalid(format!("the pinned artifact is defective: {error}"))
        })?;

    check_output_count(verified.header().output_count, expected_output_count)?;

    Ok(verified)
}

/// Checks the artifact's bit count against the known-hash `txouts` sum for
/// `0..=H` (zips#1346 S:380-386).
fn check_output_count(bits: u64, outputs: u64) -> Result<(), HintsError> {
    if bits == outputs {
        Ok(())
    } else {
        Err(HintsError::OutputCount { bits, outputs })
    }
}

/// Where the artifact bytes came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Source {
    /// A file in the artifact directory.
    Local,
    /// A peer.
    Fetched,
}

/// Returns bytes that hash to `hash`: from the artifact directory if a file
/// there does, otherwise from the first candidate peer that serves the whole
/// object. The object is exactly `expected_len` bytes long.
///
/// Peers are asked only while the artifact is missing locally (zakura
/// `a573c5ef95`). Stale artifact files are deleted first.
async fn load_or_fetch<P: ArtifactPeer>(
    mode: Mode,
    hash: ObjectHash,
    expected_len: u64,
    peers: watch::Receiver<Arc<[P]>>,
    artifact_dir: &Path,
) -> Result<(Arc<[u8]>, Source), HintsError> {
    let dir = artifact_dir.to_owned();
    let local =
        tokio::task::spawn_blocking(move || load_saved_artifact(&dir, hash, Some(expected_len)))
            .await
            .expect("artifact loading does not panic")?;

    if let Some(bytes) = local {
        return Ok((bytes, Source::Local));
    }

    let bytes = fetch_from_peers(mode, hash, expected_len, peers).await?;
    Ok((bytes, Source::Fetched))
}

/// Fetches the object `hash` whole from one candidate peer at a time, under
/// `mode`'s retry policy.
///
/// With `Mode::Auto`, the whole acquisition, downloads included, ends at
/// [`AUTO_TIMEOUT`].
async fn fetch_from_peers<P: ArtifactPeer>(
    mode: Mode,
    hash: ObjectHash,
    expected_len: u64,
    peers: watch::Receiver<Arc<[P]>>,
) -> Result<Arc<[u8]>, HintsError> {
    let mut mismatches = 0_usize;

    if mode == Mode::Auto {
        let rounds = fetch_rounds(mode, hash, expected_len, peers, &mut mismatches);
        match tokio::time::timeout(AUTO_TIMEOUT, rounds).await {
            Ok(result) => result,
            Err(_elapsed) => Err(give_up(mismatches)),
        }
    } else {
        fetch_rounds(mode, hash, expected_len, peers, &mut mismatches).await
    }
}

/// Asks every candidate peer in turn, waiting between rounds, until one
/// serves the object or `Mode::Auto` runs out of rounds. Counts the peers
/// that served a mismatching object in `mismatches`.
///
/// Peers that failed in an earlier round are asked after the others, so a
/// peer that connected since is not kept waiting behind them.
async fn fetch_rounds<P: ArtifactPeer>(
    mode: Mode,
    hash: ObjectHash,
    expected_len: u64,
    mut peers: watch::Receiver<Arc<[P]>>,
    mismatches: &mut usize,
) -> Result<Arc<[u8]>, HintsError> {
    let mut failed_rounds = 0;
    let mut failed_peers = HashSet::new();

    loop {
        let mut candidates: Vec<P> = peers
            .borrow_and_update()
            .iter()
            .filter(|peer| peer.serves_artifacts())
            .cloned()
            .collect();

        // The registry lists peers in connection order. Start each round at
        // a different peer, so one slow or failing early peer is not asked
        // first every round.
        if !candidates.is_empty() {
            let start = failed_rounds % candidates.len();
            candidates.rotate_left(start);
        }
        // A stable sort, so the rotation still orders each group.
        candidates.sort_by_key(|peer| failed_peers.contains(&peer.label()));

        for peer in &candidates {
            match fetch_whole(peer, hash, expected_len).await {
                Ok(bytes) => return Ok(bytes),
                Err(PeerFailure::Mismatch) => *mismatches += 1,
                Err(failure) => {
                    debug!(peer = %peer.label(), %failure, "peer did not serve the artifact")
                }
            }
            failed_peers.insert(peer.label());
        }

        if candidates.is_empty() {
            // Wait for the first peers to connect. A closed registry never
            // sends new peers.
            if peers.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        } else {
            failed_rounds += 1;
            if mode == Mode::Auto && failed_rounds >= AUTO_ROUNDS {
                return Err(give_up(*mismatches));
            }

            tokio::time::sleep(RETRY_INTERVAL).await;
        }
    }
}

/// The error for an `Auto` acquisition that ran out of time or rounds.
fn give_up(mismatches: usize) -> HintsError {
    if mismatches > 0 {
        HintsError::Invalid(format!(
            "{mismatches} complete objects from peers did not hash to the pin"
        ))
    } else {
        HintsError::NotFound
    }
}

/// Why one peer did not deliver the artifact.
#[derive(Debug, thiserror::Error)]
enum PeerFailure {
    /// The peer does not have the object.
    #[error("the peer does not have the object")]
    NotFound,

    /// The request failed or timed out, or the peer stopped making progress.
    #[error("unavailable: {0}")]
    Unavailable(String),

    /// The peer claims a size other than the pinned artifact's exact length.
    /// Size disagreement is resolved by the hash check, not penalized: the
    /// peer is dropped before any of its data is kept.
    #[error("claimed object size {0} is not the pinned artifact's length")]
    Size(u64),

    /// The peer contradicted itself; its connection was closed.
    #[error("protocol violation: {0}")]
    Violation(String),

    /// The peer delivered a complete object that does not hash to the
    /// request, and was penalized.
    #[error("the complete object does not hash to the pin")]
    Mismatch,
}

/// Fetches the whole object `hash`, exactly `expected_len` bytes long, from
/// `peer`, and checks its hash.
///
/// The peer's `size` is unauthenticated: a first `size` other than
/// `expected_len` drops the peer before any data is kept, and a `size` that
/// changes or falls below the delivered bytes is a protocol violation
/// (zips#1344, "`get-object`"). The buffer is allocated once, at exactly
/// `expected_len`, after the first non-empty answer that claims that size,
/// and is never copied: a peer that claims the size but sends no data makes
/// the node allocate nothing.
///
/// The download, not the hash check, must finish within
/// [`whole_object_timeout`], so a slow peer is dropped however it paces its
/// answers. A peer that falls below [`MIN_RATE`] after [`RATE_GRACE`] is
/// dropped as soon as one of its answers shows it, before that deadline.
async fn fetch_whole<P: ArtifactPeer>(
    peer: &P,
    hash: ObjectHash,
    expected_len: u64,
) -> Result<Arc<[u8]>, PeerFailure> {
    let mut bytes: Option<Arc<[u8]>> = None;
    let mut filled: usize = 0;
    let mut claimed_size: Option<u64> = None;
    let mut requests: u64 = 0;
    let started = tokio::time::Instant::now();
    let deadline = started + whole_object_timeout(expected_len);

    loop {
        // The cast is a lossless widening: the buffer length fits u64.
        let offset = filled as u64;

        if let Some(size) = claimed_size {
            let needed = size.div_ceil(u64::from(PIECE_LENGTH));
            if requests >= needed.saturating_add(EXTRA_REQUESTS) {
                return Err(PeerFailure::Unavailable(format!(
                    "no progress: {offset} of {size} bytes after {requests} requests"
                )));
            }
        }
        requests += 1;

        let piece_deadline = deadline.min(tokio::time::Instant::now() + PIECE_TIMEOUT);
        let chunk =
            tokio::time::timeout_at(piece_deadline, peer.get_object(hash, offset, PIECE_LENGTH))
                .await
                .map_err(|_| {
                    PeerFailure::Unavailable(format!(
                        "get-object timed out after {offset} of {expected_len} bytes"
                    ))
                })?;

        let (size, data) = match chunk {
            Ok(ObjectChunk::Found { size, data }) => (size, data),
            Ok(ObjectChunk::NotFound) | Err(SyncPeerError::NotFound) if offset == 0 => {
                return Err(PeerFailure::NotFound)
            }
            Ok(ObjectChunk::NotFound) => {
                return Err(PeerFailure::Unavailable(format!(
                    "lost the object after {offset} bytes"
                )))
            }
            Err(error) => return Err(PeerFailure::Unavailable(error.to_string())),
        };

        if let Some(previous) = claimed_size {
            if size != previous {
                let reason = format!("get-object size changed from {previous} to {size}");
                peer.report_violation(reason.clone());
                return Err(PeerFailure::Violation(reason));
            }
        } else if size != expected_len {
            return Err(PeerFailure::Size(size));
        }
        claimed_size = Some(size);

        // The cast is a lossless widening: the data length fits u64.
        let end = offset.saturating_add(data.len() as u64);
        // The cast is a lossless widening: u32 fits usize on supported targets.
        if size < offset || end > size || data.len() > PIECE_LENGTH as usize {
            let reason = format!(
                "get-object answer of {} bytes at {offset} for an object of {size} bytes",
                data.len()
            );
            peer.report_violation(reason.clone());
            return Err(PeerFailure::Violation(reason));
        }

        // An empty answer is legal (a short answer cut off right after its size), but it must
        // not make the node allocate the whole artifact for no data.
        if !data.is_empty() {
            let buffer = bytes.get_or_insert_with(|| {
                #[cfg(test)]
                tests::BUFFER_ALLOCATIONS.with(|count| count.set(count.get() + 1));

                let len = usize::try_from(expected_len)
                    .expect("artifact_len bounds the length by MAX_ARTIFACT_LEN, which fits usize");
                // `RepeatN` has a trusted length, so this allocates the `Arc`
                // once, at its final size.
                std::iter::repeat_n(0, len).collect()
            });
            let buffer =
                Arc::get_mut(buffer).expect("the buffer is not shared until it is complete");
            // `end <= size == expected_len`, which fits usize (checked at allocation).
            let end_index = filled + data.len();
            buffer[filled..end_index].copy_from_slice(&data);
            filled = end_index;
        }

        if end == size {
            break;
        }

        if below_min_rate(end, started.elapsed()) {
            return Err(PeerFailure::Unavailable(format!(
                "below the minimum rate: {end} of {size} bytes after {:?}",
                started.elapsed()
            )));
        }
    }

    // Only a zero-length object completes without data, and its bytes are empty.
    let bytes = bytes.unwrap_or_else(|| Arc::from([]));
    let hashed = bytes.clone();
    let actual = tokio::task::spawn_blocking(move || ObjectHash::of(&hashed))
        .await
        .expect("hashing does not panic");

    if actual != hash {
        warn!(
            peer = %peer.label(),
            expected = %hash,
            %actual,
            "peer served a spentness-hint artifact that does not match the pin",
        );
        peer.penalize(
            MISMATCH_PENALTY,
            format!("get-object {hash}: the complete object hashes to {actual}"),
        )
        .await;
        return Err(PeerFailure::Mismatch);
    }

    Ok(bytes)
}

/// How long one peer may take to deliver the whole `expected_len`-byte
/// object: the time at [`MIN_RATE`], and at least [`PIECE_TIMEOUT`].
fn whole_object_timeout(expected_len: u64) -> Duration {
    PIECE_TIMEOUT.max(Duration::from_secs(expected_len.div_ceil(MIN_RATE)))
}

/// Whether `delivered` bytes after `elapsed` are below [`MIN_RATE`], once
/// [`RATE_GRACE`] has passed.
fn below_min_rate(delivered: u64, elapsed: Duration) -> bool {
    let Some(paced) = elapsed.checked_sub(RATE_GRACE) else {
        return false;
    };
    let floor = u128::from(MIN_RATE).saturating_mul(paced.as_millis()) / 1000;
    u128::from(delivered) < floor
}

/// A peer that may serve the artifact: [`SyncPeer`] in production, scripted
/// peers in tests.
trait ArtifactPeer: Clone + Send + Sync + 'static {
    /// Whether the peer advertises `NODE_SYNC_ARTIFACTS`.
    fn serves_artifacts(&self) -> bool;

    /// The peer, for logs.
    fn label(&self) -> String;

    /// One `get-object` request.
    fn get_object(
        &self,
        hash: ObjectHash,
        offset: u64,
        length: u32,
    ) -> impl Future<Output = Result<ObjectChunk, SyncPeerError>> + Send;

    /// Adds misbehavior points.
    fn penalize(&self, points: u32, reason: String) -> impl Future<Output = ()> + Send;

    /// Closes the connection for a violation the stream reader cannot see.
    fn report_violation(&self, reason: String);
}

impl ArtifactPeer for SyncPeer {
    fn serves_artifacts(&self) -> bool {
        self.services().contains(PeerServices::NODE_SYNC_ARTIFACTS)
    }

    fn label(&self) -> String {
        self.addr().to_string()
    }

    fn get_object(
        &self,
        hash: ObjectHash,
        offset: u64,
        length: u32,
    ) -> impl Future<Output = Result<ObjectChunk, SyncPeerError>> + Send {
        SyncPeer::get_object(self, hash, offset, length)
    }

    fn penalize(&self, points: u32, reason: String) -> impl Future<Output = ()> + Send {
        SyncPeer::penalize(self, points, reason)
    }

    fn report_violation(&self, reason: String) {
        SyncPeer::report_violation(self, reason)
    }
}

/// The file that holds the object `hash`: its lowercase hex hash, as
/// `get-object` serving expects.
fn artifact_path(artifact_dir: &Path, hash: ObjectHash) -> PathBuf {
    artifact_dir.join(hash.to_string())
}

/// Loads the artifact file for `hash` that an earlier run saved in
/// `artifact_dir`, if there is one. Blocking.
///
/// First deletes the stale files of other pins, and partial files
/// ([`remove_stale_files`]), so a revoked artifact is never loaded or
/// served. Then returns the bytes of the file for `hash` if they hash to
/// `hash`. With `expected_len`, a file of another length is deleted without
/// being read; a file with other contents is deleted too.
pub fn load_saved_artifact(
    artifact_dir: &Path,
    hash: ObjectHash,
    expected_len: Option<u64>,
) -> Result<Option<Arc<[u8]>>, HintsError> {
    remove_stale_files(artifact_dir, hash)?;

    let path = artifact_path(artifact_dir, hash);

    let length = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };

    if expected_len.is_none_or(|expected_len| length == expected_len) {
        let bytes: Arc<[u8]> = std::fs::read(&path)?.into();
        if ObjectHash::of(&bytes) == hash {
            return Ok(Some(bytes));
        }
    }

    warn!(
        ?path,
        "deleting a spentness-hint artifact file that does not match the pin"
    );
    std::fs::remove_file(&path)?;

    Ok(None)
}

/// Deletes the artifact files and partial files in `artifact_dir` that are
/// not the complete file for `current`: artifacts of earlier, possibly
/// revoked, pins (zakura `116239f34e`). Only files with the names this
/// module writes are touched.
fn remove_stale_files(artifact_dir: &Path, current: ObjectHash) -> Result<(), HintsError> {
    let entries = match std::fs::read_dir(artifact_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };

    let current = current.to_string();

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };

        let object = name.strip_suffix(TEMP_SUFFIX).unwrap_or(name);
        let is_ours = object.len() == 64 && object.parse::<ObjectHash>().is_ok();

        if is_ours && name != current && entry.file_type()?.is_file() {
            debug!(file = name, "deleting a stale spentness-hint artifact file");
            std::fs::remove_file(entry.path())?;
        }
    }

    Ok(())
}

/// Saves `bytes` as the file for `hash`.
async fn save_artifact(
    artifact_dir: &Path,
    hash: ObjectHash,
    bytes: Arc<[u8]>,
) -> Result<(), HintsError> {
    let dir = artifact_dir.to_owned();
    tokio::task::spawn_blocking(move || write_artifact_file(&dir, hash, &bytes))
        .await
        .expect("artifact saving does not panic")
}

/// Writes `bytes` as the file for `hash`, through a temporary file and a
/// rename, so the file name only ever holds a complete object.
fn write_artifact_file(
    artifact_dir: &Path,
    hash: ObjectHash,
    bytes: &[u8],
) -> Result<(), HintsError> {
    std::fs::create_dir_all(artifact_dir)?;

    let path = artifact_path(artifact_dir, hash);
    let temp = artifact_dir.join(format!("{hash}{TEMP_SUFFIX}"));

    std::fs::write(&temp, bytes)?;
    std::fs::rename(&temp, &path)?;

    Ok(())
}
