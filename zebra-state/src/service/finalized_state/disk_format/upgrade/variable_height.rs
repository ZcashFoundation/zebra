//! Migrate every fixed-width block-height field to the canonical variable-width encoding.

#![allow(clippy::unwrap_in_result)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, TryRecvError};
use rocksdb::{IteratorMode, ReadOptions, WriteBatch};
use semver::Version;
use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    block::{self, Height},
    common::atomic_write,
    state_snapshot::ObjectHash,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DB},
        disk_format::{
            transparent::{AddressBalanceLocationChange, OutputLocation},
            upgrade::DbFormatChange,
            IntoDisk,
        },
        ZebraDb,
    },
    BoxError, HistoryCapabilities, SnapshotBase, StateInitError, TransactionLocation,
};

use super::{CancelFormatChange, DiskFormatUpgrade};

const MIGRATION_BATCH_RECORDS: usize = 1_000;
const LEGACY_HEIGHT_DISK_BYTES: usize = 3;
const LEGACY_TRANSACTION_LOCATION_DISK_BYTES: usize = 5;
const LEGACY_OUTPUT_LOCATION_DISK_BYTES: usize = 8;
const LEGACY_ADDRESS_TRANSACTION_DISK_BYTES: usize = 13;
const LEGACY_ADDRESS_UNSPENT_OUTPUT_DISK_BYTES: usize = 16;
const LEGACY_SUBTREE_DISK_BYTES: usize = 35;
const LEGACY_SNAPSHOT_BASE_DISK_BYTES: usize = 2 * LEGACY_HEIGHT_DISK_BYTES + 32 + 32 + 1;
const MIGRATION_METADATA_FILE_NAME: &str = "zebra-fixed-height-migration";
static NEXT_MIGRATION_OPERATION: AtomicU64 = AtomicU64::new(0);

/// Implements the v33 variable-width height migration.
pub struct Upgrade;

#[derive(Clone, Copy)]
enum Part {
    Unchanged,
    Height,
    HeightOrEmpty,
    TransactionLocation,
    OutputLocation,
    AddressBalance,
    AddressTransaction,
    AddressUnspentOutput,
    OptionalTransactionLocation,
    Subtree,
    SnapshotBase,
}

struct Family {
    name: &'static str,
    key: Part,
    value: Part,
}

const AFFECTED_FAMILIES: &[Family] = &[
    Family {
        name: "hash_by_height",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "height_by_hash",
        key: Part::Unchanged,
        value: Part::Height,
    },
    Family {
        name: "block_header_by_height",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "tx_by_loc",
        key: Part::TransactionLocation,
        value: Part::Unchanged,
    },
    Family {
        name: "hash_by_tx_loc",
        key: Part::TransactionLocation,
        value: Part::Unchanged,
    },
    Family {
        name: "tx_loc_by_hash",
        key: Part::Unchanged,
        value: Part::TransactionLocation,
    },
    Family {
        name: "balance_by_transparent_addr",
        key: Part::Unchanged,
        value: Part::AddressBalance,
    },
    Family {
        name: "tx_loc_by_transparent_addr_loc",
        key: Part::AddressTransaction,
        value: Part::Unchanged,
    },
    Family {
        name: "utxo_by_out_loc",
        key: Part::OutputLocation,
        value: Part::Unchanged,
    },
    Family {
        name: "utxo_loc_by_transparent_addr_loc",
        key: Part::AddressUnspentOutput,
        value: Part::Unchanged,
    },
    Family {
        name: "tx_loc_by_spent_out_loc",
        key: Part::OutputLocation,
        value: Part::TransactionLocation,
    },
    Family {
        name: "sprout_nullifiers",
        key: Part::Unchanged,
        value: Part::OptionalTransactionLocation,
    },
    Family {
        name: "sprout_note_commitment_tree",
        key: Part::HeightOrEmpty,
        value: Part::Unchanged,
    },
    Family {
        name: "sapling_nullifiers",
        key: Part::Unchanged,
        value: Part::OptionalTransactionLocation,
    },
    Family {
        name: "sapling_note_commitment_tree",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "sapling_note_commitment_subtree",
        key: Part::Unchanged,
        value: Part::Subtree,
    },
    Family {
        name: "orchard_nullifiers",
        key: Part::Unchanged,
        value: Part::OptionalTransactionLocation,
    },
    Family {
        name: "orchard_note_commitment_tree",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "orchard_note_commitment_subtree",
        key: Part::Unchanged,
        value: Part::Subtree,
    },
    Family {
        name: "ironwood_nullifiers",
        key: Part::Unchanged,
        value: Part::OptionalTransactionLocation,
    },
    Family {
        name: "ironwood_note_commitment_tree",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "ironwood_note_commitment_subtree",
        key: Part::Unchanged,
        value: Part::Subtree,
    },
    Family {
        name: "history_tree",
        key: Part::HeightOrEmpty,
        value: Part::Unchanged,
    },
    Family {
        name: "block_info",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "snapshot_base",
        key: Part::Unchanged,
        value: Part::SnapshotBase,
    },
    // The checkpoint sync column families. The `spentness_artifact` record stores its height as a
    // fixed four-byte integer, not as a disk `Height`, so it needs no rewrite.
    Family {
        name: "sync_meta_by_height",
        key: Part::Height,
        value: Part::Unchanged,
    },
    Family {
        name: "spentness_journal",
        key: Part::OutputLocation,
        value: Part::Unchanged,
    },
];

#[derive(Clone, Copy)]
enum Pass {
    WriteConverted,
    DeleteLegacy,
}

impl DiskFormatUpgrade for Upgrade {
    fn version(&self) -> Version {
        Version::new(33, 0, 0)
    }

    fn description(&self) -> &'static str {
        "encode block heights with a canonical variable width"
    }

    fn run(
        &self,
        _initial_tip_height: Height,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        if !checkpoint_paths(db).0.exists() {
            // The synchronous open path rewrites the records and removes the checkpoint before it
            // runs the pending upgrade list, so that older migrations read the records with the
            // current codec. By the time the list reaches v33, nothing is left to rewrite.
            assert!(
                current_height_markers_exist(db.disk_db().inner(), db.path())
                    .expect("the live database must be readable during its migration")
                    || db.finalized_tip_height().is_none(),
                "the v33 rewrite needs its checkpoint until every record is converted"
            );
            return Ok(());
        }

        let source = open_source(db)
            .expect("the fixed-width checkpoint must be readable during its migration");
        let live = db.disk_db().inner();

        for family in AFFECTED_FAMILIES {
            tracing::info!(
                column_family = family.name,
                "migrating fixed-width height records"
            );
            for pass in [
                Pass::WriteConverted,
                Pass::DeleteLegacy,
                Pass::WriteConverted,
            ] {
                rewrite_pass(live, &source, family, pass, cancel_receiver)?;
            }
        }

        Ok(())
    }

    fn validate(
        &self,
        db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        if !checkpoint_paths(db).0.exists() {
            return Ok(Ok(()));
        }
        let source = open_source(db)
            .expect("the fixed-width checkpoint must be readable during migration validation");
        let live = db.disk_db().inner();

        for family in AFFECTED_FAMILIES {
            if let Err(error) = validate_family(live, &source, family, cancel_receiver)? {
                return Ok(Err(format!("{}: {error}", family.name)));
            }
        }

        Ok(Ok(()))
    }

    fn requires_full_history(&self) -> bool {
        false
    }
}

/// Returns whether this format change includes the v33 height rewrite.
pub(crate) fn is_required(change: &DbFormatChange) -> bool {
    let DbFormatChange::Upgrade {
        older_disk_version,
        newer_running_version,
    } = change
    else {
        return false;
    };
    let v33 = Version::new(33, 0, 0);

    older_disk_version < &v33 && newer_running_version >= &v33
}

fn migration_metadata_path(db: &ZebraDb) -> PathBuf {
    migration_metadata_path_for(db.path())
}

fn migration_metadata_path_for(path: &Path) -> PathBuf {
    checkpoint_paths_for(path).0.with_extension("migration")
}

/// Returns whether the database at `path` has an unfinished v33 rewrite: its checkpoint and the
/// live migration metadata that names it both exist.
///
/// A database with an unfinished rewrite can already hold current-format records in the column
/// families converted before it stopped, so those records don't show that the rewrite is done.
pub(crate) fn rewrite_in_progress(path: &Path) -> bool {
    checkpoint_paths_for(path).0.exists() && migration_metadata_path_for(path).exists()
}

/// Rewrites every affected record from the fixed-width checkpoint that [`prepare`] made, checks the
/// result, and removes the checkpoint.
///
/// The open path calls this before any other pending upgrade, so that older migrations, such as
/// the 29.1 synchronization metadata backfill, read and write the records with the current codec.
/// After this returns, the live database holds only current-format records, and the v33 entry in
/// the upgrade list has nothing left to rewrite.
pub(crate) fn rewrite(db: &ZebraDb) {
    let (_never_cancel_sender, cancel_receiver) = crossbeam_channel::bounded(1);
    Upgrade
        .run(Height::MIN, db, &cancel_receiver)
        .expect("the synchronous variable-height rewrite cannot be cancelled");
    Upgrade
        .validate(db, &cancel_receiver)
        .expect("the synchronous variable-height rewrite cannot be cancelled")
        .expect("the variable-height rewrite must convert every record");
    finish(db);
}

fn new_migration_operation() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("migration operation timestamps must be after the Unix epoch")
        .as_nanos();
    let sequence = NEXT_MIGRATION_OPERATION.fetch_add(1, Ordering::Relaxed);
    format!("{timestamp}-{}-{sequence}", std::process::id())
}

fn write_migration_metadata(path: PathBuf, operation: &str) -> Result<(), BoxError> {
    atomic_write(path, operation.as_bytes())??;
    Ok(())
}

fn check_migration_metadata(db: &ZebraDb, checkpoint: &Path) -> Result<(), StateInitError> {
    let invalid = |source: BoxError| StateInitError::HistoryRead {
        path: db.path().to_path_buf(),
        source,
    };
    let live_operation = fs::read_to_string(migration_metadata_path(db))
        .map_err(|source| invalid(Box::new(source)))?;
    let checkpoint_operation = fs::read_to_string(checkpoint.join(MIGRATION_METADATA_FILE_NAME))
        .map_err(|source| invalid(Box::new(source)))?;
    if live_operation.is_empty() || live_operation != checkpoint_operation {
        return Err(invalid(
            "migration checkpoint operation metadata is missing or does not match the active \
             migration; remove only the derived checkpoint before restarting the migration"
                .into(),
        ));
    }

    Ok(())
}

/// Creates or reopens the immutable checkpoint, checks pruned-state settings, and reads its tip.
///
/// The checkpoint is renamed into place only after RocksDB finishes it. If checkpoint creation is
/// interrupted, no records have been rewritten, so a stale temporary checkpoint can be discarded.
pub(crate) fn prepare(db: &ZebraDb) -> Result<Option<Height>, StateInitError> {
    let (checkpoint, temporary) = checkpoint_paths(db);
    if !checkpoint.exists() {
        remove_directory(&temporary);
        db.disk_db()
            .create_checkpoint(&temporary)
            .expect("creating the fixed-width migration checkpoint must succeed");
        let operation = new_migration_operation();
        write_migration_metadata(migration_metadata_path(db), &operation)
            .and_then(|()| {
                write_migration_metadata(temporary.join(MIGRATION_METADATA_FILE_NAME), &operation)
            })
            .map_err(|source| StateInitError::HistoryRead {
                path: db.path().to_path_buf(),
                source,
            })?;
        fs::rename(&temporary, &checkpoint)
            .expect("a completed migration checkpoint must be renameable into place");
    }

    let source = open_source(db)?;
    let history = legacy_history_capabilities(db, &source)?;
    if db.config().prune_blocks_older_than.is_none() && history.is_pruned() {
        return Err(StateInitError::PrunedDatabase {
            path: db.path().to_path_buf(),
            lowest_retained_height: history.lowest_retained_height,
        });
    }
    if history.is_pruned() {
        let disk_version = db
            .format_version_on_disk()
            .map_err(|source| StateInitError::HistoryRead {
                path: db.path().to_path_buf(),
                source,
            })?
            .ok_or_else(|| StateInitError::HistoryRead {
                path: db.path().to_path_buf(),
                source: "pruned state requires a stored format version".into(),
            })?;
        let disk_indexer = disk_version.build.contains("indexer");
        let running_indexer = cfg!(feature = "indexer");
        if disk_indexer != running_indexer {
            return Err(StateInitError::PrunedIndexModeMismatch {
                path: db.path().to_path_buf(),
                disk_indexer,
                running_indexer,
            });
        }
    }

    legacy_tip(db, &source)
}

/// Removes the migration checkpoint once every record is converted.
///
/// The live metadata goes first: a checkpoint without it is the leftover of an interrupted
/// removal, not an unfinished rewrite (see [`rewrite_in_progress`]).
pub(crate) fn finish(db: &ZebraDb) {
    let (checkpoint, temporary) = checkpoint_paths(db);
    remove_file(&migration_metadata_path(db));
    remove_directory(&checkpoint);
    remove_directory(&temporary);
}

fn checkpoint_paths(db: &ZebraDb) -> (PathBuf, PathBuf) {
    checkpoint_paths_for(db.path())
}

fn checkpoint_paths_for(path: &Path) -> (PathBuf, PathBuf) {
    let network_directory = path
        .file_name()
        .expect("a state database path always ends in its network directory")
        .to_string_lossy();
    let checkpoint = path.with_file_name(format!("{network_directory}.fixed-height-checkpoint"));
    let temporary = checkpoint.with_extension("tmp");

    (checkpoint, temporary)
}

fn remove_directory(path: &PathBuf) {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("removing migration checkpoint {path:?} must succeed: {error}"),
    }
}

fn remove_file(path: &PathBuf) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("removing migration metadata {path:?} must succeed: {error}"),
    }
}

fn open_source(db: &ZebraDb) -> Result<DB, StateInitError> {
    let (checkpoint, _) = checkpoint_paths(db);
    check_migration_metadata(db, &checkpoint)?;
    let source =
        DiskDb::open_read_only(&checkpoint).map_err(|source| StateInitError::HistoryRead {
            path: db.path().to_path_buf(),
            source: Box::new(source),
        })?;
    let invalid = |source: BoxError| StateInitError::HistoryRead {
        path: db.path().to_path_buf(),
        source,
    };
    let live_identity = db
        .disk_db()
        .inner()
        .get_db_identity()
        .map_err(|error| invalid(error.into()))?;
    let checkpoint_identity = source
        .get_db_identity()
        .map_err(|error| invalid(error.into()))?;
    if live_identity != checkpoint_identity {
        return Err(invalid(
            "migration checkpoint database identity does not match the active state; \
             remove only the derived checkpoint before restarting the migration"
                .into(),
        ));
    }

    if current_height_markers_exist(&source, db.path())? {
        return Err(invalid(
            "migration source contains variable-height records; restore its format version \
             instead of running the legacy migration again"
                .into(),
        ));
    }

    Ok(source)
}

pub(crate) fn current_height_markers_exist(db: &DB, path: &Path) -> Result<bool, StateInitError> {
    let Some(hashes) = db.cf_handle("hash_by_height") else {
        return Ok(false);
    };
    for height in [Height::MIN, Height(1)] {
        if db
            .get_pinned_cf(&hashes, height.as_bytes())
            .map_err(|source| StateInitError::HistoryRead {
                path: path.to_path_buf(),
                source: Box::new(source),
            })?
            .is_some()
        {
            return Ok(true);
        }
    }

    let mut heights = db.raw_iterator_cf(&hashes);
    heights.seek_to_first();
    if heights
        .key()
        .is_some_and(|key| key.len() != LEGACY_HEIGHT_DISK_BYTES)
    {
        return Ok(true);
    }
    heights
        .status()
        .map_err(|source| StateInitError::HistoryRead {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;

    Ok(false)
}

fn legacy_tip(db: &ZebraDb, source: &DB) -> Result<Option<Height>, StateInitError> {
    let Some(hashes) = source.cf_handle("hash_by_height") else {
        return Ok(None);
    };
    let mut iterator = source.raw_iterator_cf(&hashes);
    iterator.seek_to_last();
    iterator
        .status()
        .map_err(|source| StateInitError::HistoryRead {
            path: db.path().to_path_buf(),
            source: Box::new(source),
        })?;

    iterator
        .key()
        .map(|key| {
            decode_legacy_height(key).map_err(|source| StateInitError::HistoryRead {
                path: db.path().to_path_buf(),
                source,
            })
        })
        .transpose()
}

fn legacy_history_capabilities(
    db: &ZebraDb,
    source: &DB,
) -> Result<HistoryCapabilities, StateInitError> {
    let invalid = |source: BoxError| StateInitError::HistoryRead {
        path: db.path().to_path_buf(),
        source,
    };
    let snapshot_base = legacy_snapshot_base(source).map_err(invalid)?;
    let Some(index) = source.cf_handle("hash_by_tx_loc") else {
        return Ok(HistoryCapabilities::default());
    };
    let first = legacy_transaction_location(Height(1));
    if snapshot_base.is_none()
        && source
            .get_pinned_cf(&index, &first)
            .map_err(|error| invalid(error.into()))?
            .is_none()
    {
        return Ok(HistoryCapabilities::default());
    }
    let transactions = source
        .cf_handle("tx_by_loc")
        .ok_or_else(|| invalid("indexed blocks have no transaction column family".into()))?;
    let mut bodies = source.raw_iterator_cf(&transactions);
    bodies.seek(&first);
    bodies.status().map_err(|error| invalid(error.into()))?;
    let location = bodies
        .key()
        .ok_or_else(|| invalid("indexed blocks have no valid retained body suffix".into()))?;
    let (height, index) = decode_legacy_transaction_location(location)
        .map_err(|error| invalid(error.to_string().into()))?;
    if index != 0 {
        return Err(invalid(
            "the first retained body is missing its coinbase".into(),
        ));
    }

    if let Some(base) = snapshot_base {
        return Ok(HistoryCapabilities {
            lowest_retained_height: height.max(base.window_start),
            lowest_known_height: base.window_start,
            lowest_header_height: legacy_lowest_header_height(source, &base)
                .map_err(|error| invalid(error.into()))?,
            snapshot_base: Some(base),
        });
    }

    Ok(HistoryCapabilities {
        lowest_retained_height: if height == Height(1) {
            Height::MIN
        } else {
            height
        },
        lowest_known_height: Height::MIN,
        lowest_header_height: Height::MIN,
        snapshot_base: None,
    })
}

/// Returns the first non-genesis height whose header the legacy snapshot database `source`
/// holds, like `DiskDb::lowest_header_height` does for the current format.
fn legacy_lowest_header_height(source: &DB, base: &SnapshotBase) -> Result<Height, rocksdb::Error> {
    let Some(below_window) = base.window_start.previous().ok().filter(|h| !h.is_min()) else {
        return Ok(Height::MIN);
    };
    let Some(headers) = source.cf_handle("header_by_height") else {
        return Ok(base.window_start);
    };
    for height in [Height(1), below_window] {
        if source
            .get_pinned_cf(&headers, legacy_height_bytes(height))?
            .is_none()
        {
            return Ok(base.window_start);
        }
    }

    Ok(Height::MIN)
}

fn legacy_snapshot_base(source: &DB) -> Result<Option<SnapshotBase>, BoxError> {
    let Some(marker) = source.cf_handle("snapshot_base") else {
        return Ok(None);
    };
    let Some(bytes) = source.get_pinned_cf(&marker, ().as_bytes())? else {
        return Ok(None);
    };
    if bytes.len() != LEGACY_SNAPSHOT_BASE_DISK_BYTES {
        return Err("the legacy snapshot base marker has an invalid length".into());
    }
    let window_start = decode_legacy_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES])?;
    let height =
        decode_legacy_height(&bytes[LEGACY_HEIGHT_DISK_BYTES..2 * LEGACY_HEIGHT_DISK_BYTES])?;
    let base = SnapshotBase {
        window_start,
        height,
        block_hash: block::Hash(bytes[6..38].try_into().expect("32 bytes were sliced")),
        manifest: ObjectHash(bytes[38..70].try_into().expect("32 bytes were sliced")),
        deferred_checks: bytes[70],
    };
    if base.window_start == Height::MIN || base.window_start > base.height {
        return Err("the legacy snapshot base marker has invalid heights".into());
    }

    let hashes = source
        .cf_handle("hash_by_height")
        .ok_or("snapshot databases have a block hash column family")?;
    let stored = source
        .get_pinned_cf(&hashes, legacy_height_bytes(base.height))?
        .filter(|hash| hash.len() == 32)
        .map(|hash| block::Hash(hash.as_ref().try_into().expect("32 bytes were filtered")));
    if stored != Some(base.block_hash) {
        return Err("the legacy snapshot base marker's block hash does not match".into());
    }

    Ok(Some(base))
}

fn legacy_height_bytes(height: Height) -> Vec<u8> {
    assert!(
        height.0 <= 0x00ff_ffff,
        "legacy snapshot markers contain three-byte heights"
    );
    height.0.to_be_bytes()[1..].to_vec()
}

/// Merges fixed-width address balances while the live database and its checkpoint are open.
///
/// Both inputs and outputs stay in the legacy format, so RocksDB never decodes legacy operands with
/// the variable-width merge operator. Converted records are written as puts, which hide their older
/// merge operands, and normal v33 block writes use puts rather than merge operations.
pub(crate) fn fetch_add_legacy_balance_and_received(
    _: &[u8],
    existing_val: Option<&[u8]>,
    operands: &rocksdb::MergeOperands,
) -> Option<Vec<u8>> {
    existing_val
        .into_iter()
        .chain(operands)
        .map(legacy_address_balance_change)
        .reduce(|a, b| (a + b).expect("address balance/received should not overflow"))
        .map(|change| legacy_address_balance(&change))
}

fn legacy_address_balance_change(bytes: &[u8]) -> AddressBalanceLocationChange {
    assert!(
        bytes.len() == 16 || bytes.len() == 24,
        "legacy address balances contain 16 or 24 bytes"
    );
    let balance = Amount::<NegativeAllowed>::from_bytes(
        bytes[..8]
            .try_into()
            .expect("legacy address balances contain an eight-byte amount"),
    )
    .expect("stored address balances contain a valid amount");
    let location = decode_legacy_output_location(&bytes[8..16]);
    let received = if bytes.len() == 24 {
        u64::from_le_bytes(
            bytes[16..]
                .try_into()
                .expect("a 24-byte legacy address balance contains eight received bytes"),
        )
    } else {
        0
    };
    let mut change = AddressBalanceLocationChange::new(location);
    *change.balance_mut() = balance;
    *change.received_mut() = received;

    change
}

fn legacy_address_balance(change: &AddressBalanceLocationChange) -> Vec<u8> {
    [
        change.balance().as_bytes().to_vec(),
        legacy_output_location(change.address_location()),
        change.received().to_le_bytes().to_vec(),
    ]
    .concat()
}

fn decode_legacy_output_location(bytes: &[u8]) -> OutputLocation {
    require_len(bytes, LEGACY_OUTPUT_LOCATION_DISK_BYTES);
    let (height, transaction_index) =
        decode_legacy_transaction_location(&bytes[..LEGACY_TRANSACTION_LOCATION_DISK_BYTES])
            .expect("legacy output locations contain a valid transaction location");
    let output_index = u32::from_be_bytes([
        0,
        bytes[LEGACY_TRANSACTION_LOCATION_DISK_BYTES],
        bytes[LEGACY_TRANSACTION_LOCATION_DISK_BYTES + 1],
        bytes[LEGACY_TRANSACTION_LOCATION_DISK_BYTES + 2],
    ]);

    OutputLocation::from_output_index(
        TransactionLocation::from_index(height, transaction_index),
        output_index,
    )
}

fn legacy_output_location(location: OutputLocation) -> Vec<u8> {
    assert!(
        location.height().0 <= 0x00ff_ffff,
        "legacy address locations contain three-byte heights"
    );
    let height = location.height().0.to_be_bytes();
    [
        height[1..].to_vec(),
        location.transaction_index().as_bytes().to_vec(),
        location.output_index().as_bytes().to_vec(),
    ]
    .concat()
}

fn rewrite_pass(
    live: &DB,
    source: &DB,
    family: &Family,
    pass: Pass,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<(), CancelFormatChange> {
    let live_cf = live
        .cf_handle(family.name)
        .expect("every affected column family is opened with current state");
    let source_cf = source
        .cf_handle(family.name)
        .expect("every affected column family exists in the fixed-width checkpoint");
    let iterator = source.iterator_cf_opt(&source_cf, ReadOptions::default(), IteratorMode::Start);
    let mut batch = WriteBatch::default();
    let mut records = 0_usize;

    for item in iterator {
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }
        let (key, value) = item.expect("reading the fixed-width migration checkpoint must succeed");
        match pass {
            Pass::WriteConverted => {
                let (new_key, new_value) = convert_record(family, &key, &value);
                batch.put_cf(&live_cf, new_key, new_value);
            }
            Pass::DeleteLegacy => batch.delete_cf(&live_cf, key),
        }
        records += 1;

        if records % MIGRATION_BATCH_RECORDS == 0 {
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }
            live.write(std::mem::take(&mut batch))
                .expect("writing a variable-height migration batch must succeed");
        }
    }

    if records % MIGRATION_BATCH_RECORDS != 0 {
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }
        live.write(batch)
            .expect("writing the final variable-height migration batch must succeed");
    }

    Ok(())
}

fn validate_family(
    live: &DB,
    source: &DB,
    family: &Family,
    cancel_receiver: &Receiver<CancelFormatChange>,
) -> Result<Result<(), String>, CancelFormatChange> {
    let live_cf = live
        .cf_handle(family.name)
        .expect("every affected column family is opened with current state");
    let source_cf = source
        .cf_handle(family.name)
        .expect("every affected column family exists in the fixed-width checkpoint");
    let mut source_count = 0_usize;

    for item in source.iterator_cf_opt(&source_cf, ReadOptions::default(), IteratorMode::Start) {
        if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
            return Err(CancelFormatChange);
        }
        let (key, value) = item.expect("reading the fixed-width migration checkpoint must succeed");
        let (new_key, new_value) = convert_record(family, &key, &value);
        let actual = live
            .get_pinned_cf(&live_cf, &new_key)
            .expect("reading migrated records must succeed");
        if actual.as_deref() != Some(new_value.as_slice()) {
            return Ok(Err(format!(
                "converted record is missing or has the wrong value at key {new_key:?}"
            )));
        }
        source_count += 1;
    }

    let mut live_count = 0_usize;
    for item in live.iterator_cf_opt(&live_cf, ReadOptions::default(), IteratorMode::Start) {
        item.expect("iterating migrated records must succeed");
        live_count += 1;
        if live_count % MIGRATION_BATCH_RECORDS == 0
            && !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty))
        {
            return Err(CancelFormatChange);
        }
    }
    if live_count != source_count {
        return Ok(Err(format!(
            "record count differs: the fixed-width source has {source_count}, the live database has {live_count}"
        )));
    }

    Ok(Ok(()))
}

fn convert_record(family: &Family, key: &[u8], value: &[u8]) -> (Vec<u8>, Vec<u8>) {
    (
        convert_part(family.key, key),
        convert_part(family.value, value),
    )
}

fn convert_part(part: Part, bytes: &[u8]) -> Vec<u8> {
    match part {
        Part::Unchanged => bytes.to_vec(),
        Part::Height => convert_height(bytes),
        Part::HeightOrEmpty if bytes.is_empty() => Vec::new(),
        Part::HeightOrEmpty => convert_height(bytes),
        Part::TransactionLocation => {
            require_len(bytes, LEGACY_TRANSACTION_LOCATION_DISK_BYTES);
            [
                convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
                bytes[LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
            ]
            .concat()
        }
        Part::OutputLocation => {
            require_len(bytes, LEGACY_OUTPUT_LOCATION_DISK_BYTES);
            [
                convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
                bytes[LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
            ]
            .concat()
        }
        Part::AddressBalance => {
            assert!(
                bytes.len() == 16 || bytes.len() == 24,
                "legacy address balances contain 16 or 24 bytes"
            );
            [
                bytes[..8].to_vec(),
                convert_output_location(&bytes[8..16]),
                bytes[16..].to_vec(),
            ]
            .concat()
        }
        Part::AddressTransaction => {
            require_len(bytes, LEGACY_ADDRESS_TRANSACTION_DISK_BYTES);
            [
                convert_output_location(&bytes[..LEGACY_OUTPUT_LOCATION_DISK_BYTES]),
                convert_transaction_location(&bytes[LEGACY_OUTPUT_LOCATION_DISK_BYTES..]),
            ]
            .concat()
        }
        Part::AddressUnspentOutput => {
            require_len(bytes, LEGACY_ADDRESS_UNSPENT_OUTPUT_DISK_BYTES);
            [
                convert_output_location(&bytes[..LEGACY_OUTPUT_LOCATION_DISK_BYTES]),
                convert_output_location(&bytes[LEGACY_OUTPUT_LOCATION_DISK_BYTES..]),
            ]
            .concat()
        }
        Part::OptionalTransactionLocation if bytes.is_empty() => Vec::new(),
        Part::OptionalTransactionLocation => convert_transaction_location(bytes),
        Part::Subtree => {
            require_len(bytes, LEGACY_SUBTREE_DISK_BYTES);
            [
                convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
                bytes[LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
            ]
            .concat()
        }
        Part::SnapshotBase => {
            require_len(bytes, LEGACY_SNAPSHOT_BASE_DISK_BYTES);
            [
                convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
                convert_height(&bytes[LEGACY_HEIGHT_DISK_BYTES..2 * LEGACY_HEIGHT_DISK_BYTES]),
                bytes[2 * LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
            ]
            .concat()
        }
    }
}

fn convert_height(bytes: &[u8]) -> Vec<u8> {
    decode_legacy_height(bytes)
        .expect("legacy heights contain exactly three bytes")
        .as_bytes()
}

fn convert_transaction_location(bytes: &[u8]) -> Vec<u8> {
    require_len(bytes, LEGACY_TRANSACTION_LOCATION_DISK_BYTES);
    [
        convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
        bytes[LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
    ]
    .concat()
}

fn convert_output_location(bytes: &[u8]) -> Vec<u8> {
    require_len(bytes, LEGACY_OUTPUT_LOCATION_DISK_BYTES);
    [
        convert_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES]),
        bytes[LEGACY_HEIGHT_DISK_BYTES..].to_vec(),
    ]
    .concat()
}

fn decode_legacy_height(bytes: &[u8]) -> Result<Height, BoxError> {
    let bytes: [u8; LEGACY_HEIGHT_DISK_BYTES] = bytes
        .try_into()
        .map_err(|_| "legacy heights contain exactly three bytes")?;
    Ok(Height(u32::from_be_bytes([
        0, bytes[0], bytes[1], bytes[2],
    ])))
}

fn legacy_transaction_location(height: Height) -> Vec<u8> {
    let height = height.0.to_be_bytes();
    [height[1..].to_vec(), vec![0, 0]].concat()
}

fn decode_legacy_transaction_location(bytes: &[u8]) -> Result<(Height, u16), &'static str> {
    if bytes.len() != LEGACY_TRANSACTION_LOCATION_DISK_BYTES {
        return Err("legacy transaction locations contain exactly five bytes");
    }
    let height = decode_legacy_height(&bytes[..LEGACY_HEIGHT_DISK_BYTES])
        .map_err(|_| "legacy transaction locations contain a three-byte height")?;
    let index = u16::from_be_bytes(
        bytes[LEGACY_HEIGHT_DISK_BYTES..]
            .try_into()
            .map_err(|_| "legacy transaction locations contain a two-byte index")?,
    );

    Ok((height, index))
}

fn require_len(bytes: &[u8], expected: usize) {
    assert_eq!(
        bytes.len(),
        expected,
        "invalid legacy fixed-width record length"
    );
}

#[cfg(test)]
mod tests;
