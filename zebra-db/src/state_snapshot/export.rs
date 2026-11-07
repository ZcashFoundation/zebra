//! Exporting a state snapshot from a finalized database (design,
//! "State changes").
//!
//! The exporter opens the state read-only, refuses unless its finalized tip
//! is the snapshot height, has the block body at that height and the block
//! hashes from the window start, and has no spending index, then writes every component's pieces and the manifest to
//! an [`ArtifactStore`], canonicalizing the values the format requires.
//!
//! # Rows
//!
//! With the snapshot at `H` and the window at `L..=H`, `L = max(1, H - 2303)`
//! (design, "Snapshot contents"):
//!
//! - the hashes and headers of `{0} ∪ L..=H`, but the bodies of genesis and
//!   `H` only: an importing node keeps the bodies it commits above `H`, so it
//!   downloads no window body it would not keep. The location rows of their
//!   transactions are shipped;
//! - `block_info` also ships `L - 1`, the parent of the window's first block,
//!   but no hash or height row for it;
//! - for the other blocks, the transaction locations of only the transactions
//!   that still have an unspent output at `H`, which the UTXO set needs;
//! - the note commitment tree column families ship genesis, every subtree's
//!   end height, the last row below the window, and the window, so the subtree
//!   format checks pass and every window height reads its own tree;
//! - the UTXO set, the nullifier and anchor sets, the subtrees, and the tip
//!   singletons are shipped whole.
//!
//! The indexes an importer rebuilds are never shipped: the transaction hashes
//! by location (`hash_by_tx_loc`), which is the inverse of the shipped
//! locations, and the address balance, UTXO and transaction indexes, which
//! follow from the UTXO set, except for each address's received total and
//! history, which a pruned node cannot know. The spending index
//! (`tx_loc_by_spent_out_loc`) is never shipped either.
//!
//! # Reading
//!
//! The state is opened as a RocksDB secondary instance, which sees the
//! database as of its last catch-up with the primary, so every component reads
//! the same state. Each component is read on its own thread with raw
//! iterators that skip the block cache and read ahead, and the transaction
//! locations of the blocks without bodies are selected with a binary search
//! of the UTXO set's transaction locations.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, ScopedJoinHandle},
};

use rocksdb::ReadOptions;
use semver::Version;

use zebra_chain::{
    block::{self, Height},
    parameters::{Network, NetworkUpgrade},
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

use crate::{
    constants::{state_database_format_version_in_code, STATE_DATABASE_KIND},
    service::finalized_state::{
        can_read_without_migration, FromDisk, IntoDisk, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    Config,
};

use super::{
    format::{
        canonical_value, window_start, ComponentId, ComponentManifest, FormatError, Manifest,
        PieceManifest, PieceWriter, INDEX_MODE_DEFAULT,
    },
    store::ArtifactStore,
    SnapshotError,
};

/// How far ahead the column family scans read.
const READAHEAD_BYTES: usize = 4 << 20;

/// The length of an on-disk block height.
const HEIGHT_LEN: usize = 3;

/// The length of an on-disk transaction location: a height and an index.
const TX_LOC_LEN: usize = HEIGHT_LEN + 2;

/// The length of an on-disk output location: a transaction location and an
/// output index. Address locations are output locations.
const OUTPUT_LOC_LEN: usize = TX_LOC_LEN + 3;

/// The length of a note commitment subtree value: an end height and a root.
const SUBTREE_VALUE_LEN: usize = HEIGHT_LEN + 32;

/// The on-disk genesis height.
const GENESIS: [u8; HEIGHT_LEN] = [0; HEIGHT_LEN];

/// Exports the finalized state at `height` from the database `config` names
/// into `store`, and returns the commitment that names the snapshot: `height`,
/// the hash of block `height`, and the hash of the manifest.
///
/// The state is opened read-only, so a stopped node's state, or a running
/// node's whose tip is `height`, can be exported. Refuses a state whose tip is
/// not `height`, that has no body at `height` or no block hashes from the
/// window start, that needs a format
/// migration or is in a newer format, or that has a spending index, and
/// refuses to run in a build with the `indexer` feature.
pub fn export_state_snapshot(
    config: &Config,
    network: &Network,
    height: block::Height,
    store: &ArtifactStore,
) -> Result<StateSnapshotCommitment, SnapshotError> {
    if height < Height(1) {
        return Err(SnapshotError::GenesisHeight);
    }

    export_with_window(config, network, height, window_start(height), store)
}

/// Exports the snapshot at `height` with a window of hashes and headers from
/// `window_start`, which is [`window_start`]`(height)` for the snapshots
/// nodes import; tests use shorter windows.
///
/// # Panics
///
/// If `window_start` is genesis or above `height`.
pub fn export_with_window(
    config: &Config,
    network: &Network,
    height: block::Height,
    window_start: block::Height,
    store: &ArtifactStore,
) -> Result<StateSnapshotCommitment, SnapshotError> {
    if height < Height(1) {
        return Err(SnapshotError::GenesisHeight);
    }
    assert!(
        Height(1) <= window_start && window_start <= height,
        "callers pass a window that starts above genesis and at or below the snapshot height",
    );
    if cfg!(feature = "indexer") {
        return Err(SnapshotError::IndexerBuild);
    }

    let db = ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &state_database_format_version_in_code(),
        network,
        // A read-only instance never runs format upgrades, whatever this flag says.
        false,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        true,
    )?;
    // A secondary instance sees the database as of its last catch-up, so after this one every
    // component reads the same state.
    db.try_catch_up_with_primary()?;

    check_format(&db)?;
    let block_hash = check_tip(&db, network, height)?;
    check_history(&db, height, window_start)?;

    if NetworkUpgrade::Nu5
        .activation_height(network)
        .is_none_or(|nu5| height < nu5)
    {
        warn!(target: "zebra_state::state_snapshot::export",
            ?height,
            "the snapshot height is below NU5 activation, so nodes will not import the snapshot"
        );
    }

    info!(target: "zebra_state::state_snapshot::export",
        ?height,
        ?window_start,
        path = %db.path().display(),
        store = %store.root().display(),
        "exporting a state snapshot"
    );

    let exporter = Exporter {
        db: &db,
        store,
        height,
        window_start,
        cancelled: AtomicBool::new(false),
    };
    let components = exporter.export_components()?;

    let manifest = Manifest {
        genesis_hash: network.genesis_hash(),
        height,
        block_hash,
        window_start,
        db_format_major: db_format_major(),
        index_mode: INDEX_MODE_DEFAULT,
        parameters_hash: network.state_snapshot_parameters_hash(),
        components,
    };
    let manifest_bytes = manifest.encode()?;
    let manifest_hash = ObjectHash::of(&manifest_bytes);
    // The manifest is written last, so a store never holds a manifest whose pieces it lacks.
    store.put(manifest_hash, &manifest_bytes)?;

    info!(target: "zebra_state::state_snapshot::export",
        ?height,
        ?block_hash,
        manifest = %manifest_hash,
        pieces = manifest.pieces().count(),
        piece_bytes = manifest.total_piece_bytes(),
        "exported a state snapshot"
    );

    Ok(StateSnapshotCommitment {
        height,
        block_hash,
        manifest: manifest_hash,
    })
}

/// Returns the major database format version of this build, which the
/// manifest records.
fn db_format_major() -> u16 {
    u16::try_from(state_database_format_version_in_code().major)
        .expect("database format major versions are small")
}

/// Refuses a state whose format this build cannot export canonically: one
/// that needs a migration, one in a newer format, and one with a spending
/// index.
fn check_format(db: &ZebraDb) -> Result<(), SnapshotError> {
    let running_version = state_database_format_version_in_code();
    let disk_version = db
        .format_version_on_disk()
        .map_err(|source| SnapshotError::FormatVersion { source })?
        .ok_or_else(|| SnapshotError::FormatVersion {
            source: "the state has no format version".into(),
        })?;

    // A state indexed by a build with the `indexer` feature stores spending transaction locations
    // as the nullifier values, so its rows are not the default index mode's.
    if disk_version.build.contains("indexer") {
        return Err(SnapshotError::SpendingIndex { disk_version });
    }

    let release = |version: &Version| (version.major, version.minor, version.patch);
    if release(&disk_version) > release(&running_version) {
        return Err(SnapshotError::NewerFormat {
            disk_version,
            running_version,
        });
    }
    if !can_read_without_migration(&disk_version, &running_version) {
        return Err(SnapshotError::NeedsMigration {
            disk_version,
            running_version,
        });
    }

    Ok(())
}

/// Returns the hash of the block at `height`, which must be the state's
/// finalized tip, after checking the state is for `network`.
fn check_tip(
    db: &ZebraDb,
    network: &Network,
    height: block::Height,
) -> Result<block::Hash, SnapshotError> {
    let (tip, block_hash) = db.tip().ok_or(SnapshotError::EmptyState)?;
    if tip != height {
        return Err(SnapshotError::TipMismatch { height, tip });
    }

    if db.hash(Height(0)) != Some(network.genesis_hash()) {
        return Err(inconsistent(
            ComponentId::HashByHeight,
            format!("the genesis block is not {network}'s"),
        ));
    }

    Ok(block_hash)
}

/// Refuses a state that has no body for the snapshot block, or no block hashes
/// from the first block of the window.
fn check_history(
    db: &ZebraDb,
    height: block::Height,
    window_start: block::Height,
) -> Result<(), SnapshotError> {
    let history = db
        .history_capabilities()
        .map_err(SnapshotError::HistoryRead)?;

    if !history.has_block_body(height) || window_start < history.lowest_known_height {
        return Err(SnapshotError::PrunedBelowWindow {
            window_start,
            lowest_retained_height: history
                .lowest_retained_height
                .max(history.lowest_known_height),
        });
    }

    Ok(())
}

/// Returns the error for a component whose rows in the state are missing or
/// contradict each other.
pub fn inconsistent(component: ComponentId, reason: impl Into<String>) -> SnapshotError {
    SnapshotError::Inconsistent {
        component,
        reason: reason.into(),
    }
}

/// Returns the options for a scan from `from` (inclusive) to `to` (exclusive).
fn scan_options(from: Option<&[u8]>, to: Option<&[u8]>) -> ReadOptions {
    let mut options = ReadOptions::default();
    // An export reads each row once, so it keeps the block cache for other readers.
    options.fill_cache(false);
    options.set_readahead_size(READAHEAD_BYTES);
    if let Some(from) = from {
        options.set_iterate_lower_bound(from.to_vec());
    }
    if let Some(to) = to {
        options.set_iterate_upper_bound(to.to_vec());
    }
    options
}

/// Returns the options for point reads.
fn point_options() -> ReadOptions {
    let mut options = ReadOptions::default();
    options.fill_cache(false);
    options
}

/// Returns the on-disk bytes of `height`.
fn height_bytes(height: Height) -> [u8; HEIGHT_LEN] {
    height.as_bytes()
}

/// Returns the height in the first bytes of an on-disk key.
fn key_height(key: &[u8]) -> Height {
    Height::from_bytes(&key[..HEIGHT_LEN])
}

/// Whether the rows of `component` are a set: keys with empty values.
fn is_set(component: ComponentId) -> bool {
    matches!(
        component,
        ComponentId::SproutNullifiers
            | ComponentId::SaplingNullifiers
            | ComponentId::OrchardNullifiers
            | ComponentId::IronwoodNullifiers
            | ComponentId::SaplingAnchors
            | ComponentId::OrchardAnchors
            | ComponentId::IronwoodAnchors
    )
}

/// Joins a component thread, resuming its panic if it panicked.
fn join<T>(handle: ScopedJoinHandle<'_, T>) -> T {
    handle
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// Writes one component's records into pieces in key order, splitting at
/// record boundaries, and stores each piece when it is full.
pub struct ComponentWriter<'a> {
    component: ComponentId,
    store: &'a ArtifactStore,
    piece: PieceWriter<Vec<u8>>,
    pieces: Vec<PieceManifest>,
    entry_count: u64,
    /// The key of the last record, which the next must be above even if it
    /// starts a new piece.
    last_key: Vec<u8>,
}

impl<'a> ComponentWriter<'a> {
    /// Returns a writer for `component` that stores its pieces in `store`.
    pub fn new(component: ComponentId, store: &'a ArtifactStore) -> Self {
        ComponentWriter {
            component,
            store,
            piece: PieceWriter::new(Vec::new()),
            pieces: Vec::new(),
            entry_count: 0,
            last_key: Vec::new(),
        }
    }

    /// Appends the record `key` → `value`, starting a new piece if the current
    /// one is full.
    pub fn push(&mut self, key: &[u8], value: &[u8]) -> Result<(), SnapshotError> {
        if self.entry_count > 0 && key <= self.last_key.as_slice() {
            return Err(FormatError::KeyNotIncreasing.into());
        }

        match self.piece.push(key, value) {
            Ok(()) => {}
            Err(FormatError::PieceFull) => {
                self.finish_piece()?;
                self.piece.push(key, value)?;
            }
            Err(error) => return Err(error.into()),
        }

        self.entry_count += 1;
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        Ok(())
    }

    /// Appends the record `key` → `value` from the state, canonicalizing the
    /// value if the component requires it.
    fn push_row(&mut self, key: &[u8], value: &[u8]) -> Result<(), SnapshotError> {
        if self.component.is_canonicalized() {
            let value = canonical_value(self.component, key, value)?;
            return self.push(key, &value);
        }

        if is_set(self.component) && !value.is_empty() {
            return Err(inconsistent(
                self.component,
                "a row of a set has a value: the state has a spending index or is corrupt",
            ));
        }
        self.push(key, value)
    }

    /// Stores the current piece, if it has any records, and starts a new one.
    fn finish_piece(&mut self) -> Result<(), SnapshotError> {
        let piece = std::mem::replace(&mut self.piece, PieceWriter::new(Vec::new()));
        if piece.is_empty() {
            return Ok(());
        }

        let bytes = piece.finish()?;
        let hash = ObjectHash::of(&bytes);
        self.store.put(hash, &bytes)?;
        self.pieces.push(PieceManifest {
            size: u32::try_from(bytes.len()).map_err(|_| FormatError::PieceBytesExceeded)?,
            hash,
        });
        Ok(())
    }

    /// Stores the last piece and returns the component's manifest entry.
    pub fn finish(mut self) -> Result<ComponentManifest, SnapshotError> {
        self.finish_piece()?;
        Ok(ComponentManifest {
            id: self.component,
            entry_count: self.entry_count,
            pieces: self.pieces,
        })
    }
}

/// Exports the components of one snapshot from an open state.
pub struct Exporter<'a> {
    db: &'a ZebraDb,
    store: &'a ArtifactStore,
    /// The snapshot height `H`.
    height: Height,
    /// The first height of the window, `L`.
    window_start: Height,
    /// Set when a component fails, so the other components stop scanning: the
    /// export cannot finish, and a refusal known in seconds must not wait for
    /// hours of reads.
    cancelled: AtomicBool,
}

impl<'a> Exporter<'a> {
    /// Returns an exporter of the snapshot at `height`, with its window
    /// starting at `window_start`, from `db` into `store`.
    pub fn new(
        db: &'a ZebraDb,
        store: &'a ArtifactStore,
        height: Height,
        window_start: Height,
    ) -> Self {
        Exporter {
            db,
            store,
            height,
            window_start,
            cancelled: AtomicBool::new(false),
        }
    }

    /// Exports every component, each on its own thread, and returns their
    /// manifest entries in [`ComponentId::ALL`] order.
    ///
    /// The transaction index components wait for the UTXO set, whose
    /// transaction locations in blocks without bodies select their rows. The first
    /// component to fail stops the others, and its error is returned.
    fn export_components(&self) -> Result<Vec<ComponentManifest>, SnapshotError> {
        let mut results = BTreeMap::new();
        let mut without_body = 0;

        thread::scope(|scope| {
            let utxos = scope.spawn(|| self.run(|| self.utxos()));
            let independent: Vec<_> = ComponentId::ALL
                .into_iter()
                .filter(|component| !Self::needs_utxos(*component))
                .map(|component| {
                    let handle = scope.spawn(move || self.run(|| self.component(component)));
                    (component, handle)
                })
                .collect();

            match join(utxos) {
                Ok((utxos, locations)) => {
                    results.insert(ComponentId::Utxos, Ok(utxos));
                    without_body = locations.len();

                    // The threads own the locations, because they may outlive this closure's
                    // locals until the scope joins them.
                    let locations = Arc::new(locations);
                    let tx_loc_by_hash = {
                        let locations = Arc::clone(&locations);
                        scope.spawn(move || self.run(|| self.tx_loc_by_hash(&locations)))
                    };
                    results.insert(ComponentId::TxLocByHash, join(tx_loc_by_hash));
                }
                Err(error) => {
                    results.insert(ComponentId::Utxos, Err(error));
                }
            }

            for (component, handle) in independent {
                results.insert(component, join(handle));
            }
        });

        // A failed UTXO set stops the transaction index, so its error is reported first, then the
        // first failure in component order. A component that stopped because another failed is
        // not the failure.
        let mut manifests = BTreeMap::new();
        let mut error: Option<SnapshotError> = None;
        for component in std::iter::once(ComponentId::Utxos).chain(ComponentId::ALL) {
            let Some(result) = results.remove(&component) else {
                continue;
            };
            match result {
                Ok(manifest) => {
                    info!(target: "zebra_state::state_snapshot::export",
                        %component,
                        entries = manifest.entry_count,
                        pieces = manifest.pieces.len(),
                        "exported a state snapshot component"
                    );
                    manifests.insert(component, manifest);
                }
                Err(failure) => {
                    if error
                        .as_ref()
                        .is_none_or(|error| matches!(error, SnapshotError::Cancelled))
                    {
                        error = Some(failure);
                    }
                }
            }
        }
        if let Some(error) = error {
            return Err(error);
        }

        Self::check_transaction_index(&manifests, without_body)?;

        Ok(ComponentId::ALL
            .into_iter()
            .map(|component| {
                manifests
                    .remove(&component)
                    .expect("every component was exported above")
            })
            .collect())
    }

    /// Whether `component` is exported after the UTXO set.
    fn needs_utxos(component: ComponentId) -> bool {
        matches!(component, ComponentId::Utxos | ComponentId::TxLocByHash)
    }

    /// Checks that the transaction locations ship one row for each
    /// transaction with a body, and `without_body` more.
    fn check_transaction_index(
        manifests: &BTreeMap<ComponentId, ComponentManifest>,
        without_body: usize,
    ) -> Result<(), SnapshotError> {
        let entries = |component| manifests[&component].entry_count;
        // A count of locations in memory fits in a `u64`.
        let expected = entries(ComponentId::Transactions) + without_body as u64;

        if entries(ComponentId::TxLocByHash) != expected {
            return Err(inconsistent(
                ComponentId::TxLocByHash,
                format!(
                    "it has {} rows for the {} shipped transactions",
                    entries(ComponentId::TxLocByHash),
                    expected,
                ),
            ));
        }

        Ok(())
    }

    /// Exports one of the components that do not depend on the UTXO set.
    fn component(&self, component: ComponentId) -> Result<ComponentManifest, SnapshotError> {
        match component {
            ComponentId::HashByHeight | ComponentId::Headers => {
                self.height_rows(component, self.window_start)
            }
            ComponentId::BlockInfo => self.height_rows(component, self.window_parent()),
            ComponentId::HeightByHash => self.height_by_hash(),
            ComponentId::Transactions => self.transactions(),

            ComponentId::SproutNullifiers
            | ComponentId::SaplingNullifiers
            | ComponentId::OrchardNullifiers
            | ComponentId::IronwoodNullifiers
            | ComponentId::SproutAnchors
            | ComponentId::SaplingAnchors
            | ComponentId::OrchardAnchors
            | ComponentId::IronwoodAnchors
            | ComponentId::SaplingSubtrees
            | ComponentId::OrchardSubtrees
            | ComponentId::IronwoodSubtrees => self.all_rows(component),

            ComponentId::SaplingTree => self.tree(component, ComponentId::SaplingSubtrees),
            ComponentId::OrchardTree => self.tree(component, ComponentId::OrchardSubtrees),
            ComponentId::IronwoodTree => self.tree(component, ComponentId::IronwoodSubtrees),

            ComponentId::SproutTipTree | ComponentId::HistoryTree | ComponentId::ValuePools => {
                self.singleton(component)
            }

            ComponentId::Utxos | ComponentId::TxLocByHash => {
                unreachable!("the UTXO set and the transaction index are exported separately")
            }
        }
    }

    /// Runs `export` for one component, stopping the other components if it
    /// fails.
    fn run<T>(
        &self,
        export: impl FnOnce() -> Result<T, SnapshotError>,
    ) -> Result<T, SnapshotError> {
        export().inspect_err(|_| self.cancel())
    }

    /// Stops the other components' scans: a component failed, so the export
    /// cannot finish.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Calls `row` with each row of the column family `cf` from `from`
    /// (inclusive) to `to` (exclusive), in key order, stopping at its first
    /// error, which stops the other components too, or with
    /// [`SnapshotError::Cancelled`] once another component has failed.
    pub fn scan(
        &self,
        cf: &'static str,
        from: Option<&[u8]>,
        to: Option<&[u8]>,
        mut row: impl FnMut(&[u8], &[u8]) -> Result<(), SnapshotError>,
    ) -> Result<(), SnapshotError> {
        let mut iter = self
            .db
            .raw_iterator(cf, scan_options(from, to))
            .ok_or(SnapshotError::MissingColumnFamily(cf))?;

        iter.seek_to_first();
        while let (Some(key), Some(value)) = (iter.key(), iter.value()) {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(SnapshotError::Cancelled);
            }
            row(key, value).inspect_err(|_| self.cancel())?;
            iter.next();
        }
        // An iterator reports read errors here, not by ending early.
        iter.status().inspect_err(|_| self.cancel())?;

        Ok(())
    }

    /// The height below the window: the parent of its first block.
    fn window_parent(&self) -> Height {
        self.window_start
            .previous()
            .expect("the window starts above genesis")
    }

    /// The upper bound of the height-keyed rows of the snapshot: `H + 1`.
    fn end_bytes(&self) -> [u8; HEIGHT_LEN] {
        height_bytes(
            self.height
                .next()
                .expect("the tip is below the maximum on-disk height"),
        )
    }

    /// The height after `height` in a height-keyed component that ships
    /// genesis and `start..=H`, or `None` after `H`.
    fn following(&self, height: Height, start: Height) -> Option<Height> {
        if height >= self.height {
            None
        } else if height == Height(0) {
            Some(start.max(Height(1)))
        } else {
            height.next().ok()
        }
    }

    /// Exports a component keyed by height with a row for genesis and for
    /// every height in `start..=H`.
    fn height_rows(
        &self,
        component: ComponentId,
        start: Height,
    ) -> Result<ComponentManifest, SnapshotError> {
        let cf = component.column_family();
        let mut writer = ComponentWriter::new(component, self.store);
        let mut next = Some(Height(0));

        let mut row = |key: &[u8], value: &[u8]| {
            let expected = next.ok_or_else(|| inconsistent(component, "a row is above the tip"))?;
            if key != height_bytes(expected).as_slice() {
                return Err(inconsistent(
                    component,
                    format!("the row for height {expected:?} is missing"),
                ));
            }
            next = self.following(expected, start);
            writer.push_row(key, value)
        };

        let genesis_end = height_bytes(Height(1));
        if start <= Height(1) {
            self.scan(cf, Some(&GENESIS), Some(&self.end_bytes()), &mut row)?;
        } else {
            self.scan(cf, Some(&GENESIS), Some(&genesis_end), &mut row)?;
            self.scan(
                cf,
                Some(&height_bytes(start)),
                Some(&self.end_bytes()),
                &mut row,
            )?;
        }

        if let Some(missing) = next {
            return Err(inconsistent(
                component,
                format!("the row for height {missing:?} is missing"),
            ));
        }

        writer.finish()
    }

    /// Exports the block heights of the hashes of the shipped blocks.
    fn height_by_hash(&self) -> Result<ComponentManifest, SnapshotError> {
        let component = ComponentId::HeightByHash;

        // The shipped hashes, with their heights.
        let mut rows = Vec::new();
        let mut collect = |height: &[u8], hash: &[u8]| {
            rows.push((hash.to_vec(), height.to_vec()));
            Ok(())
        };
        let hash_by_height = ComponentId::HashByHeight.column_family();
        self.scan(
            hash_by_height,
            Some(&GENESIS),
            Some(&height_bytes(Height(1))),
            &mut collect,
        )?;
        self.scan(
            hash_by_height,
            Some(&height_bytes(self.window_start)),
            Some(&self.end_bytes()),
            &mut collect,
        )?;

        // Each must have its height row, with the same height.
        let cf = component.column_family();
        let mut iter = self
            .db
            .raw_iterator(cf, point_options())
            .ok_or(SnapshotError::MissingColumnFamily(cf))?;
        for (hash, height) in &rows {
            iter.seek(hash);
            if iter.key() != Some(hash.as_slice()) || iter.value() != Some(height.as_slice()) {
                iter.status()?;
                return Err(inconsistent(
                    component,
                    format!(
                        "the block at height {:?} has no matching height row",
                        key_height(height),
                    ),
                ));
            }
        }
        iter.status()?;
        drop(iter);

        rows.sort();
        let mut writer = ComponentWriter::new(component, self.store);
        for (hash, height) in rows {
            writer.push_row(&hash, &height)?;
        }
        writer.finish()
    }

    /// Exports the transactions of genesis and of the snapshot block, checking
    /// that each has its coinbase transaction.
    fn transactions(&self) -> Result<ComponentManifest, SnapshotError> {
        let component = ComponentId::Transactions;
        let cf = component.column_family();
        let mut writer = ComponentWriter::new(component, self.store);
        let mut current: Option<Height> = None;
        let mut next = Some(Height(0));

        let mut row = |key: &[u8], value: &[u8]| {
            if key.len() != TX_LOC_LEN {
                return Err(inconsistent(
                    component,
                    "a key is not a transaction location",
                ));
            }
            let height = key_height(key);
            if current != Some(height) {
                let expected =
                    next.ok_or_else(|| inconsistent(component, "a row is above the tip"))?;
                if height != expected || key[HEIGHT_LEN..] != [0, 0] {
                    return Err(inconsistent(
                        component,
                        format!("the coinbase transaction of the block at height {expected:?} is missing"),
                    ));
                }
                current = Some(height);
                next = self.following(height, self.height);
            }
            writer.push_row(key, value)
        };

        let genesis_end = height_bytes(Height(1));
        let tx_loc = |height: &[u8; HEIGHT_LEN]| [height.as_slice(), &[0, 0]].concat();
        self.scan(
            cf,
            Some(&tx_loc(&GENESIS)),
            Some(&tx_loc(&genesis_end)),
            &mut row,
        )?;
        self.scan(
            cf,
            Some(&tx_loc(&height_bytes(self.height))),
            Some(&tx_loc(&self.end_bytes())),
            &mut row,
        )?;

        if let Some(missing) = next {
            return Err(inconsistent(
                component,
                format!("the coinbase transaction of the block at height {missing:?} is missing"),
            ));
        }

        writer.finish()
    }

    /// Exports the UTXO set, and returns the distinct transaction locations of
    /// its outputs in blocks the snapshot ships no body for, in order.
    fn utxos(&self) -> Result<(ComponentManifest, Vec<[u8; TX_LOC_LEN]>), SnapshotError> {
        let component = ComponentId::Utxos;
        let mut writer = ComponentWriter::new(component, self.store);
        let mut without_body: Vec<[u8; TX_LOC_LEN]> = Vec::new();

        self.scan(component.column_family(), None, None, |key, value| {
            if key.len() != OUTPUT_LOC_LEN {
                return Err(inconsistent(component, "a key is not an output location"));
            }
            let tx_loc: [u8; TX_LOC_LEN] = key[..TX_LOC_LEN]
                .try_into()
                .expect("the key is longer than a transaction location");
            // The transactions of the blocks with bodies are shipped whole.
            if !self.in_shipped_block(&tx_loc) && without_body.last() != Some(&tx_loc) {
                without_body.push(tx_loc);
            }
            writer.push_row(key, value)
        })?;

        Ok((writer.finish()?, without_body))
    }

    /// Whether the transaction at the on-disk location `tx_loc` is shipped
    /// with its block: it is in genesis or in the snapshot block.
    fn in_shipped_block(&self, tx_loc: &[u8]) -> bool {
        tx_loc[..HEIGHT_LEN] == GENESIS || tx_loc[..HEIGHT_LEN] == height_bytes(self.height)[..]
    }

    /// Exports the locations of the shipped transactions by hash: one scan,
    /// whose rows are already in key order.
    fn tx_loc_by_hash(
        &self,
        without_body: &[[u8; TX_LOC_LEN]],
    ) -> Result<ComponentManifest, SnapshotError> {
        let component = ComponentId::TxLocByHash;
        let mut writer = ComponentWriter::new(component, self.store);

        self.scan(component.column_family(), None, None, |key, value| {
            let Ok(tx_loc) = <[u8; TX_LOC_LEN]>::try_from(value) else {
                return Err(inconsistent(
                    component,
                    "a value is not a transaction location",
                ));
            };
            if self.in_shipped_block(&tx_loc) || without_body.binary_search(&tx_loc).is_ok() {
                writer.push_row(key, value)?;
            }
            Ok(())
        })?;

        writer.finish()
    }

    /// Exports every row of `component`.
    fn all_rows(&self, component: ComponentId) -> Result<ComponentManifest, SnapshotError> {
        let mut writer = ComponentWriter::new(component, self.store);
        self.scan(component.column_family(), None, None, |key, value| {
            writer.push_row(key, value)
        })?;
        writer.finish()
    }

    /// Exports the note commitment trees of `component` that the snapshot
    /// needs: genesis, the end height of every subtree in `subtrees`, the last
    /// row below the window, and every row in the window.
    fn tree(
        &self,
        component: ComponentId,
        subtrees: ComponentId,
    ) -> Result<ComponentManifest, SnapshotError> {
        let window = height_bytes(self.window_start);

        // The heights below the window that need their exact row: genesis, for the genesis root
        // checks, and the subtree end heights, for the subtree checks.
        let mut needed = BTreeSet::from([GENESIS]);
        self.scan(subtrees.column_family(), None, None, |_key, value| {
            let end_height = value
                .get(..HEIGHT_LEN)
                .filter(|_| value.len() == SUBTREE_VALUE_LEN)
                .and_then(|end_height| <[u8; HEIGHT_LEN]>::try_from(end_height).ok())
                .ok_or_else(|| inconsistent(subtrees, "a value is not an end height and a root"))?;
            if end_height < window {
                needed.insert(end_height);
            }
            Ok(())
        })?;

        let cf = component.column_family();
        let mut rows = BTreeMap::new();
        let mut iter = self
            .db
            .raw_iterator(cf, point_options())
            .ok_or(SnapshotError::MissingColumnFamily(cf))?;
        for height in needed {
            iter.seek(height);
            match (iter.key(), iter.value()) {
                (Some(key), Some(value)) if key == height.as_slice() => {
                    rows.insert(key.to_vec(), value.to_vec());
                }
                _ => {
                    iter.status()?;
                    return Err(inconsistent(
                        component,
                        format!("the tree at height {:?} is missing", key_height(&height)),
                    ));
                }
            }
        }

        // The tree in effect below the window, which reads back for the window heights before
        // the pool's first change in the window.
        iter.seek_for_prev(height_bytes(self.window_parent()));
        if let (Some(key), Some(value)) = (iter.key(), iter.value()) {
            rows.insert(key.to_vec(), value.to_vec());
        }
        iter.status()?;
        drop(iter);

        self.scan(cf, Some(&window), Some(&self.end_bytes()), |key, value| {
            rows.insert(key.to_vec(), value.to_vec());
            Ok(())
        })?;

        let mut writer = ComponentWriter::new(component, self.store);
        for (key, value) in rows {
            if key.len() != HEIGHT_LEN {
                return Err(inconsistent(component, "a key is not a height"));
            }
            writer.push_row(&key, &value)?;
        }
        writer.finish()
    }

    /// Exports a singleton, keyed by `()`.
    ///
    /// Formats before 1.4.0 keyed the Sprout tip tree and the history tree by
    /// the tip height; like the state's readers, the exporter then reads the
    /// last such row, and ships it keyed by `()`. The value pools were always
    /// keyed by `()`, the only row the state reads for them, so any other row
    /// is refused.
    fn singleton(&self, component: ComponentId) -> Result<ComponentManifest, SnapshotError> {
        let cf = component.column_family();
        let mut iter = self
            .db
            .raw_iterator(cf, point_options())
            .ok_or(SnapshotError::MissingColumnFamily(cf))?;

        // The empty key sorts first.
        iter.seek_to_first();
        if iter.key().is_some_and(|key| !key.is_empty()) {
            if component == ComponentId::ValuePools {
                return Err(inconsistent(component, "the row is not keyed by ()"));
            }
            iter.seek_to_last();
        }
        let value = iter.value().map(<[u8]>::to_vec);
        iter.status()?;
        drop(iter);

        let mut writer = ComponentWriter::new(component, self.store);
        match value {
            Some(value) => writer.push_row(&[], &value)?,
            // There is no history tree before Heartwood.
            None if component == ComponentId::HistoryTree => {}
            None => return Err(inconsistent(component, "the state has no row")),
        }
        writer.finish()
    }
}
