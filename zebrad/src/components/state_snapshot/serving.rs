//! Serving state snapshots to version 2 peers (design, §8).
//!
//! With `[sync] serve_state_snapshots` on, the objects in the artifact store
//! are published as the node's object index, which the network serves through
//! `get-object` requests. Each object is re-hashed against its file name in
//! the background before it is published, and rejected manifests and the
//! pieces of unverified manifests are left out.

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, Read},
    path::Path,
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;

use zebra_chain::{parameters::Network, state_snapshot::ObjectHash};
use zebra_network::{ObjectEntry, ObjectIndex, ObjectSource};
use zebra_state::state_snapshot::{
    format::{Manifest, MAX_MANIFEST_BYTES},
    store::ArtifactStore,
};

#[cfg(test)]
mod tests;

/// How many bytes of an object are hashed at a time, so a 16 MiB piece is
/// never read into memory whole.
const HASH_CHUNK_BYTES: usize = 1 << 20;

/// What a scan of the artifact store found.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanSummary {
    /// The objects published in the index.
    pub published: usize,

    /// The verified manifests whose every piece is present and verified.
    pub complete_manifests: usize,

    /// The verified manifests with at least one piece missing.
    pub incomplete_manifests: usize,

    /// The files left out: objects that do not hash to their names, rejected
    /// manifests, and objects no verified manifest lists.
    pub excluded: usize,
}

impl ScanSummary {
    /// Returns whether the index holds a manifest with every piece, so peers
    /// can download a whole snapshot from this node.
    pub fn serves_complete_manifest(&self) -> bool {
        self.complete_manifests > 0
    }
}

/// This node's object index, as the network reads it, and the scan that
/// fills it.
#[derive(Debug)]
pub struct ServingIndex {
    /// The source the network serves from. It holds an empty index until the
    /// first scan of the store finishes.
    pub source: ObjectSource,

    /// `Some` once the first scan has published its index.
    summary: watch::Receiver<Option<ScanSummary>>,
}

impl ServingIndex {
    /// Waits up to `timeout` for the first scan to publish its index, and
    /// returns what it found, or `None` if the scan did not finish in time.
    pub async fn first_scan(&mut self, timeout: Duration) -> Option<ScanSummary> {
        let wait = async {
            loop {
                if let Some(summary) = *self.summary.borrow_and_update() {
                    return Some(summary);
                }
                if self.summary.changed().await.is_err() {
                    return None;
                }
            }
        };

        tokio::time::timeout(timeout, wait).await.ok().flatten()
    }
}

/// Adds the verified objects in `store` for `network` to the object index that
/// `index_sender` publishes, and returns the source the network reads it through.
///
/// The store is scanned on a blocking thread: every object is re-hashed
/// against its file name, manifests are decoded for `network`, and the objects
/// are added once, when the scan finishes. The index keeps the objects it
/// already holds, such as the spentness-hint artifact.
pub fn object_index(
    store: ArtifactStore,
    network: Network,
    index_sender: watch::Sender<Arc<ObjectIndex>>,
) -> ServingIndex {
    let source = ObjectSource(index_sender.subscribe());
    let (summary_sender, summary) = watch::channel(None);

    tokio::task::spawn_blocking(move || {
        let (index, scan) = scan_store(&store, &network);
        info!(
            store = %store.root().display(),
            published = scan.published,
            complete_manifests = scan.complete_manifests,
            incomplete_manifests = scan.incomplete_manifests,
            excluded = scan.excluded,
            "scanned the state snapshot store"
        );
        index_sender.send_modify(|published| {
            let mut updated = ObjectIndex::clone(published);
            updated.extend(index);
            *published = Arc::new(updated);
        });
        let _ = summary_sender.send(Some(scan));
    });

    ServingIndex { source, summary }
}

/// Scans `store` and returns the objects to serve: every verified manifest
/// that is not marked as rejected, and every verified piece such a manifest
/// lists with the same size (design, §8).
///
/// Blocks on the filesystem: every object is read once to re-hash it.
pub(crate) fn scan_store(store: &ArtifactStore, network: &Network) -> (ObjectIndex, ScanSummary) {
    let mut summary = ScanSummary::default();

    let entries = match fs::read_dir(store.objects_dir()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            debug!(store = %store.root().display(), "the state snapshot store has no objects");
            return (ObjectIndex::new(), summary);
        }
        Err(error) => {
            warn!(?error, store = %store.root().display(), "could not read the state snapshot store");
            return (ObjectIndex::new(), summary);
        }
    };

    // Re-hash every object against its file name before trusting it. The store is on this node's
    // disk, so the number of files is bounded by what the operator put there.
    let mut verified: HashMap<ObjectHash, ObjectEntry> = HashMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(hash) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<ObjectHash>().ok())
        else {
            debug!(path = %path.display(), "skipping a store file that is not named by an object hash");
            summary.excluded += 1;
            continue;
        };

        match verify_object(&path, hash) {
            Ok(size) => {
                verified.insert(hash, ObjectEntry { path, size });
            }
            Err(error) => {
                warn!(
                    %error,
                    %hash,
                    path = %path.display(),
                    "not serving a state snapshot object that does not hash to its file name"
                );
                summary.excluded += 1;
            }
        }
    }

    // A manifest is at most `MAX_MANIFEST_BYTES` and decodes for this network.
    let mut manifests: Vec<(ObjectHash, Manifest)> = Vec::new();
    for (hash, entry) in &verified {
        // A usize always fits in a u64 on the targets Zebra supports.
        if entry.size > MAX_MANIFEST_BYTES as u64 {
            continue;
        }
        let Ok(bytes) = fs::read(&entry.path) else {
            continue;
        };
        if let Ok(manifest) = Manifest::decode(&bytes, network) {
            manifests.push((*hash, manifest));
        }
    }
    manifests.sort_by_key(|(hash, _)| *hash);

    let mut index = ObjectIndex::new();
    let mut listed_pieces: HashMap<ObjectHash, u32> = HashMap::new();
    let mut rejected: HashSet<ObjectHash> = HashSet::new();
    for (hash, manifest) in &manifests {
        if store.is_rejected(*hash) {
            info!(manifest = %hash, "not serving a rejected state snapshot manifest");
            summary.excluded += 1;
            rejected.insert(*hash);
            continue;
        }

        let mut complete = true;
        for (_, _, piece) in manifest.pieces() {
            listed_pieces.insert(piece.hash, piece.size);
            complete &= verified
                .get(&piece.hash)
                .is_some_and(|entry| entry.size == u64::from(piece.size));
        }
        if complete {
            summary.complete_manifests += 1;
        } else {
            summary.incomplete_manifests += 1;
        }

        let entry = verified
            .get(hash)
            .expect("decoded manifests come from the verified objects")
            .clone();
        index.insert(*hash, entry);
    }

    for (hash, entry) in verified {
        if index.contains_key(&hash) || rejected.contains(&hash) {
            continue;
        }
        match listed_pieces.get(&hash) {
            Some(size) if u64::from(*size) == entry.size => {
                index.insert(hash, entry);
            }
            _ => {
                debug!(
                    %hash,
                    "not serving a state snapshot object that no verified manifest lists"
                );
                summary.excluded += 1;
            }
        }
    }

    summary.published = index.len();
    (index, summary)
}

/// Why an object in the store is not served.
#[derive(Debug, Error)]
enum VerifyError {
    /// The file could not be read.
    #[error("could not read the object: {0}")]
    Io(#[from] io::Error),

    /// The file's contents hash to `actual`, not to its name.
    #[error("the object hashes to {actual}, not to its file name")]
    HashMismatch {
        /// The hash of the file's contents.
        actual: ObjectHash,
    },
}

/// Re-hashes the object at `path` in chunks and returns its size if it hashes
/// to `expected`.
fn verify_object(path: &Path, expected: ObjectHash) -> Result<u64, VerifyError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; HASH_CHUNK_BYTES];
    let mut size: u64 = 0;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        // A usize always fits in a u64 on the targets Zebra supports.
        size = size.saturating_add(read as u64);
    }

    let actual = ObjectHash(hasher.finalize().into());
    if actual == expected {
        Ok(size)
    } else {
        Err(VerifyError::HashMismatch { actual })
    }
}
