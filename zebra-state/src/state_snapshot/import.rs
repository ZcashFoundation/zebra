//! Importing a verified state snapshot into a new finalized database
//! (design, "State changes").
//!
//! Each verified piece becomes one SST file for its component, built as the
//! pieces arrive. The importer then ingests the SST files into a database
//! under a temporary root, writes the singletons, the [`SnapshotBase`] marker
//! and the version file, reopens the database to run the self-consistency and
//! chain checks, and renames it into place only if every check passes.
//!
//! A database renamed into place has passed every check of the design's trust
//! model, and the database format checks that run at every start, so a
//! snapshot that is wrong for this node is refused with
//! [`SnapshotError::Refused`] instead of failing later.
//!
//! The manifest's parameters hash is checked against the network's consensus
//! parameters, so Regtest and custom-testnet snapshots cannot be imported for
//! a different configuration with the same genesis block.

use std::{
    collections::HashMap,
    fmt,
    fs::{self, File},
    io::{self, BufReader, Read, Write},
    path::{Path, PathBuf},
};

use rayon::prelude::*;
use rocksdb::{IngestExternalFileOptions, SstFileWriter, WriteBatch, DB};

use zebra_chain::{
    block,
    history_tree::NonEmptyHistoryTree,
    parameters::{Network, NetworkUpgrade},
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

use crate::{
    config::write_database_format_version_at_path,
    constants::{
        state_database_format_version_in_code, DATABASE_FORMAT_VERSION_FILE_NAME,
        STATE_DATABASE_KIND,
    },
    service::finalized_state::{
        DiskDb, HistoryTreeParts, SNAPSHOT_BASE, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    Config, IntoDisk,
};

use super::{
    format::{ComponentId, Manifest, PieceManifest, PieceReader, MAX_PIECE_BYTES},
    SnapshotBase, SnapshotError,
};

mod checks;
mod records;

#[cfg(test)]
mod tests;

/// The directory under the state cache directory that holds import temporary
/// roots, one per bootstrap: `<cache_dir>/state-snapshot-import/<nonce>/`.
pub const IMPORT_DIR: &str = "state-snapshot-import";

/// The number of block hashes and headers a snapshot carries, ending at the
/// snapshot height: its window is `L..=H` with `L = max(1, H - 2303)` (design,
/// "Snapshot contents").
pub const WINDOW_BLOCKS: u32 = 2_304;

/// The largest SST metadata file: two keys of at most a piece each, the piece
/// hash and the counts.
const MAX_SST_METADATA_BYTES: usize = 2 * MAX_PIECE_BYTES + 32 + 8 + 4 + 4;

/// Returns the directory that holds the import temporary roots for the state
/// `config`. A leftover from an interrupted bootstrap is deleted before the
/// next one starts.
pub fn import_dir(config: &Config) -> PathBuf {
    config.cache_dir.join(IMPORT_DIR)
}

/// Returns where the SST file built from piece `index` of `component` goes
/// under the import temporary root `temp_root`.
pub fn sst_path(temp_root: &Path, component: ComponentId, index: u32) -> PathBuf {
    temp_root
        .join("sst")
        .join(component.column_family())
        .join(format!("{index:08}.sst"))
}

/// Returns the window start `L` of a snapshot at `height`: the first height
/// whose hash and header the snapshot carries, `max(1, H - 2303)`.
pub fn window_start(height: block::Height) -> block::Height {
    block::Height(height.0.saturating_sub(WINDOW_BLOCKS - 1).max(1))
}

/// Returns whether `component` is a singleton: one record under the empty key,
/// which the importer writes with a batch instead of an SST file. The caller
/// does not need to build SST files for singleton pieces.
pub fn is_singleton(component: ComponentId) -> bool {
    matches!(
        component,
        ComponentId::SproutTipTree | ComponentId::HistoryTree | ComponentId::ValuePools
    )
}

/// The checks of a state snapshot import, as [`SnapshotError::Refused`] names
/// them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum ImportCheck {
    /// The manifest hashes to the commitment's manifest hash.
    ///
    /// The caller verifies the manifest object against the commitment before
    /// importing it, so a manifest that fails this check is a local fault, not
    /// the snapshot's.
    ManifestHash,

    /// The manifest describes the committed block, for this network and
    /// database format, with the window the design gives it.
    Manifest,

    /// The snapshot meets the import preconditions (design, "Trust model and
    /// verification"): NU5 is active
    /// at its height `H`, and `H + 1` is at or below the network's last
    /// checkpoint, so the block after the snapshot is checkpoint-verified.
    ///
    /// The other preconditions, `consensus.checkpoint_sync` and the checkpoint
    /// height the block verifier router settles on, are the bootstrap decision
    /// rule's to check: the importer does not see them, and the network's
    /// checkpoint list bounds the router's height from above.
    Preconditions,

    /// The hash of the snapshot block is the checkpoint at its height, when the
    /// height has a checkpoint.
    Checkpoint,

    /// A piece file has the size and hash its manifest entry gives.
    ///
    /// The scheduler verifies every piece as it arrives, so a piece file that
    /// fails this check is a local fault, not the snapshot's.
    Piece,

    /// A piece holds valid records of its component, in the current database
    /// encoding.
    Records,

    /// The pieces of each component hold the manifest's entry count, in key
    /// order, and every singleton is present.
    EntryCounts,

    /// The history tree ends at the snapshot block, for the network upgrade at
    /// the snapshot height.
    HistoryTree,

    /// The note commitment trees at the snapshot height have the roots the
    /// history tree commits to, and those roots are anchors.
    TreeRoots,

    /// The window headers chain to the snapshot block, and the genesis and
    /// snapshot block bodies are complete.
    Window,

    /// Every stored transaction location has its hash and location index rows.
    TransactionIndex,

    /// The address indexes match the UTXO set.
    AddressIndex,

    /// The value pools match the UTXO set and the block info at the snapshot
    /// height.
    ValuePools,

    /// The database opens as the current format, describes the snapshot, and
    /// passes the database format checks.
    DatabaseFormat,
}

impl ImportCheck {
    /// Returns whether a failure of this check is the snapshot's fault, so the
    /// snapshot must never be imported again.
    ///
    /// A piece file or a manifest that does not match the commitment is this
    /// node's fault: the caller verified both before the import.
    pub fn rejects_snapshot(self) -> bool {
        !matches!(self, ImportCheck::Piece | ImportCheck::ManifestHash)
    }
}

impl fmt::Display for ImportCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ImportCheck::ManifestHash => "manifest hash",
            ImportCheck::Manifest => "manifest",
            ImportCheck::Preconditions => "precondition",
            ImportCheck::Checkpoint => "checkpoint",
            ImportCheck::Piece => "piece",
            ImportCheck::Records => "record",
            ImportCheck::EntryCounts => "entry count",
            ImportCheck::HistoryTree => "history tree",
            ImportCheck::TreeRoots => "tree root",
            ImportCheck::Window => "window",
            ImportCheck::TransactionIndex => "transaction index",
            ImportCheck::AddressIndex => "address index",
            ImportCheck::ValuePools => "value pool",
            ImportCheck::DatabaseFormat => "database format",
        })
    }
}

/// Returns a refusal by `check`.
fn refused(check: ImportCheck, detail: impl Into<String>) -> SnapshotError {
    SnapshotError::Refused {
        check,
        detail: detail.into(),
    }
}

/// Returns a closure that maps an I/O error at `path` to a [`SnapshotError`].
fn io_error(path: &Path) -> impl FnOnce(io::Error) -> SnapshotError + '_ {
    move |source| SnapshotError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Returns a closure that maps a database error during `operation` to a
/// [`SnapshotError`].
fn db_error(operation: &'static str) -> impl FnOnce(rocksdb::Error) -> SnapshotError {
    move |source| SnapshotError::Database { operation, source }
}

/// Builds one SST file at `sst_path` for `component` from the verified piece
/// file at `piece_path`, and returns the number of records it holds.
///
/// The piece must have the size and hash of its manifest entry `piece`, and
/// every record must pass the component's record checks: keys and values in
/// the current database encoding, canonicalized values in their canonical
/// encoding, and note commitment trees whose cached roots are their recomputed
/// roots. The SST file is built with the database's options and is complete
/// once it exists, with a metadata file next to it that names the piece, so
/// [`import_state_snapshot`] reuses it only for that piece.
pub fn build_piece_sst(
    piece_path: &Path,
    component: ComponentId,
    piece: PieceManifest,
    sst_path: &Path,
) -> Result<u64, SnapshotError> {
    let bytes = read_piece(piece_path, piece)?;
    let mut reader = PieceReader::new(bytes.as_slice());

    let directory = sst_path
        .parent()
        .ok_or_else(|| SnapshotError::Unsupported(format!("no directory for {sst_path:?}")))?;
    fs::create_dir_all(directory).map_err(io_error(directory))?;
    let partial = with_suffix(sst_path, ".partial");

    let options = DiskDb::options();
    let mut writer = SstFileWriter::create(&options);
    writer
        .open(&partial)
        .map_err(db_error("creating a snapshot SST file"))?;

    let mut first_key = None;
    let mut last_key = Vec::new();
    while let Some(record) = reader.next_record().map_err(|error| {
        refused(
            ImportCheck::Records,
            format!("{component} piece {}: {error}", piece.hash),
        )
    })? {
        records::check_record(component, &record.key, &record.value).map_err(|detail| {
            refused(
                ImportCheck::Records,
                format!("{component} piece {}: {detail}", piece.hash),
            )
        })?;
        writer
            .put(&record.key, &record.value)
            .map_err(db_error("writing a snapshot SST file"))?;
        if first_key.is_none() {
            first_key = Some(record.key.clone());
        }
        last_key = record.key;
    }
    let first_key = first_key.ok_or_else(|| {
        refused(
            ImportCheck::Records,
            format!("{component} piece {} has no records", piece.hash),
        )
    })?;
    writer
        .finish()
        .map_err(db_error("finishing a snapshot SST file"))?;

    let records = reader.records();
    let metadata = SstMetadata {
        piece: piece.hash,
        records,
        first_key,
        last_key,
    };
    let metadata_path = sst_metadata_path(sst_path);
    let partial_metadata = with_suffix(&metadata_path, ".partial");
    write_synced(&partial_metadata, &metadata.encode())?;
    fs::rename(&partial_metadata, &metadata_path).map_err(io_error(&metadata_path))?;
    fs::rename(&partial, sst_path).map_err(io_error(sst_path))?;

    Ok(records)
}

/// Reads the verified piece at `path`, checking its size before reading it and
/// its hash after.
fn read_piece(path: &Path, piece: PieceManifest) -> Result<Vec<u8>, SnapshotError> {
    // A `u32` fits in a `usize` on every platform Zebra supports.
    if piece.size as usize > MAX_PIECE_BYTES {
        return Err(refused(
            ImportCheck::Records,
            format!("piece {} is over the piece size limit", piece.hash),
        ));
    }
    let file = File::open(path).map_err(io_error(path))?;
    let size = file.metadata().map_err(io_error(path))?.len();
    if size != u64::from(piece.size) {
        return Err(refused(
            ImportCheck::Piece,
            format!(
                "piece {} is {size} bytes, not the manifest's {}",
                piece.hash, piece.size
            ),
        ));
    }

    // The size was just checked against `MAX_PIECE_BYTES`.
    let mut bytes = Vec::with_capacity(piece.size as usize);
    file.take(u64::from(piece.size))
        .read_to_end(&mut bytes)
        .map_err(io_error(path))?;
    let hash = ObjectHash::of(&bytes);
    if hash != piece.hash {
        return Err(refused(
            ImportCheck::Piece,
            format!("piece {} hashes to {hash}", piece.hash),
        ));
    }
    Ok(bytes)
}

/// Returns `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Returns the path of the metadata file of the SST file at `sst_path`.
fn sst_metadata_path(sst_path: &Path) -> PathBuf {
    with_suffix(sst_path, ".meta")
}

/// Writes `bytes` to a new file at `path` and syncs it.
fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    let mut file = File::create(path).map_err(io_error(path))?;
    file.write_all(bytes).map_err(io_error(path))?;
    file.sync_all().map_err(io_error(path))
}

/// What an SST file holds: the piece it was built from, its record count, and
/// its key range, which [`build_piece_sst`] writes next to it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SstMetadata {
    /// The hash of the piece the SST file was built from.
    piece: ObjectHash,

    /// The number of records in the piece.
    records: u64,

    /// The first key in the piece.
    first_key: Vec<u8>,

    /// The last key in the piece.
    last_key: Vec<u8>,
}

impl SstMetadata {
    /// Encodes the metadata: the piece hash, the record count, and each key
    /// with a 4-byte length, all little-endian.
    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(32 + 8 + 8 + self.first_key.len() + self.last_key.len());
        bytes.extend_from_slice(&self.piece.0);
        bytes.extend_from_slice(&self.records.to_le_bytes());
        for key in [&self.first_key, &self.last_key] {
            // Keys come from a piece, which is at most `MAX_PIECE_BYTES` long.
            bytes.extend_from_slice(&(key.len() as u32).to_le_bytes());
            bytes.extend_from_slice(key);
        }
        bytes
    }

    /// Decodes metadata that [`Self::encode`] wrote, or returns `None`.
    fn decode(bytes: &[u8]) -> Option<Self> {
        let (piece, rest) = bytes.split_first_chunk::<32>()?;
        let (records, mut rest) = rest.split_first_chunk::<8>()?;
        let mut keys = Vec::with_capacity(2);
        for _ in 0..2 {
            let (len, after_len) = rest.split_first_chunk::<4>()?;
            let len = usize::try_from(u32::from_le_bytes(*len)).ok()?;
            if after_len.len() < len {
                return None;
            }
            let (key, after_key) = after_len.split_at(len);
            keys.push(key.to_vec());
            rest = after_key;
        }
        let last_key = keys.pop()?;
        let first_key = keys.pop()?;
        rest.is_empty().then_some(SstMetadata {
            piece: ObjectHash(*piece),
            records: u64::from_le_bytes(*records),
            first_key,
            last_key,
        })
    }

    /// Reads the metadata of the SST file at `sst_path`, or returns `None` if
    /// there is no complete SST file there.
    fn read(sst_path: &Path) -> Result<Option<Self>, SnapshotError> {
        if !sst_path.is_file() {
            return Ok(None);
        }
        let path = sst_metadata_path(sst_path);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&path)(error)),
        };
        let mut bytes = Vec::new();
        // The bound is far above any metadata `build_piece_sst` writes.
        file.take(MAX_SST_METADATA_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error(&path))?;
        Ok(Self::decode(&bytes))
    }
}

/// Decodes the history tree a snapshot ships for its height from the single
/// record of its `history_tree` piece at `piece_path`, for `network`.
///
/// The value is not trusted: it must be the canonical encoding of a tree for
/// this network that rebuilds from its peaks.
pub fn decode_history_tree(
    piece_path: &Path,
    network: &Network,
) -> Result<NonEmptyHistoryTree, SnapshotError> {
    let io_error = |source| SnapshotError::Io {
        path: piece_path.to_path_buf(),
        source,
    };

    let file = File::open(piece_path).map_err(io_error)?;
    let mut reader = PieceReader::new(BufReader::new(file));

    let record = reader.next_record()?.ok_or_else(|| {
        SnapshotError::InvalidHistoryTree("the history tree piece has no record".to_string())
    })?;
    if !record.key.is_empty() {
        return Err(SnapshotError::InvalidHistoryTree(
            "the history tree record has a key, but the column family has a single `()` key"
                .to_string(),
        ));
    }
    if reader.next_record()?.is_some() {
        return Err(SnapshotError::InvalidHistoryTree(
            "the history tree piece has more than one record".to_string(),
        ));
    }

    HistoryTreeParts::decode_canonical(&record.value, network)
        .map_err(SnapshotError::InvalidHistoryTree)
}

/// Imports the snapshot `manifest` describes into the database `config` names
/// for `network`, from the SST files built under `temp_root` and the verified
/// `pieces`, by hash, and returns the marker the database records.
///
/// `deferred_checks` is the set of [`SnapshotBase`] `DEFERRED_*` checks the
/// caller could not run against the chain. The importer checks the block hash
/// itself when the snapshot height has a checkpoint, and records the others
/// in the marker.
///
/// SST files the caller did not build are built here, from `pieces`. Every
/// check of the design's trust model runs before the database is renamed into
/// place, and any failure refuses the import: [`SnapshotError::Refused`] names
/// the check. The caller deletes `temp_root` afterwards, whatever the result.
pub fn import_state_snapshot(
    temp_root: &Path,
    manifest: &Manifest,
    commitment: StateSnapshotCommitment,
    deferred_checks: u8,
    network: &Network,
    config: &Config,
    pieces: &HashMap<ObjectHash, PathBuf>,
) -> Result<SnapshotBase, SnapshotError> {
    let running_version = state_database_format_version_in_code();
    let target = config.db_path(STATE_DATABASE_KIND, running_version.major, network);
    let import_config = import_config(temp_root, config)?;
    let import_path = import_config.db_path(STATE_DATABASE_KIND, running_version.major, network);
    if target.exists() {
        return Err(SnapshotError::Unsupported(format!(
            "a state database already exists at {target:?}"
        )));
    }
    if import_path.exists() {
        return Err(SnapshotError::Unsupported(format!(
            "the import directory {import_path:?} is not empty"
        )));
    }

    let base = check_manifest(manifest, commitment, deferred_checks, network)?;
    let ssts = prepare_ssts(temp_root, manifest, pieces)?;

    {
        let db = open_raw(&import_path)?;
        for (component, paths) in &ssts {
            ingest(&db, *component, paths)?;
        }
        let singletons = write_singletons(&db, manifest, base, network, pieces)?;
        checks::check_raw(&db, manifest, &base, network, &singletons)?;
        let version_path = import_path.join(DATABASE_FORMAT_VERSION_FILE_NAME);
        write_database_format_version_at_path(version_path.clone(), &running_version).map_err(
            |error| SnapshotError::Io {
                path: version_path,
                source: io::Error::other(error),
            },
        )?;
    }

    checks::check_database(&import_config, network, &base)?;

    sync_tree(&import_path)?;
    if let Some(backup) = config.non_finalized_state_backup_dir(network) {
        match fs::remove_dir_all(&backup) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&backup)(error)),
        }
    }
    install(&import_path, &target)?;

    Ok(base)
}

/// Returns the configuration that opens the database under `temp_root`: the
/// node's state settings, with the temporary root as its cache directory and
/// nothing that writes or deletes outside it.
fn import_config(temp_root: &Path, config: &Config) -> Result<Config, SnapshotError> {
    if config.ephemeral {
        return Err(SnapshotError::Unsupported(
            "an ephemeral state cannot be imported from a snapshot".to_string(),
        ));
    }
    if config.prune_blocks_older_than.is_none() {
        return Err(SnapshotError::Unsupported(
            "a state imported from a snapshot requires pruning".to_string(),
        ));
    }
    if cfg!(feature = "indexer") {
        return Err(SnapshotError::Unsupported(
            "snapshots carry no spending index, so builds with the indexer feature cannot \
             import them"
                .to_string(),
        ));
    }

    Ok(Config {
        cache_dir: temp_root.to_path_buf(),
        delete_old_database: false,
        should_backup_non_finalized_state: false,
        debug_stop_at_height: None,
        debug_validity_check_interval: None,
        ..config.clone()
    })
}

/// Checks the manifest against the commitment, the network and the running
/// database format, and returns the marker the database will record.
fn check_manifest(
    manifest: &Manifest,
    commitment: StateSnapshotCommitment,
    deferred_checks: u8,
    network: &Network,
) -> Result<SnapshotBase, SnapshotError> {
    manifest.check()?;
    let manifest_hash = ObjectHash::of(&manifest.encode()?);
    if manifest_hash != commitment.manifest {
        return Err(refused(
            ImportCheck::ManifestHash,
            format!(
                "the manifest hashes to {manifest_hash}, not {}",
                commitment.manifest
            ),
        ));
    }

    let height = manifest.height;
    let mismatch = if height != commitment.height || manifest.block_hash != commitment.block_hash {
        Some(format!(
            "the manifest is for block {} at {height:?}, not {} at {:?}",
            manifest.block_hash, commitment.block_hash, commitment.height
        ))
    } else if manifest.genesis_hash != network.genesis_hash() {
        Some(format!(
            "the manifest is for the network with genesis {}, not {}",
            manifest.genesis_hash,
            network.genesis_hash()
        ))
    } else if manifest.parameters_hash.is_some() != Manifest::needs_parameters_hash(network) {
        Some("the manifest's parameters hash does not match the network kind".to_string())
    } else if manifest.parameters_hash != network.state_snapshot_parameters_hash() {
        Some("the manifest's parameters hash does not match the consensus parameters".to_string())
    } else if u64::from(manifest.db_format_major) != state_database_format_version_in_code().major {
        Some(format!(
            "the manifest is in database format {}, not {}",
            manifest.db_format_major,
            state_database_format_version_in_code().major
        ))
    } else if manifest.window_start != window_start(height) {
        Some(format!(
            "the window starts at {:?}, not {:?}",
            manifest.window_start,
            window_start(height)
        ))
    } else {
        None
    };
    if let Some(mismatch) = mismatch {
        return Err(refused(ImportCheck::Manifest, mismatch));
    }

    if NetworkUpgrade::Nu5
        .activation_height(network)
        .is_none_or(|activation| activation > height)
    {
        return Err(refused(
            ImportCheck::Preconditions,
            format!("NU5 is not active at the snapshot height {height:?}"),
        ));
    }
    // `H + 1 <= C` for the last checkpoint `C` (design, "Trust model and verification"): the
    // block after the snapshot is
    // checkpoint-verified, and the checkpoint verifier anchors `C` back to the snapshot block.
    let last_checkpoint = network.checkpoint_list().max_height();
    if height >= last_checkpoint {
        return Err(refused(
            ImportCheck::Preconditions,
            format!(
                "the snapshot height {height:?} is not below the last checkpoint {last_checkpoint:?}"
            ),
        ));
    }

    let mut deferred_checks = deferred_checks
        & (SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS);
    if let Some(checkpoint) = network.checkpoint_list().hash(height) {
        if checkpoint != commitment.block_hash {
            return Err(refused(
                ImportCheck::Checkpoint,
                format!(
                    "block {} at {height:?} is not the checkpoint {checkpoint}",
                    commitment.block_hash
                ),
            ));
        }
        deferred_checks &= !SnapshotBase::DEFERRED_BLOCK_HASH;
    }

    Ok(SnapshotBase {
        window_start: manifest.window_start,
        height,
        block_hash: commitment.block_hash,
        manifest: commitment.manifest,
        deferred_checks,
    })
}

/// Returns the SST files of every component that is not a singleton, in piece
/// order, building the ones the caller has not built, after checking their
/// entry counts and key order against the manifest.
fn prepare_ssts(
    temp_root: &Path,
    manifest: &Manifest,
    pieces: &HashMap<ObjectHash, PathBuf>,
) -> Result<Vec<(ComponentId, Vec<PathBuf>)>, SnapshotError> {
    let mut missing = Vec::new();
    for (component, index, piece) in manifest.pieces() {
        if is_singleton(component) {
            continue;
        }
        let path = sst_path(temp_root, component, index);
        if SstMetadata::read(&path)?.is_none_or(|metadata| metadata.piece != piece.hash) {
            let piece_path = pieces.get(&piece.hash).ok_or(SnapshotError::MissingPiece {
                component,
                index,
                hash: piece.hash,
            })?;
            missing.push((component, piece, piece_path, path));
        }
    }
    missing
        .par_iter()
        .try_for_each(|(component, piece, piece_path, path)| {
            build_piece_sst(piece_path, *component, *piece, path).map(drop)
        })?;

    let mut ssts = Vec::new();
    for component in &manifest.components {
        if is_singleton(component.id) || component.pieces.is_empty() {
            continue;
        }
        let mut paths = Vec::with_capacity(component.pieces.len());
        let mut records = 0u64;
        let mut last_key: Option<Vec<u8>> = None;
        for (index, piece) in (0u32..).zip(&component.pieces) {
            let path = sst_path(temp_root, component.id, index);
            let metadata = SstMetadata::read(&path)?
                .filter(|metadata| metadata.piece == piece.hash)
                .ok_or(SnapshotError::MissingPiece {
                    component: component.id,
                    index,
                    hash: piece.hash,
                })?;
            if last_key
                .as_ref()
                .is_some_and(|last_key| *last_key >= metadata.first_key)
            {
                return Err(refused(
                    ImportCheck::EntryCounts,
                    format!(
                        "piece {index} of {} does not start after the previous piece",
                        component.id
                    ),
                ));
            }
            records = records.saturating_add(metadata.records);
            last_key = Some(metadata.last_key);
            paths.push(path);
        }
        if records != component.entry_count {
            return Err(refused(
                ImportCheck::EntryCounts,
                format!(
                    "{} has {records} records, not the manifest's {}",
                    component.id, component.entry_count
                ),
            ));
        }
        ssts.push((component.id, paths));
    }
    Ok(ssts)
}

/// Opens a new database at `path` with every column family and the database's
/// options, with automatic compactions off while the SST files are ingested.
fn open_raw(path: &Path) -> Result<DB, SnapshotError> {
    let mut options = DiskDb::options();
    options.set_disable_auto_compactions(true);
    let column_families = DiskDb::construct_column_families(
        options.clone(),
        path,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
    );
    fs::create_dir_all(path).map_err(io_error(path))?;
    DB::open_cf_descriptors(&options, path, column_families)
        .map_err(db_error("creating the import database"))
}

/// Returns the handle of `name`, which [`open_raw`] created.
fn column_family<'db>(db: &'db DB, name: &str) -> &'db rocksdb::ColumnFamily {
    db.cf_handle(name)
        .expect("the import database is opened with every column family in the code")
}

/// Ingests the SST files of `component` into its empty column family, moving
/// them into the database.
///
/// Global sequence numbers are disabled, so the files land at the bottom level
/// and ingestion fails if the column family is not empty or the files overlap.
fn ingest(db: &DB, component: ComponentId, paths: &[PathBuf]) -> Result<(), SnapshotError> {
    let mut options = IngestExternalFileOptions::default();
    options.set_move_files(true);
    options.set_allow_global_seqno(false);
    db.ingest_external_file_cf_opts(
        column_family(db, component.column_family()),
        &options,
        paths.to_vec(),
    )
    .map_err(db_error("ingesting snapshot SST files"))
}

/// The decoded singletons of a snapshot.
pub(super) struct Singletons {
    /// The history tree at the snapshot height.
    pub(super) history_tree: zebra_chain::history_tree::NonEmptyHistoryTree,

    /// The Sprout note commitment tree at the snapshot height.
    pub(super) sprout_tree: zebra_chain::sprout::tree::NoteCommitmentTree,

    /// The chain value pools at the snapshot height.
    pub(super) value_pools:
        zebra_chain::value_balance::ValueBalance<zebra_chain::amount::NonNegative>,
}

/// Reads, checks and writes the singleton components and the marker in one
/// batch, and returns the decoded singletons.
fn write_singletons(
    db: &DB,
    manifest: &Manifest,
    base: SnapshotBase,
    network: &Network,
    pieces: &HashMap<ObjectHash, PathBuf>,
) -> Result<Singletons, SnapshotError> {
    let mut batch = WriteBatch::default();
    let mut values = HashMap::new();
    for component in [
        ComponentId::SproutTipTree,
        ComponentId::HistoryTree,
        ComponentId::ValuePools,
    ] {
        let entry = manifest.component(component);
        let [piece] = entry.pieces.as_slice() else {
            return Err(refused(
                ImportCheck::EntryCounts,
                format!("{component} has {} pieces, not one", entry.pieces.len()),
            ));
        };
        let path = pieces.get(&piece.hash).ok_or(SnapshotError::MissingPiece {
            component,
            index: 0,
            hash: piece.hash,
        })?;
        let bytes = read_piece(path, *piece)?;
        let mut reader = PieceReader::new(bytes.as_slice());
        let records = |error| refused(ImportCheck::Records, format!("{component}: {error}"));
        let record = reader.next_record().map_err(records)?;
        let Some(record) = record.filter(|_| entry.entry_count == 1) else {
            return Err(refused(
                ImportCheck::EntryCounts,
                format!("{component} does not hold exactly one record"),
            ));
        };
        if reader.next_record().map_err(records)?.is_some() {
            return Err(refused(
                ImportCheck::EntryCounts,
                format!("{component} holds more than one record"),
            ));
        }
        records::check_record(component, &record.key, &record.value)
            .map_err(|detail| refused(ImportCheck::Records, format!("{component}: {detail}")))?;
        batch.put_cf(
            column_family(db, component.column_family()),
            &record.key,
            &record.value,
        );
        values.insert(component, record.value);
    }
    batch.put_cf(
        column_family(db, SNAPSHOT_BASE),
        ().as_bytes(),
        base.as_bytes(),
    );

    let decoded = |component| {
        values
            .get(&component)
            .map(Vec::as_slice)
            .expect("every singleton was read above")
    };
    let singletons = Singletons {
        history_tree: checks::history_tree(decoded(ComponentId::HistoryTree), &base, network)?,
        sprout_tree: records::sprout_tree(decoded(ComponentId::SproutTipTree))
            .map_err(|detail| refused(ImportCheck::Records, detail))?,
        value_pools: records::value_pools(decoded(ComponentId::ValuePools))
            .map_err(|detail| refused(ImportCheck::Records, detail))?,
    };

    db.write(batch)
        .map_err(db_error("writing the snapshot singletons"))?;
    for component in [
        ComponentId::SproutTipTree,
        ComponentId::HistoryTree,
        ComponentId::ValuePools,
    ] {
        db.flush_cf(column_family(db, component.column_family()))
            .map_err(db_error("flushing the snapshot singletons"))?;
    }
    db.flush_cf(column_family(db, SNAPSHOT_BASE))
        .map_err(db_error("flushing the snapshot base marker"))?;
    db.flush_wal(true)
        .map_err(db_error("syncing the import database"))?;

    Ok(singletons)
}

/// Syncs every file under `path`, then the directories, so a crash after the
/// rename leaves a complete database.
fn sync_tree(path: &Path) -> Result<(), SnapshotError> {
    for entry in fs::read_dir(path).map_err(io_error(path))? {
        let entry = entry.map_err(io_error(path))?;
        let entry_path = entry.path();
        if entry.file_type().map_err(io_error(&entry_path))?.is_dir() {
            sync_tree(&entry_path)?;
        } else {
            File::open(&entry_path)
                .and_then(|file| file.sync_all())
                .map_err(io_error(&entry_path))?;
        }
    }
    sync_directory(path)
}

/// Syncs the directory at `path`, so the entries renamed into it are durable.
///
/// Windows cannot open a directory as a file, and its file systems journal
/// directory updates themselves, so there this is a no-op.
fn sync_directory(path: &Path) -> Result<(), SnapshotError> {
    if cfg!(windows) {
        return Ok(());
    }
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(io_error(path))
}

/// Renames the checked database at `import_path` to `target`.
fn install(import_path: &Path, target: &Path) -> Result<(), SnapshotError> {
    let parent = target
        .parent()
        .ok_or_else(|| SnapshotError::Unsupported(format!("no directory for {target:?}")))?;
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    if target.exists() {
        return Err(SnapshotError::Unsupported(format!(
            "a state database already exists at {target:?}"
        )));
    }
    fs::rename(import_path, target).map_err(io_error(target))?;
    sync_directory(parent)
}
