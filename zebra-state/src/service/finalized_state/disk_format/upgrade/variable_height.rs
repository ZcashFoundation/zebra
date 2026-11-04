//! Migrate every fixed-width block-height field to the canonical variable-width encoding.

#![allow(clippy::unwrap_in_result)]

use std::{fs, path::PathBuf};

use crossbeam_channel::{Receiver, TryRecvError};
use rocksdb::{IteratorMode, ReadOptions, WriteBatch};
use semver::Version;
use zebra_chain::{
    block::{self, Height},
    state_snapshot::ObjectHash,
};

use crate::{
    service::finalized_state::{
        disk_db::{DiskDb, DB},
        disk_format::{upgrade::DbFormatChange, IntoDisk},
        ZebraDb,
    },
    BoxError, HistoryCapabilities, SnapshotBase, StateInitError,
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

/// Removes migration checkpoints after the version file records a complete v33 upgrade.
pub(crate) fn finish(db: &ZebraDb) {
    let (checkpoint, temporary) = checkpoint_paths(db);
    remove_directory(&checkpoint);
    remove_directory(&temporary);
}

fn checkpoint_paths(db: &ZebraDb) -> (PathBuf, PathBuf) {
    let path = db.path();
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

fn open_source(db: &ZebraDb) -> Result<DB, StateInitError> {
    let (checkpoint, _) = checkpoint_paths(db);
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

    Ok(source)
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
        snapshot_base: None,
    })
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
