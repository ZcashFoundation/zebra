//! Tool-only comparison of the rows selected for a snapshot, not whole databases.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use rocksdb::{DBRawIteratorWithThreadMode, DBWithThreadMode, ReadOptions, SingleThreaded};
use semver::Version;
use serde::Serialize;

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
    serialization::ZcashDeserialize,
};

use crate::{
    config::database_format_version_at_path,
    constants::{
        state_database_format_version_in_code, DATABASE_FORMAT_VERSION_FILE_NAME,
        STATE_DATABASE_KIND,
    },
    service::finalized_state::{
        ZebraDb, HEADER_BY_HEIGHT, HEIGHT_DISK_BYTES, TRANSACTION_LOCATION_DISK_BYTES,
    },
    BoxError, Config, FromDisk, IntoDisk,
};

use super::{
    format::{canonical_value, window_start, ComponentId},
    SnapshotBase,
};

/// The matching snapshot-selected state, read independently from the imported database.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ComparisonSummary {
    /// The common finalized height.
    pub height: Height,
    /// The common finalized block hash in display-order hexadecimal.
    pub block_hash: String,
    /// Selected entry counts, keyed by snapshot component column family.
    pub component_entries: BTreeMap<&'static str, u64>,
    /// The imported Sprout tip root in hexadecimal.
    pub sprout_root: String,
    /// The imported Sapling tip root in hexadecimal.
    pub sapling_root: String,
    /// The imported Orchard tip root in hexadecimal.
    pub orchard_root: String,
    /// The imported Ironwood tip root in hexadecimal.
    pub ironwood_root: String,
    /// The imported history root in display-order hexadecimal, absent before activation.
    pub history_root: Option<String>,
}

/// Independently opens and compares two states' snapshot-selected rows.
///
/// The expected state is a read-only secondary and may have a live primary, but
/// callers must keep that source unchanged during comparison. The imported state
/// must be stopped: this function owns its exclusive RocksDB primary lock while
/// checking that the stored snapshot marker exactly matches `expected_base`
/// and until every selected row and summary field has been read.
///
/// Both states must already exist in the current format, at distinct paths even
/// after resolving aliases. Missing states are never created and neither state is
/// migrated. Both finalized tips must have the same height and hash. Header-only
/// context above that height is ignored. Canonical snapshot encodings are compared,
/// so cached tree roots do not cause spurious differences.
pub fn compare_states(
    expected: &Config,
    imported: &Config,
    network: &Network,
    expected_base: SnapshotBase,
) -> Result<ComparisonSummary, BoxError> {
    let version = state_database_format_version_in_code();
    let expected_path = existing_state_path(expected, network, &version, "expected")?;
    let imported_path = existing_state_path(imported, network, &version, "imported")?;
    let same_state = expected_path == imported_path;
    #[cfg(unix)]
    let same_state = {
        use std::os::unix::fs::MetadataExt;

        // Also reject filesystem aliases such as bind mounts, not only symlinks.
        let expected = expected_path.metadata()?;
        let imported = imported_path.metadata()?;
        same_state || (expected.dev(), expected.ino()) == (imported.dev(), imported.ino())
    };
    if same_state {
        return Err("expected and imported states must not be the same database".into());
    }

    let imported =
        ZebraDb::open_for_snapshot_comparison(imported, network, &imported_path, &version, None)
            .map_err(|error| {
                format!(
                    "cannot exclusively open imported state {}: {error}; stop its primary before comparison",
                    imported_path.display()
                )
            })?;
    let actual_base = imported
        .checked_snapshot_base()
        .map_err(|error| format!("cannot read imported state snapshot base marker: {error}"))?
        .ok_or("imported state is missing its snapshot base marker")?;
    if actual_base != expected_base {
        return Err(format!(
            "imported state snapshot base marker mismatch: expected {expected_base:?}, found {actual_base:?}"
        )
        .into());
    }

    let secondary = tempfile::tempdir()?;
    let expected = open_snapshot_source_at_path(
        expected,
        network,
        &expected_path,
        &version,
        secondary.path(),
    )?;
    compare_databases(&expected, &imported)
}

/// Checks a prepared source's finalized tip and checkpoint headers without
/// creating or migrating its database. Read and decoding errors are returned.
pub fn validate_snapshot_source(
    config: &Config,
    network: &Network,
    expected_tip: (Height, block::Hash),
    checkpoints: &[(Height, block::Hash)],
) -> Result<(), BoxError> {
    let secondary = tempfile::tempdir()?;
    let db = open_snapshot_source(config, network, secondary.path())?;
    let tip = checked_tip(&db)?;
    if tip != expected_tip {
        return Err(format!(
            "prepared source tip {tip:?} does not match resolved expected_tip {expected_tip:?}"
        )
        .into());
    }
    for &(height, hash) in checkpoints {
        let family = if height <= tip.0 {
            ComponentId::Headers.column_family()
        } else {
            HEADER_BY_HEIGHT
        };
        let key = height.as_bytes();
        let mut rows = open_family(&db, family)?;
        rows.seek(key);
        rows.status()?;
        let Some((_, mut bytes)) = rows.item().filter(|(stored, _)| *stored == key) else {
            return Err(format!("prepared source header at height {} is missing", height.0).into());
        };
        let header = block::Header::zcash_deserialize(&mut bytes).map_err(|error| {
            format!(
                "invalid prepared source header at height {}: {error}",
                height.0
            )
        })?;
        if !bytes.is_empty() || header.hash() != hash {
            return Err(format!(
                "prepared source header at height {} differs from its resolved checkpoint",
                height.0
            )
            .into());
        }
    }
    Ok(())
}

fn checked_tip(db: &ZebraDb) -> Result<(Height, block::Hash), BoxError> {
    let mut rows = open_family(db, "hash_by_height")?;
    rows.seek_to_last();
    rows.status()?;
    let (key, hash) = rows.item().ok_or("state has no finalized tip")?;
    if key.len() != HEIGHT_DISK_BYTES {
        return Err("invalid finalized tip height key".into());
    }
    let hash = block::Hash(hash.try_into().map_err(|_| "invalid finalized tip hash")?);
    Ok((Height::from_bytes(key), hash))
}

/// Opens an existing snapshot source as a read-only RocksDB secondary.
///
/// The source must be persistent, use the current format, and contain all
/// required column families. The primary database is never created or migrated.
/// Use a separate working directory for `secondary_dir` and keep it alive
/// until the returned database and all of its clones have been dropped.
pub(super) fn open_snapshot_source(
    config: &Config,
    network: &Network,
    secondary_dir: &Path,
) -> Result<ZebraDb, BoxError> {
    let version = state_database_format_version_in_code();
    let path = existing_state_path(config, network, &version, "snapshot source")?;
    open_snapshot_source_at_path(config, network, &path, &version, secondary_dir)
}

fn open_snapshot_source_at_path(
    config: &Config,
    network: &Network,
    path: &Path,
    version: &Version,
    secondary_dir: &Path,
) -> Result<ZebraDb, BoxError> {
    ZebraDb::open_for_snapshot_comparison(config, network, path, version, Some(secondary_dir))
        .map_err(|error| {
            format!(
                "cannot open snapshot source state {} as a secondary: {error}",
                path.display()
            )
            .into()
        })
}

/// Resolves aliases and checks the format without opening a database.
fn existing_state_path(
    config: &Config,
    network: &Network,
    version: &Version,
    role: &str,
) -> Result<PathBuf, BoxError> {
    if config.ephemeral {
        return Err(format!("{role} state must be persistent, not ephemeral").into());
    }
    let path = config.db_path(STATE_DATABASE_KIND, version.major, network);
    let path = path
        .canonicalize()
        .map_err(|error| format!("{role} state {} is unavailable: {error}", path.display()))?;
    let disk_version = database_format_version_at_path(
        &path.join(DATABASE_FORMAT_VERSION_FILE_NAME),
        &path,
        version.major,
    )
    .map_err(|error| format!("cannot read {role} state format: {error}"))?;
    if disk_version.as_ref() != Some(version) {
        return Err(format!(
            "{role} state {} must already use current format {version}, found {disk_version:?}",
            path.display()
        )
        .into());
    }
    Ok(path)
}

/// Compares already-open database views without changing either state.
fn compare_databases(
    expected: &ZebraDb,
    imported: &ZebraDb,
) -> Result<ComparisonSummary, BoxError> {
    let expected_tip = checked_tip(expected)?;
    let imported_tip = checked_tip(imported)?;
    if expected_tip != imported_tip {
        return Err(format!(
            "finalized tip mismatch: expected {expected_tip:?}, imported {imported_tip:?}"
        )
        .into());
    }
    let height = expected_tip.0;
    let start = window_start(height);
    let utxo_transactions = utxo_transactions(expected, height)?;
    let mut component_entries = BTreeMap::new();
    for component in ComponentId::ALL {
        let mut expected_rows = open_rows(expected, component)?;
        let mut imported_rows = open_rows(imported, component)?;
        let mut subtree_ends = BTreeSet::new();
        let (expected_below_window, imported_below_window) =
            if let Some(subtrees) = subtrees_for(component) {
                subtree_ends = subtree_end_heights(expected, subtrees, start)?;
                (
                    tree_below_window(&mut expected_rows, start)?,
                    tree_below_window(&mut imported_rows, start)?,
                )
            } else {
                (None, None)
            };

        let selected = |key: &[u8], value: &[u8], below_window| -> Result<bool, BoxError> {
            let in_window = |height_in_row: Height| {
                height_in_row.is_min() || (start..=height).contains(&height_in_row)
            };
            let stored = |height_in_row: Height| height_in_row.is_min() || height_in_row == height;
            use ComponentId::*;
            Ok(match component {
                HashByHeight | Headers => in_window(height_of(key)?),
                HeightByHash => in_window(height_of(value)?),
                Transactions => stored(height_of(key)?),
                BlockInfo => {
                    let row_height = height_of(key)?;
                    row_height.is_min()
                        || (start.0.saturating_sub(1)..=height.0).contains(&row_height.0)
                }
                TxLocByHash => {
                    let location = value
                        .try_into()
                        .map_err(|_| "invalid transaction location")?;
                    stored(height_of(value)?) || utxo_transactions.binary_search(location).is_ok()
                }
                SaplingTree | OrchardTree | IronwoodTree => {
                    let row_height = height_of(key)?;
                    row_height.is_min()
                        || subtree_ends.contains(&row_height)
                        || (start..=height).contains(&row_height)
                        || Some(row_height) == below_window
                }
                _ => true,
            })
        };

        expected_rows.seek_to_first();
        imported_rows.seek_to_first();
        let mut entries = 0u64;
        loop {
            let expected_row = selected_row(&mut expected_rows, |key, value| {
                selected(key, value, expected_below_window)
            })?;
            let imported_row = selected_row(&mut imported_rows, |key, value| {
                selected(key, value, imported_below_window)
            })?;
            match (expected_row, imported_row) {
                (Some((key, _)), Some((other_key, _))) if key != other_key => {
                    return Err(format!(
                        "{component} key mismatch: expected {key:02x?}, imported {other_key:02x?}"
                    )
                    .into());
                }
                (Some((key, value)), Some((_, other_value))) => {
                    if value != other_value
                        && (!component.is_canonicalized()
                            || canonical_value(component, key, value)?
                                != canonical_value(component, key, other_value)?)
                    {
                        return Err(format!("{component} value mismatch at key {key:02x?}").into());
                    }
                }
                (Some((key, _)), None) => {
                    return Err(
                        format!("{component} missing imported row at key {key:02x?}").into(),
                    );
                }
                (None, Some((key, _))) => {
                    return Err(format!("{component} extra imported row at key {key:02x?}").into());
                }
                (None, None) => break,
            }
            expected_rows.next();
            imported_rows.next();
            entries = entries
                .checked_add(1)
                .ok_or("snapshot component entry count overflow")?;
        }
        component_entries.insert(component.column_family(), entries);
    }
    Ok(ComparisonSummary {
        height: imported_tip.0,
        block_hash: imported_tip.1.to_string(),
        component_entries,
        sprout_root: hex::encode(Into::<[u8; 32]>::into(
            imported.sprout_tree_for_tip().root(),
        )),
        sapling_root: hex::encode(Into::<[u8; 32]>::into(
            imported.sapling_tree_for_tip().root(),
        )),
        orchard_root: hex::encode(Into::<[u8; 32]>::into(
            imported.orchard_tree_for_tip().root(),
        )),
        ironwood_root: hex::encode(Into::<[u8; 32]>::into(
            imported.ironwood_tree_for_tip().root(),
        )),
        history_root: imported.history_tree().hash().map(|root| root.to_string()),
    })
}

fn height_of(bytes: &[u8]) -> Result<Height, BoxError> {
    let height = bytes
        .get(..HEIGHT_DISK_BYTES)
        .ok_or("invalid on-disk height")?;
    Ok(Height::from_bytes(height))
}

type RawRows<'a> = DBRawIteratorWithThreadMode<'a, DBWithThreadMode<SingleThreaded>>;
type Row<'a> = (&'a [u8], &'a [u8]);

fn open_rows(db: &ZebraDb, component: ComponentId) -> Result<RawRows<'_>, BoxError> {
    open_family(db, component.column_family())
}

fn open_family<'a>(db: &'a ZebraDb, family: &str) -> Result<RawRows<'a>, BoxError> {
    let mut options = ReadOptions::default();
    options.fill_cache(false);
    db.raw_iterator(family, options)
        .ok_or_else(|| format!("state is missing column family {family}").into())
}

/// Advance to the next selected row without copying its RocksDB key or value.
fn selected_row<'a>(
    rows: &'a mut RawRows<'_>,
    mut keep: impl FnMut(&[u8], &[u8]) -> Result<bool, BoxError>,
) -> Result<Option<Row<'a>>, BoxError> {
    while let Some((key, value)) = rows.item() {
        if keep(key, value)? {
            return Ok(rows.item());
        }
        rows.next();
    }
    rows.status()?;
    Ok(None)
}

/// Collects sorted, distinct transaction locations for UTXOs without shipped bodies.
fn utxo_transactions(
    db: &ZebraDb,
    tip: Height,
) -> Result<Vec<[u8; TRANSACTION_LOCATION_DISK_BYTES]>, BoxError> {
    let mut rows = open_rows(db, ComponentId::Utxos)?;
    rows.seek_to_first();
    let mut transactions = Vec::new();
    while let Some(key) = rows.key() {
        let height = height_of(key)?;
        if !height.is_min() && height != tip {
            let location = key
                .get(..TRANSACTION_LOCATION_DISK_BYTES)
                .ok_or("invalid UTXO transaction location")?
                .try_into()?;
            if transactions.last() != Some(&location) {
                transactions.push(location);
            }
        }
        rows.next();
    }
    rows.status()?;
    Ok(transactions)
}

fn subtrees_for(component: ComponentId) -> Option<ComponentId> {
    use ComponentId::*;
    match component {
        SaplingTree => Some(SaplingSubtrees),
        OrchardTree => Some(OrchardSubtrees),
        IronwoodTree => Some(IronwoodSubtrees),
        _ => None,
    }
}

fn subtree_end_heights(
    db: &ZebraDb,
    subtrees: ComponentId,
    start: Height,
) -> Result<BTreeSet<Height>, BoxError> {
    let mut rows = open_rows(db, subtrees)?;
    rows.seek_to_first();
    let mut ends = BTreeSet::new();
    while let Some(value) = rows.value() {
        let height = height_of(value)?;
        if height < start && !height.is_min() {
            ends.insert(height);
        }
        rows.next();
    }
    rows.status()?;
    Ok(ends)
}

fn tree_below_window(rows: &mut RawRows<'_>, start: Height) -> Result<Option<Height>, BoxError> {
    rows.seek_for_prev(start.previous()?.as_bytes());
    rows.status()?;
    rows.key().map(height_of).transpose()
}
