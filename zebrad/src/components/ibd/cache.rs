//! The disk overflow tier of the known-hash IBD download window.
//!
//! [`BlockCache`] is a block store under the state cache directory
//! (`<cache_dir>/ibd-block-cache/`, one file per block). The engine writes
//! blocks here **raw** when they arrive beyond the memory byte budget, and
//! promotes the lowest cached heights back through the normal
//! verify-and-commit path as commits free memory. This keeps the network
//! saturated while the state service works through the commit pipeline, and
//! upholds the §4.5 invariant: each block is downloaded at most once.
//!
//! Cached bytes are **untrusted input**: nothing beyond the connection
//! handler's header-hash match was checked before the write, and the file
//! may be corrupted or torn while on disk (there is deliberately no per-block
//! fsync). Every entry read back via [`BlockCache::get`] must go through the
//! full verify path (hash vs the pinned list, merkle, linkage) exactly like a
//! network response; [`verify_entry`] is only a cheap header-level filter.
//!
//! # On-disk format
//!
//! Each entry is one file named `<height>-<hash_hex>.bin`, where `<height>`
//! is the decimal block height and `<hash_hex>` is the 64-character
//! lowercase hex block hash in display order ([`block::Hash`]'s `Display`).
//! The file contains a single sidecar line followed by the raw block:
//!
//! ```text
//! zebra-ibd-cache-v1 <source-socket-addr or "-">\n
//! <canonical zcash-serialized block bytes>
//! ```
//!
//! The sidecar records the address of the peer that delivered the block, so
//! a corrupt body discovered at promotion (or a bad auth-data commitment
//! discovered at commit) can still be attributed to its source for
//! misbehavior reporting. The real (unredacted) address is written: the file
//! lives in the node's own state cache directory, never in logs or metrics,
//! and is deleted when the block commits.
//!
//! Torn or corrupted entries never panic and are never trusted:
//! - a file whose name or sidecar line cannot be parsed is dropped (deleted)
//!   by [`BlockCache::scan`] or [`BlockCache::get`], and the height is
//!   refetched — §4.5 exception (a);
//! - a truncated or corrupted *body* is returned as-is by
//!   [`BlockCache::get`] and caught downstream by re-verification.
//!
//! # Blocking I/O
//!
//! Internals are synchronous `std::fs` operations behind a mutex; the public
//! async methods wrap them in [`tokio::task::spawn_blocking`], so they are
//! safe to call from async tasks (the engine never does file I/O inline on
//! its loop). Each operation touches one file, except the batched
//! [`BlockCache::evict_through`], [`BlockCache::scan`], and
//! [`BlockCache::remove_all`].

use std::{
    collections::{btree_map, BTreeMap},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use zebra_chain::{
    block::{self, Block, MAX_BLOCK_BYTES},
    serialization::{ZcashDeserialize, ZcashSerialize},
};
use zebra_network::PeerSocketAddr;

/// The name of the cache directory under the state cache directory.
///
/// Startup wiring (TODO(known-hash-ibd D6)) joins this onto the configured
/// state `cache_dir` and passes the result to [`BlockCache::new`].
pub const CACHE_DIR_NAME: &str = "ibd-block-cache";

/// The magic tag starting every entry's sidecar line.
///
/// Identifies the file as a cache entry and versions the format; entries
/// with an unknown tag are treated as corrupt and deleted.
const SIDECAR_MAGIC: &str = "zebra-ibd-cache-v1";

/// The longest possible sidecar line, including the trailing newline.
///
/// The magic tag plus a space plus a socket address (at most 58 bytes for an
/// IPv6 address with a scope ID) is far below this bound, so the newline
/// search over a file's head is bounded even on garbage files.
const MAX_SIDECAR_BYTES: usize = 128;

/// The file extension of cache entries.
const FILE_EXTENSION: &str = "bin";

/// A cached block read back from the disk tier.
///
/// The bytes are **untrusted**: the caller must re-verify them through the
/// full verify-and-commit path against `expected_hash` before use.
#[derive(Clone, Debug)]
pub struct CachedBlock {
    /// The raw block bytes as fetched from the network — possibly corrupted
    /// or truncated while on disk.
    pub bytes: Vec<u8>,

    /// The pinned hash this entry was stored under, recovered from the file
    /// name. Re-verification must check the block against this hash.
    pub expected_hash: block::Hash,

    /// The peer that delivered the block, when known, for misbehavior
    /// attribution if re-verification fails.
    pub source: Option<PeerSocketAddr>,
}

/// The in-memory index record for one cached block.
#[derive(Copy, Clone, Debug)]
struct Entry {
    /// The pinned hash from the entry's file name.
    hash: block::Hash,

    /// The size of the raw block bytes (the file minus its sidecar line).
    block_bytes: u32,
}

/// The disk overflow tier: a block store with one file per block.
///
/// Cheaply cloneable; clones share the same directory and index. See the
/// [module docs](self) for the on-disk format and trust model.
#[derive(Clone, Debug)]
pub struct BlockCache {
    /// The shared directory, index, and byte accounting.
    inner: Arc<Mutex<Inner>>,
}

/// The state shared by all clones of a [`BlockCache`].
#[derive(Debug)]
struct Inner {
    /// The cache directory, created lazily on first write or scan.
    dir: PathBuf,

    /// Whether `dir` has been created by this instance.
    dir_created: bool,

    /// The height-ordered index of entries believed to be on disk.
    entries: BTreeMap<block::Height, Entry>,

    /// The total raw block bytes of all indexed entries.
    ///
    /// Excludes the per-file sidecar overhead (under 100 bytes per entry).
    bytes: u64,
}

impl BlockCache {
    /// Returns a new cache over `dir` (conventionally
    /// `<state cache_dir>/`[`CACHE_DIR_NAME`]).
    ///
    /// Does no I/O: the directory is created on first write, and entries
    /// left by a previous run are only discovered by [`scan`](Self::scan).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.into(),
                dir_created: false,
                entries: BTreeMap::new(),
                bytes: 0,
            })),
        }
    }

    /// Serializes `block` and writes it as the cache entry for `height`,
    /// recording `source` for later misbehavior attribution.
    ///
    /// Replaces any previous entry for `height` (a refetched copy after a
    /// failed promotion). There is no per-block fsync: torn writes are
    /// caught by re-verification at promotion.
    ///
    /// Returns the raw block byte size, for the engine's `Slot::Cached`
    /// accounting.
    pub async fn put(
        &self,
        height: block::Height,
        block: &Arc<Block>,
        source: Option<PeerSocketAddr>,
    ) -> io::Result<u32> {
        let cache = self.clone();
        let block = Arc::clone(block);

        tokio::task::spawn_blocking(move || cache.lock().put(height, &block, source))
            .await
            .expect("cache I/O closures only do std I/O and index bookkeeping, and do not panic")
    }

    /// Reads the cache entry for `height` back for promotion.
    ///
    /// The returned bytes are **untrusted**: the caller must run them
    /// through the full verify path against
    /// [`expected_hash`](CachedBlock::expected_hash).
    ///
    /// Returns `None` when `height` is not cached, or when the entry's file
    /// or sidecar is unreadable — in that case the entry is dropped (and its
    /// file deleted), and the caller must refetch the height from the
    /// network (§4.5 exception (a)). A readable entry with a corrupt *body*
    /// is still returned: only re-verification can detect it.
    pub async fn get(&self, height: block::Height) -> Option<CachedBlock> {
        let cache = self.clone();

        tokio::task::spawn_blocking(move || cache.lock().get(height))
            .await
            .expect("cache I/O closures only do std I/O and index bookkeeping, and do not panic")
    }

    /// Deletes every entry at or below `height`, which has been committed to
    /// the state.
    ///
    /// Eviction is batched: the engine calls this lazily as its commit
    /// frontier advances, not once per committed block. Files that fail to
    /// delete are logged and forgotten; they are re-pruned by the next
    /// [`scan`](Self::scan).
    pub async fn evict_through(&self, height: block::Height) {
        let cache = self.clone();

        tokio::task::spawn_blocking(move || cache.lock().evict_through(height))
            .await
            .expect("cache I/O closures only do std I/O and index bookkeeping, and do not panic")
    }

    /// Scans the cache directory after a (re)start, rebuilding the index
    /// from entries left by a previous run.
    ///
    /// Keeps only heights the engine still needs — within
    /// `(state_tip, list_max]` — and deletes everything else: stale entries
    /// at or below the committed tip, entries beyond the pinned list, and
    /// files whose name or sidecar cannot be parsed. Survivors' bodies are
    /// *not* verified here; they are re-verified at promotion like any other
    /// cached bytes.
    ///
    /// Returns the surviving `(height, raw block bytes)` pairs in ascending
    /// height order, for the engine's `Slot::Cached` accounting.
    pub async fn scan(
        &self,
        state_tip: Option<block::Height>,
        list_max: block::Height,
    ) -> io::Result<Vec<(block::Height, u32)>> {
        let cache = self.clone();

        tokio::task::spawn_blocking(move || cache.lock().scan(state_tip, list_max))
            .await
            .expect("cache I/O closures only do std I/O and index bookkeeping, and do not panic")
    }

    /// Deletes the entire cache directory, for the `Completed` handoff to
    /// the legacy syncer.
    ///
    /// The cache remains usable afterwards: the directory is recreated on
    /// the next write.
    pub async fn remove_all(&self) -> io::Result<()> {
        let cache = self.clone();

        tokio::task::spawn_blocking(move || cache.lock().remove_all())
            .await
            .expect("cache I/O closures only do std I/O and index bookkeeping, and do not panic")
    }

    /// Returns the total raw block bytes of all indexed entries.
    pub fn bytes(&self) -> u64 {
        self.lock().bytes
    }

    /// Returns the number of indexed entries.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    /// Returns whether the cache has no indexed entries.
    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }

    /// Locks the shared state.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned lock means a previous operation panicked mid-update.
        // Recovering it is safe: the index is advisory (rebuildable from
        // disk by `scan`), and every cached byte is re-verified at
        // promotion regardless of index state.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inner {
    /// Synchronous [`BlockCache::put`]: see its docs.
    //
    // The `expect` checks an internal invariant (network blocks are bounded
    // far below `u32::MAX`); the `Result` is only for real file I/O errors.
    #[allow(clippy::unwrap_in_result)]
    fn put(
        &mut self,
        height: block::Height,
        block: &Block,
        source: Option<PeerSocketAddr>,
    ) -> io::Result<u32> {
        let raw = block.zcash_serialize_to_vec()?;
        let block_bytes = u32::try_from(raw.len()).expect(
            "blocks fetched from the network are bounded by MAX_BLOCK_BYTES, far below u32::MAX",
        );
        let hash = block.hash();

        // The real (unredacted) address is stored for misbehavior
        // attribution: this is a local file under the state cache dir, not a
        // log or metric (see the module docs).
        let source = source.map_or_else(
            || "-".to_string(),
            |addr| addr.remove_socket_addr_privacy().to_string(),
        );

        let mut file = format!("{SIDECAR_MAGIC} {source}\n").into_bytes();
        file.extend_from_slice(&raw);
        drop(raw);

        self.ensure_dir()?;
        fs::write(self.entry_path(height, &hash), &file)?;

        if let Some(old) = self.entries.insert(height, Entry { hash, block_bytes }) {
            self.bytes = self.bytes.saturating_sub(u64::from(old.block_bytes));

            // A different hash means a different file name, so the replaced
            // copy must be deleted explicitly.
            if old.hash != hash {
                remove_entry_file(&self.dir, height, &old.hash, "replaced by a refetched copy");
            }
        }
        self.bytes = self.bytes.saturating_add(u64::from(block_bytes));

        metrics::counter!("ibd.cache.written.count").increment(1);
        self.update_bytes_metric();

        Ok(block_bytes)
    }

    /// Synchronous [`BlockCache::get`]: see its docs.
    fn get(&mut self, height: block::Height) -> Option<CachedBlock> {
        let entry = self.entries.get(&height).copied()?;
        let path = self.entry_path(height, &entry.hash);

        let mut file = match fs::read(&path) {
            Ok(file) => file,
            Err(error) => {
                warn!(
                    height = height.0,
                    %error,
                    "cached block file unreadable; dropping the entry for refetch",
                );
                self.forget(height, &entry, false);
                return None;
            }
        };

        let Some((source, body_offset)) = parse_sidecar(&file) else {
            warn!(
                height = height.0,
                "cached block sidecar corrupt; deleting the entry for refetch",
            );
            self.forget(height, &entry, true);
            return None;
        };

        // Keep only the body. The body itself may still be torn or
        // corrupted: that is for the caller's re-verification to catch.
        file.drain(..body_offset);

        metrics::counter!("ibd.cache.promoted.count").increment(1);

        Some(CachedBlock {
            bytes: file,
            expected_hash: entry.hash,
            source,
        })
    }

    /// Synchronous [`BlockCache::evict_through`]: see its docs.
    fn evict_through(&mut self, height: block::Height) {
        let survivors = self
            .entries
            .split_off(&block::Height(height.0.saturating_add(1)));
        let evicted = std::mem::replace(&mut self.entries, survivors);

        for (height, entry) in evicted {
            self.bytes = self.bytes.saturating_sub(u64::from(entry.block_bytes));
            remove_entry_file(&self.dir, height, &entry.hash, "committed");
        }

        self.update_bytes_metric();
    }

    /// Synchronous [`BlockCache::scan`]: see its docs.
    fn scan(
        &mut self,
        state_tip: Option<block::Height>,
        list_max: block::Height,
    ) -> io::Result<Vec<(block::Height, u32)>> {
        self.entries.clear();
        self.bytes = 0;
        self.ensure_dir()?;

        // The lowest height the engine still needs: the block above the tip.
        let min_keep = state_tip.map_or(0, |tip| tip.0.saturating_add(1));

        for dir_entry in fs::read_dir(&self.dir)? {
            let dir_entry = dir_entry?;
            let path = dir_entry.path();

            let metadata = match dir_entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    warn!(?path, %error, "skipping unreadable block cache entry");
                    continue;
                }
            };

            // The cache only ever creates regular files; leave anything
            // else alone rather than deleting recursively.
            if !metadata.is_file() {
                warn!(?path, "skipping non-file in the block cache directory");
                continue;
            }

            let Some((height, hash)) = dir_entry
                .file_name()
                .to_str()
                .and_then(parse_entry_file_name)
            else {
                warn!(
                    ?path,
                    "deleting unrecognized file in the block cache directory"
                );
                remove_file_logged(&path);
                continue;
            };

            if height.0 < min_keep || height > list_max {
                debug!(
                    height = height.0,
                    "pruning cached block outside the needed range",
                );
                remove_file_logged(&path);
                continue;
            }

            // Validate the sidecar with a bounded head read; this also
            // yields the exact body size without reading the body.
            let mut head = Vec::with_capacity(MAX_SIDECAR_BYTES);
            let sidecar = fs::File::open(&path)
                .and_then(|file| {
                    file.take(MAX_SIDECAR_BYTES as u64).read_to_end(&mut head)?;
                    Ok(())
                })
                .ok()
                .and_then(|()| parse_sidecar(&head));

            let Some((_source, body_offset)) = sidecar else {
                warn!(?path, "deleting cached block with a corrupt sidecar");
                remove_file_logged(&path);
                continue;
            };

            let body_len = metadata.len().saturating_sub(body_offset as u64);
            if body_len == 0 || body_len > MAX_BLOCK_BYTES {
                warn!(
                    ?path,
                    body_len, "deleting cached block with an implausible body size"
                );
                remove_file_logged(&path);
                continue;
            }
            // `body_len` was just bounded by `MAX_BLOCK_BYTES` (2 MB), which
            // fits in `u32`.
            let block_bytes = body_len as u32;

            match self.entries.entry(height) {
                // Two files for one height (a crash between the steps of a
                // replacing `put`): both copies must pass the same
                // re-verification at promotion, so keep the one scanned
                // first and delete the duplicate.
                btree_map::Entry::Occupied(_) => {
                    warn!(?path, "deleting duplicate cached block for the same height");
                    remove_file_logged(&path);
                }
                btree_map::Entry::Vacant(slot) => {
                    slot.insert(Entry { hash, block_bytes });
                    self.bytes = self.bytes.saturating_add(u64::from(block_bytes));
                }
            }
        }

        // Survivor counts are far below 2^53, so the u64 conversion from
        // usize is exact.
        metrics::counter!("ibd.cache.restored.count").increment(self.entries.len() as u64);
        self.update_bytes_metric();

        Ok(self
            .entries
            .iter()
            .map(|(height, entry)| (*height, entry.block_bytes))
            .collect())
    }

    /// Synchronous [`BlockCache::remove_all`]: see its docs.
    fn remove_all(&mut self) -> io::Result<()> {
        self.entries.clear();
        self.bytes = 0;
        self.dir_created = false;
        self.update_bytes_metric();

        match fs::remove_dir_all(&self.dir) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Drops the index entry for `height`, optionally deleting its file.
    fn forget(&mut self, height: block::Height, entry: &Entry, delete_file: bool) {
        self.entries.remove(&height);
        self.bytes = self.bytes.saturating_sub(u64::from(entry.block_bytes));

        if delete_file {
            remove_entry_file(&self.dir, height, &entry.hash, "corrupt");
        }

        self.update_bytes_metric();
    }

    /// Returns the path of the entry file for `height` and `hash`.
    fn entry_path(&self, height: block::Height, hash: &block::Hash) -> PathBuf {
        self.dir.join(entry_file_name(height, hash))
    }

    /// Creates the cache directory if this instance has not done so yet.
    fn ensure_dir(&mut self) -> io::Result<()> {
        if !self.dir_created {
            fs::create_dir_all(&self.dir)?;
            self.dir_created = true;
        }

        Ok(())
    }

    /// Publishes the byte accounting metric.
    fn update_bytes_metric(&self) {
        // Cache sizes are far below 2^53 bytes, so the f64 conversion is
        // exact.
        metrics::gauge!("ibd.cache.bytes").set(self.bytes as f64);
    }
}

/// A cheap header-level integrity check on a cached entry's bytes.
///
/// Returns whether `bytes` starts with a parseable block header whose hash
/// is `expected_hash`. This catches header corruption and any truncation
/// shorter than the header, but **not** body corruption — only the full
/// verify path (merkle root and linkage checks at promotion) confirms the
/// body, so passing this check never makes the bytes trusted.
pub fn verify_entry(bytes: &[u8], expected_hash: block::Hash) -> bool {
    let mut reader = bytes;

    matches!(
        block::Header::zcash_deserialize(&mut reader),
        Ok(header) if header.hash() == expected_hash
    )
}

/// Returns the entry file name for `height` and `hash`:
/// `<height>-<hash_hex>.bin`.
fn entry_file_name(height: block::Height, hash: &block::Hash) -> String {
    format!("{}-{hash}.{FILE_EXTENSION}", height.0)
}

/// Parses an entry file name (`<height>-<hash_hex>.bin`) into its height and
/// pinned hash.
fn parse_entry_file_name(name: &str) -> Option<(block::Height, block::Hash)> {
    let stem = name.strip_suffix(&format!(".{FILE_EXTENSION}"))?;
    let (height, hash) = stem.split_once('-')?;

    Some((block::Height(height.parse().ok()?), hash.parse().ok()?))
}

/// Parses the sidecar line at the start of an entry file.
///
/// Returns the recorded source address and the offset where the raw block
/// bytes begin, or `None` if the sidecar is torn, garbled, or from an
/// unknown format version.
fn parse_sidecar(file: &[u8]) -> Option<(Option<PeerSocketAddr>, usize)> {
    let head = &file[..file.len().min(MAX_SIDECAR_BYTES)];
    let newline = head.iter().position(|&byte| byte == b'\n')?;

    let line = std::str::from_utf8(&head[..newline]).ok()?;
    let source = line.strip_prefix(SIDECAR_MAGIC)?.strip_prefix(' ')?;

    let source = match source {
        "-" => None,
        addr => Some(addr.parse().ok()?),
    };

    Some((source, newline + 1))
}

/// Deletes the entry file for `height` and `hash` under `dir`, logging
/// failures.
fn remove_entry_file(dir: &Path, height: block::Height, hash: &block::Hash, why: &str) {
    let path = dir.join(entry_file_name(height, hash));
    debug!(?path, why, "deleting cached block file");
    remove_file_logged(&path);
}

/// Deletes `path`, logging any error except the file already being gone.
///
/// Deletion failures are not fatal: a leftover file is re-pruned by the next
/// [`BlockCache::scan`].
fn remove_file_logged(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => warn!(?path, %error, "failed to delete a block cache file"),
    }
}
