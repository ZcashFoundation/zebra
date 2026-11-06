//! Regression tests for the fixed-width to variable-width v33 migration, which runs above the v32
//! snapshot barrier.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crossbeam_channel::bounded;
use semver::Version;
use zebra_chain::{
    amount::{Amount, NegativeAllowed},
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
};

use crate::{
    config::write_database_format_version_to_disk,
    constants::STATE_DATABASE_KIND,
    service::finalized_state::{
        disk_db::DiskDb,
        disk_format::{
            block::height_from_bytes_prefix,
            transparent::{
                AddressBalanceLocationChange, AddressLocation, AddressTransaction,
                AddressUnspentOutput, OutputLocation,
            },
            FromDisk, IntoDisk,
        },
        FinalizedState, ZebraDb, STATE_COLUMN_FAMILIES_IN_CODE,
    },
    CheckpointVerifiedBlock, Config, TransactionLocation,
};

use super::*;

const EXPECTED_FAMILIES: &[&str] = &[
    "hash_by_height",
    "height_by_hash",
    "block_header_by_height",
    "tx_by_loc",
    "hash_by_tx_loc",
    "tx_loc_by_hash",
    "balance_by_transparent_addr",
    "tx_loc_by_transparent_addr_loc",
    "utxo_by_out_loc",
    "utxo_loc_by_transparent_addr_loc",
    "tx_loc_by_spent_out_loc",
    "sprout_nullifiers",
    "sprout_note_commitment_tree",
    "sapling_nullifiers",
    "sapling_note_commitment_tree",
    "sapling_note_commitment_subtree",
    "orchard_nullifiers",
    "orchard_note_commitment_tree",
    "orchard_note_commitment_subtree",
    "ironwood_nullifiers",
    "ironwood_note_commitment_tree",
    "ironwood_note_commitment_subtree",
    "history_tree",
    "block_info",
    "snapshot_base",
];

type Records = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

fn test_config(cache: &tempfile::TempDir) -> Config {
    Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    }
}

fn running_version() -> Version {
    crate::constants::state_database_format_version_in_code()
}

fn v30() -> Version {
    let mut version = running_version();
    version.major = 30;
    version.minor = 1;
    version
}

fn v32() -> Version {
    let mut version = running_version();
    version.major = 32;
    version.minor = 0;
    version
}

fn open_test_db(config: &Config) -> ZebraDb {
    open_db(config, false)
}

fn open_secondary(config: &Config) -> ZebraDb {
    open_db(config, true)
}

fn open_db(config: &Config, read_only: bool) -> ZebraDb {
    ZebraDb::new(
        config,
        STATE_DATABASE_KIND,
        &running_version(),
        &Network::Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        read_only,
    )
    .unwrap()
}

fn example_part(part: Part, salt: u8) -> Vec<u8> {
    let height = [1, 2, 3];
    let transaction = [1, 2, 3, 0xab, 0xcd];
    let output = [1, 2, 3, 0xab, 0xcd, 0x01, 0x02, 0x03];

    match part {
        Part::Unchanged => vec![salt; 32],
        Part::Height | Part::HeightOrEmpty => height.to_vec(),
        Part::TransactionLocation | Part::OptionalTransactionLocation => transaction.to_vec(),
        Part::OutputLocation => output.to_vec(),
        Part::AddressBalance => [[salt; 8].as_slice(), &output, &[0x5a; 8]].concat(),
        Part::AddressTransaction => [output.as_slice(), &transaction].concat(),
        Part::AddressUnspentOutput => [output.as_slice(), &output].concat(),
        Part::Subtree => [height.as_slice(), &[salt; 32]].concat(),
        Part::SnapshotBase => [
            height.as_slice(),
            height.as_slice(),
            &[salt; 32],
            &[salt; 32],
            &[1],
        ]
        .concat(),
    }
}

fn seed_legacy_records(db: &ZebraDb) -> Records {
    let live = db.disk_db().inner();
    let mut records = Records::new();

    for (family_index, family) in AFFECTED_FAMILIES.iter().enumerate() {
        let salt = u8::try_from(family_index + 1).unwrap();
        if family.name == "snapshot_base" {
            records.insert(family.name.to_string(), Vec::new());
            continue;
        }
        let mut family_records = vec![(
            example_part(family.key, salt),
            example_part(family.value, salt.wrapping_add(1)),
        )];
        if let Part::HeightOrEmpty = family.key {
            family_records.push((Vec::new(), vec![0x44; 32]));
        }
        match family.value {
            Part::OptionalTransactionLocation => {
                family_records.push((vec![salt.wrapping_add(1); 32], Vec::new()));
            }
            Part::AddressBalance => {
                let location = example_part(Part::OutputLocation, salt);
                family_records.push((vec![salt; 21], [[salt; 8].as_slice(), &location].concat()));
            }
            _ => {}
        }
        if family.name == "hash_by_height" {
            // old(131328) == new(256): independent put/delete passes lose one of these records.
            family_records.push((vec![0, 1, 0], vec![0xa0; 32]));
            family_records.push((vec![2, 1, 0], vec![0xb0; 32]));
        }

        let cf = live.cf_handle(family.name).unwrap();
        for (key, value) in &family_records {
            live.put_cf(&cf, key, value).unwrap();
        }
        records.insert(family.name.to_string(), family_records);
    }

    records
}

fn expected_records(records: &Records) -> Records {
    records
        .iter()
        .map(|(name, records)| {
            let family = AFFECTED_FAMILIES
                .iter()
                .find(|family| family.name == name)
                .unwrap();
            let converted = records
                .iter()
                .map(|(key, value)| convert_record(family, key, value))
                .collect();
            (name.clone(), converted)
        })
        .collect()
}

fn snapshot(db: &ZebraDb) -> Records {
    let live = db.disk_db().inner();
    STATE_COLUMN_FAMILIES_IN_CODE
        .iter()
        .map(|name| {
            let cf = live.cf_handle(name).unwrap();
            let records = live
                .iterator_cf_opt(&cf, ReadOptions::default(), IteratorMode::Start)
                .map(|item| {
                    let (key, value) = item.unwrap();
                    (key.to_vec(), value.to_vec())
                })
                .collect();
            ((*name).to_string(), records)
        })
        .collect()
}

fn assert_records(db: &ZebraDb, expected: &Records) {
    let live = db.disk_db().inner();
    for (name, expected) in expected {
        let cf = live.cf_handle(name).unwrap();
        let actual: BTreeMap<_, _> = live
            .iterator_cf_opt(&cf, ReadOptions::default(), IteratorMode::Start)
            .map(|item| {
                let (key, value) = item.unwrap();
                (key.to_vec(), value.to_vec())
            })
            .collect();
        let expected: BTreeMap<_, _> = expected
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        assert_eq!(actual, expected, "column family {name}");
    }
}

fn apply_height_upgrade(db: &ZebraDb) {
    apply_height_upgrade_from(db, v30());
}

fn apply_height_upgrade_from(db: &ZebraDb, disk_version: Version) {
    let initial_tip = prepare(db).unwrap();
    let (_sender, receiver) = bounded(1);
    let change = DbFormatChange::open_database(&running_version(), Some(disk_version));
    change
        .apply_format_upgrade(db, initial_tip, &receiver)
        .unwrap();
    finish(db);
}

#[test]
fn family_list_covers_every_height_dependent_column_family() {
    let actual: Vec<_> = AFFECTED_FAMILIES.iter().map(|family| family.name).collect();
    assert_eq!(actual, EXPECTED_FAMILIES);
}

#[test]
fn upgrade_chain_runs_variable_heights_after_the_snapshot_barrier() {
    let running = running_version();
    assert_eq!((running.major, running.minor, running.patch), (33, 0, 0));
    let chain: Vec<_> = super::super::format_upgrades(Some(v30()))
        .map(|upgrade| (upgrade.version(), upgrade.needs_migration()))
        .collect();
    assert_eq!(
        chain,
        vec![
            (Version::new(32, 0, 0), false),
            (Version::new(33, 0, 0), true)
        ]
    );

    let sources = super::super::restorable_db_source_versions(33);
    assert_eq!(&sources[..3], &[32, 30, 29]);
    for disk_version in [v30(), v32()] {
        assert!(
            !super::super::can_read_without_migration(&disk_version, &running_version()),
            "{disk_version} has fixed-width heights"
        );
        assert!(is_required(&DbFormatChange::open_database(
            &running_version(),
            Some(disk_version)
        )));
    }
    assert!(super::super::can_read_without_migration(
        &running_version(),
        &running_version()
    ));
}

#[test]
fn migration_rewrites_all_records_collisions_and_survives_reopen() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let legacy = seed_legacy_records(&db);
    let expected = expected_records(&legacy);
    db.update_format_version_on_disk(&v30()).unwrap();

    apply_height_upgrade(&db);

    assert_eq!(
        db.format_version_on_disk().unwrap(),
        Some(running_version())
    );
    assert_records(&db, &expected);
    let (checkpoint, temporary) = checkpoint_paths(&db);
    assert!(!checkpoint.exists());
    assert!(!temporary.exists());
    drop(db);

    let reopened = open_test_db(&config);
    assert_records(&reopened, &expected);
}

#[test]
fn snapshot_born_v32_database_upgrades_and_preserves_history_floors() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut config = test_config(&cache);
    config.prune_blocks_older_than = Some(crate::constants::MIN_PRUNING_RETENTION);
    let db = open_test_db(&config);
    let base = crate::SnapshotBase {
        window_start: Height(2),
        height: Height(2),
        block_hash: zebra_chain::block::Hash([0x22; 32]),
        manifest: zebra_chain::state_snapshot::ObjectHash([0x33; 32]),
        deferred_checks: 0,
    };
    let live = db.disk_db().inner();
    live.put_cf(
        live.cf_handle("hash_by_height").unwrap(),
        legacy_height(base.height),
        base.block_hash.as_bytes(),
    )
    .unwrap();
    live.put_cf(
        live.cf_handle("tx_by_loc").unwrap(),
        legacy_location(TransactionLocation::from_usize(base.height, 0)),
        [0x44; 8],
    )
    .unwrap();
    live.put_cf(
        live.cf_handle("snapshot_base").unwrap(),
        [],
        legacy_part(Part::SnapshotBase, &base.as_bytes()),
    )
    .unwrap();
    // Snapshot imports before format 33 wrote fixed-width records into `state/v32`.
    db.update_format_version_on_disk(&v32()).unwrap();

    apply_height_upgrade_from(&db, v32());

    assert_eq!(
        db.format_version_on_disk().unwrap(),
        Some(running_version())
    );
    let marker = live.cf_handle("snapshot_base").unwrap();
    let migrated = live.get_pinned_cf(&marker, []).unwrap().unwrap();
    assert_eq!(crate::SnapshotBase::decode(&migrated).unwrap(), base);
    let history = db.history_capabilities().unwrap();
    assert_eq!(history.lowest_retained_height, Height(2));
    assert_eq!(history.lowest_known_height, Height(2));
    assert_eq!(history.snapshot_base, Some(base));
    drop(migrated);
    drop(db);

    let restarted = open_test_db(&config);
    assert_eq!(
        restarted.history_capabilities().unwrap().snapshot_base,
        Some(base)
    );
}

#[test]
fn migration_merges_legacy_balance_operands_before_converting() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let legacy = DiskDb::new_for_variable_height_upgrade(
        &config,
        STATE_DATABASE_KIND,
        &v30(),
        &Network::Mainnet,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    let location = OutputLocation::from_usize(Height(1), 0, 0);
    let change = |balance, received| {
        let mut change = AddressBalanceLocationChange::new(location);
        *change.balance_mut() = Amount::<NegativeAllowed>::try_from(balance).unwrap();
        *change.received_mut() = received;
        change
    };
    let first = change(10_i64, 100_u64);
    let second = change(20_i64, 200_u64);
    let first = legacy_part(Part::AddressBalance, &first.as_bytes());
    let second = legacy_part(Part::AddressBalance, &second.as_bytes());
    let key = [7; 21];
    let live = legacy.inner();
    let cf = live.cf_handle("balance_by_transparent_addr").unwrap();
    live.merge_cf(&cf, key, first).unwrap();
    live.merge_cf(&cf, key, second).unwrap();

    let expected = (change(10_i64, 100_u64) + change(20_i64, 200_u64))
        .unwrap()
        .as_bytes();
    let legacy_expected = legacy_part(Part::AddressBalance, &expected);
    let checkpoint = cache.path().join("legacy-balance-checkpoint");
    legacy.create_checkpoint(&checkpoint).unwrap();
    let source = DiskDb::open_read_only(&checkpoint).unwrap();
    let source_cf = source.cf_handle("balance_by_transparent_addr").unwrap();
    assert_eq!(
        source.get_pinned_cf(&source_cf, key).unwrap().as_deref(),
        Some(legacy_expected.as_slice())
    );

    let family = AFFECTED_FAMILIES
        .iter()
        .find(|family| family.name == "balance_by_transparent_addr")
        .unwrap();
    let (_sender, receiver) = bounded(1);
    for pass in [
        Pass::WriteConverted,
        Pass::DeleteLegacy,
        Pass::WriteConverted,
    ] {
        rewrite_pass(live, &source, family, pass, &receiver).unwrap();
    }
    assert_eq!(
        live.get_pinned_cf(&cf, key).unwrap().as_deref(),
        Some(expected.as_slice())
    );
    drop(source);
    drop(legacy);

    let reopened = DiskDb::new(
        &config,
        STATE_DATABASE_KIND,
        &v30(),
        &Network::Mainnet,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    let live = reopened.inner();
    let cf = live.cf_handle("balance_by_transparent_addr").unwrap();
    assert_eq!(
        live.get_pinned_cf(&cf, key).unwrap().as_deref(),
        Some(expected.as_slice())
    );
}

#[test]
fn read_only_open_rejects_unmigrated_v30_records() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    seed_legacy_records(&db);
    db.update_format_version_on_disk(&v30()).unwrap();
    drop(db);

    let error = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &running_version(),
        &Network::Mainnet,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        true,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        crate::StateInitError::ReadOnlyDatabaseNeedsUpgrade { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(
            config
                .db_path(STATE_DATABASE_KIND, 33, &Network::Mainnet)
                .join("version")
        )
        .unwrap(),
        v30().to_string()
    );
}

#[test]
fn read_only_open_and_refresh_preserve_completed_migration_checkpoint() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let primary = open_test_db(&config);
    seed_legacy_records(&primary);
    primary.update_format_version_on_disk(&v30()).unwrap();
    apply_height_upgrade(&primary);
    let (checkpoint, _) = checkpoint_paths(&primary);
    primary.disk_db().create_checkpoint(&checkpoint).unwrap();

    let secondary = open_secondary(&config);
    assert!(checkpoint.exists());
    let key = Height(42).as_bytes();
    let value = [0x42; 32];
    let cf = primary
        .disk_db()
        .inner()
        .cf_handle("hash_by_height")
        .unwrap();
    primary.disk_db().inner().put_cf(&cf, &key, value).unwrap();
    primary.disk_db().inner().flush_cf(&cf).unwrap();
    secondary.try_catch_up_with_primary().unwrap();
    let secondary_cf = secondary
        .disk_db()
        .inner()
        .cf_handle("hash_by_height")
        .unwrap();
    assert_eq!(
        secondary
            .disk_db()
            .inner()
            .get_pinned_cf(&secondary_cf, &key)
            .unwrap()
            .as_deref(),
        Some(value.as_slice())
    );
    drop(secondary);
    drop(primary);

    assert!(checkpoint.exists());
    std::fs::remove_dir_all(checkpoint).unwrap();
}

#[test]
fn restart_after_the_rewrite_removes_completed_migration_checkpoint() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let (checkpoint, temporary) = checkpoint_paths(&db);
    db.disk_db().create_checkpoint(&checkpoint).unwrap();
    db.update_format_version_on_disk(&running_version())
        .unwrap();
    assert!(checkpoint.exists());
    drop(db);

    let restarted = open_test_db(&config);
    assert!(!checkpoint.exists());
    assert!(!temporary.exists());
    assert_eq!(
        restarted.format_version_on_disk().unwrap(),
        Some(running_version())
    );
}

#[test]
fn interrupted_migration_restarts_from_its_immutable_checkpoint() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let legacy = seed_legacy_records(&db);
    let expected = expected_records(&legacy);
    db.update_format_version_on_disk(&v30()).unwrap();

    let _initial_tip = prepare(&db).unwrap();
    let source = open_source(&db).unwrap();
    let (_sender, receiver) = bounded(1);
    for pass in [Pass::WriteConverted, Pass::DeleteLegacy] {
        rewrite_pass(
            db.disk_db().inner(),
            &source,
            &AFFECTED_FAMILIES[0],
            pass,
            &receiver,
        )
        .unwrap();
    }
    drop(source);

    let (sender, receiver) = bounded(1);
    sender.send(CancelFormatChange).unwrap();
    assert_eq!(
        Upgrade.run(Height(0), &db, &receiver),
        Err(CancelFormatChange)
    );
    assert_eq!(db.format_version_on_disk().unwrap(), Some(v30()));
    assert!(checkpoint_paths(&db).0.exists());

    apply_height_upgrade(&db);
    assert_records(&db, &expected);
}

#[test]
fn stale_same_identity_checkpoint_is_rejected_before_replacing_newer_state() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    seed_legacy_records(&db);
    db.update_format_version_on_disk(&v30()).unwrap();
    let checkpoint = checkpoint_paths(&db).0;
    db.disk_db().create_checkpoint(&checkpoint).unwrap();
    let stale_source = DiskDb::open_read_only(&checkpoint).unwrap();
    let stale_identity = stale_source.get_db_identity().unwrap();
    drop(stale_source);

    let live = db.disk_db().inner();
    live.put_cf(
        live.cf_handle("hash_by_height").unwrap(),
        [4, 5, 6],
        [0xee; 32],
    )
    .unwrap();
    let replacement_path = cache.path().join("newer-replacement");
    db.disk_db().create_checkpoint(&replacement_path).unwrap();
    let live_path = db.path().to_path_buf();
    drop(db);

    std::fs::remove_dir_all(&live_path).unwrap();
    std::fs::rename(replacement_path, &live_path).unwrap();
    let replacement = open_test_db(&config);
    assert_eq!(
        replacement.disk_db().inner().get_db_identity().unwrap(),
        stale_identity,
        "the restored replacement must have the stale checkpoint's lineage"
    );
    let before = snapshot(&replacement);
    let before_version = replacement.format_version_on_disk().unwrap();

    let error = prepare(&replacement).unwrap_err();
    assert!(
        matches!(error, crate::StateInitError::HistoryRead { .. }),
        "unexpected error: {error}"
    );
    assert_records(&replacement, &before);
    assert_eq!(
        replacement.format_version_on_disk().unwrap(),
        before_version
    );
}

#[test]
fn versionless_rename_gap_runs_the_height_migration_instead_of_inferring_current() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let network = Network::Mainnet;
    let legacy = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &v30(),
        &network,
        true,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    let legacy_records = seed_legacy_records(&legacy);
    let expected = expected_records(&legacy_records);
    let legacy_path = legacy.path().to_path_buf();
    drop(legacy);
    if legacy_path.join("version").exists() {
        std::fs::remove_file(legacy_path.join("version")).unwrap();
    }

    let current_path = config.db_path(STATE_DATABASE_KIND, 33, &network);
    std::fs::create_dir_all(current_path.parent().unwrap()).unwrap();
    std::fs::rename(legacy_path, &current_path).unwrap();

    let disk_version = crate::state_database_format_version_on_disk(&config, &network)
        .unwrap()
        .unwrap();
    let change = DbFormatChange::open_database(&running_version(), Some(disk_version));
    assert!(
        is_required(&change),
        "a versionless rename gap must not be inferred as the current format"
    );

    let db = open_test_db(&config);
    let initial_tip = prepare(&db).unwrap();
    let (_sender, receiver) = bounded(1);
    change
        .apply_format_upgrade(&db, initial_tip, &receiver)
        .unwrap();
    finish(&db);
    assert_eq!(
        db.format_version_on_disk().unwrap(),
        Some(running_version())
    );
    assert_records(&db, &expected);
    assert!(!checkpoint_paths(&db).0.exists());
}

#[test]
fn versionless_current_records_are_rejected_instead_of_rewritten() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let live = db.disk_db().inner();
    live.put_cf(
        live.cf_handle("hash_by_height").unwrap(),
        Height::MIN.as_bytes(),
        [0x55; 32],
    )
    .unwrap();
    let before = snapshot(&db);
    let version_path = db.path().join("version");
    if version_path.exists() {
        std::fs::remove_file(version_path).unwrap();
    }

    let error = prepare(&db).unwrap_err();
    assert!(
        matches!(error, crate::StateInitError::HistoryRead { .. }),
        "unexpected error: {error}"
    );
    assert_records(&db, &before);
}

#[test]
fn versionless_empty_current_directory_opens_as_a_new_database() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let path = config.db_path(
        STATE_DATABASE_KIND,
        running_version().major,
        &Network::Mainnet,
    );
    std::fs::create_dir_all(path).unwrap();

    let db = open_test_db(&config);
    let live = db.disk_db().inner();
    let hashes = live.cf_handle("hash_by_height").unwrap();
    live.put_cf(&hashes, Height::MIN.as_bytes(), [0x55; 32])
        .unwrap();
    assert_eq!(
        live.get_pinned_cf(&hashes, Height::MIN.as_bytes())
            .unwrap()
            .as_deref(),
        Some([0x55; 32].as_slice())
    );
}

#[test]
fn versionless_current_records_with_pending_balance_merges_survive_reopen() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let live = db.disk_db().inner();
    live.put_cf(
        live.cf_handle("hash_by_height").unwrap(),
        Height::MIN.as_bytes(),
        [0x55; 32],
    )
    .unwrap();

    let location = OutputLocation::from_usize(Height(1), 0, 0);
    let change = |balance, received| {
        let mut change = AddressBalanceLocationChange::new(location);
        *change.balance_mut() = Amount::<NegativeAllowed>::try_from(balance).unwrap();
        *change.received_mut() = received;
        change
    };
    let first = change(10_i64, 100_u64);
    let second = change(20_i64, 200_u64);
    let key = [7; 21];
    let cf = live.cf_handle("balance_by_transparent_addr").unwrap();
    live.merge_cf(&cf, key, first.as_bytes()).unwrap();
    live.merge_cf(&cf, key, second.as_bytes()).unwrap();
    let expected = (first + second).unwrap();

    let version_path = db.path().join("version");
    if version_path.exists() {
        std::fs::remove_file(version_path).unwrap();
    }
    drop(db);

    let reopened = open_test_db(&config);
    let live = reopened.disk_db().inner();
    let cf = live.cf_handle("balance_by_transparent_addr").unwrap();
    let actual = live.get_pinned_cf(&cf, key).unwrap().unwrap();
    assert_eq!(
        AddressBalanceLocationChange::from_bytes(actual).as_bytes(),
        expected.as_bytes()
    );
}

#[test]
fn checkpoint_from_a_different_database_is_rejected_before_mutation() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    seed_legacy_records(&db);
    let before = snapshot(&db);
    db.update_format_version_on_disk(&v30()).unwrap();
    let checkpoint = checkpoint_paths(&db).0;

    let other_cache = tempfile::tempdir().unwrap();
    let other = open_test_db(&test_config(&other_cache));
    seed_legacy_records(&other);
    let other_checkpoint = other_cache.path().join("other-checkpoint");
    other
        .disk_db()
        .create_checkpoint(&other_checkpoint)
        .unwrap();
    drop(other);
    std::fs::rename(other_checkpoint, &checkpoint).unwrap();

    let error = prepare(&db).unwrap_err();
    assert!(
        matches!(error, crate::StateInitError::HistoryRead { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(db.format_version_on_disk().unwrap(), Some(v30()));
    assert_records(&db, &before);
    assert!(checkpoint.exists());
}

#[test]
fn pruned_v30_records_migrate_without_full_history() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut config = test_config(&cache);
    config.prune_blocks_older_than = Some(crate::constants::MIN_PRUNING_RETENTION);
    let db = open_test_db(&config);
    let mut legacy = seed_legacy_records(&db);
    let height_one_index = (vec![0, 0, 1, 0, 0], vec![0x11; 32]);
    let height_two_body = (vec![0, 0, 2, 0, 0], vec![0x22; 32]);
    let live = db.disk_db().inner();
    for (name, record) in [
        ("hash_by_tx_loc", height_one_index.clone()),
        ("tx_by_loc", height_two_body.clone()),
    ] {
        live.put_cf(live.cf_handle(name).unwrap(), &record.0, &record.1)
            .unwrap();
        legacy.get_mut(name).unwrap().push(record);
    }
    let expected = expected_records(&legacy);
    db.update_format_version_on_disk(&v30()).unwrap();

    assert!(!Upgrade.requires_full_history());
    apply_height_upgrade(&db);

    assert_eq!(
        db.history_capabilities().unwrap().lowest_retained_height,
        Height(2)
    );
    assert_records(&db, &expected);
}

fn legacy_height(height: Height) -> Vec<u8> {
    assert!(height.0 <= 0x00ff_ffff);
    height.0.to_be_bytes()[1..].to_vec()
}

fn legacy_location(location: TransactionLocation) -> Vec<u8> {
    [
        legacy_height(location.height),
        location.index.as_bytes().to_vec(),
    ]
    .concat()
}

fn legacy_output(location: OutputLocation) -> Vec<u8> {
    [
        legacy_height(location.height()),
        location.transaction_index().as_bytes().to_vec(),
        location.output_index().as_bytes().to_vec(),
    ]
    .concat()
}

fn legacy_part(part: Part, bytes: &[u8]) -> Vec<u8> {
    match part {
        Part::Unchanged => bytes.to_vec(),
        Part::Height => legacy_height(Height::from_bytes(bytes)),
        Part::HeightOrEmpty if bytes.is_empty() => Vec::new(),
        Part::HeightOrEmpty => legacy_height(Height::from_bytes(bytes)),
        Part::TransactionLocation => legacy_location(TransactionLocation::from_bytes(bytes)),
        Part::OutputLocation => legacy_output(OutputLocation::from_bytes(bytes)),
        Part::AddressBalance => {
            let (balance, rest) = bytes.split_at(8);
            let (location, received) = AddressLocation::from_bytes_prefix(rest).unwrap();
            assert!(received.is_empty() || received.len() == 8);
            [balance, &legacy_output(location), received].concat()
        }
        Part::AddressTransaction => {
            let location = AddressTransaction::from_bytes(bytes);
            [
                legacy_output(location.address_location()),
                legacy_location(location.transaction_location()),
            ]
            .concat()
        }
        Part::AddressUnspentOutput => {
            let location = AddressUnspentOutput::from_bytes(bytes);
            [
                legacy_output(location.address_location()),
                legacy_output(location.unspent_output_location()),
            ]
            .concat()
        }
        Part::OptionalTransactionLocation if bytes.is_empty() => Vec::new(),
        Part::OptionalTransactionLocation => {
            legacy_location(TransactionLocation::from_bytes(bytes))
        }
        Part::Subtree => {
            let (height, root) = height_from_bytes_prefix(bytes).unwrap();
            assert_eq!(root.len(), 32);
            [legacy_height(height), root.to_vec()].concat()
        }
        Part::SnapshotBase => {
            let (window_start, rest) = height_from_bytes_prefix(bytes).unwrap();
            let (height, rest) = height_from_bytes_prefix(rest).unwrap();
            assert_eq!(rest.len(), 65);
            [
                legacy_height(window_start),
                legacy_height(height),
                rest.to_vec(),
            ]
            .concat()
        }
    }
}

#[tokio::test]
async fn old_chain_upgrades_legacy_pools_reads_new_writes_and_restarts() {
    let _guard = zebra_test::init();
    let network = Network::Mainnet;
    let mut source = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap();
    let mut blocks = Vec::new();
    for (_, bytes) in zebra_test::vectors::MAINNET_BLOCKS.range(0..=2) {
        let block = bytes.zcash_deserialize_into::<Arc<Block>>().unwrap();
        source
            .commit_finalized_direct(
                CheckpointVerifiedBlock::from(block.clone()).into(),
                None,
                "variable-height fixture",
            )
            .unwrap();
        blocks.push(block);
    }
    let current_records = snapshot(&source.db);
    let mut upgraded_records = current_records.clone();
    let expected_pools = source.db.finalized_value_pool();
    let expected_info = source.db.block_info(Height(2).into()).unwrap();
    let old_build = source.db.format_version_in_code().build;
    drop(source);

    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let old_version = v30();
    let old_path = config.db_path(STATE_DATABASE_KIND, 30, &network);
    let legacy = DiskDb::new(
        &config,
        STATE_DATABASE_KIND,
        &old_version,
        &network,
        STATE_COLUMN_FAMILIES_IN_CODE
            .iter()
            .map(ToString::to_string),
        false,
    )
    .unwrap();
    for (name, records) in &current_records {
        let family = AFFECTED_FAMILIES.iter().find(|family| family.name == name);
        let cf = legacy.inner().cf_handle(name).unwrap();
        for (record_index, (key, value)) in records.iter().enumerate() {
            let (key, mut value) = if let Some(family) = family {
                (
                    legacy_part(family.key, key),
                    legacy_part(family.value, value),
                )
            } else {
                (key.clone(), value.clone())
            };
            if name == "tip_chain_value_pool" {
                value.truncate(48);
            } else if name == "block_info" {
                value.truncate(52);
            }
            if name == "tip_chain_value_pool" || name == "block_info" {
                upgraded_records.get_mut(name).unwrap()[record_index].1 = value.clone();
            }
            legacy.inner().put_cf(&cf, key, value).unwrap();
        }
    }
    drop(legacy);
    let mut old_version = old_version;
    old_version.build = old_build;
    write_database_format_version_to_disk(&config, STATE_DATABASE_KIND, 30, &old_version, &network)
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        crate::config::check_and_delete_old_state_databases(&config, &network),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(old_path.exists(), "cleanup protects the reusable v30 cache");

    let mut upgraded = FinalizedState::new(
        &config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        while !upgraded.db.ready_for_pruning() {
            upgraded.db.check_for_panics();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    assert!(!old_path.exists());
    assert!(!checkpoint_paths(&upgraded.db).0.exists());
    assert_eq!(
        upgraded.db.format_version_on_disk().unwrap(),
        Some(running_version())
    );
    assert_records(&upgraded.db, &upgraded_records);
    assert_eq!(upgraded.db.finalized_value_pool(), expected_pools);
    assert!(upgraded.db.finalized_value_pool().nsm_amount().is_zero());
    assert_eq!(
        upgraded.db.block_info(Height(2).into()).unwrap(),
        expected_info
    );
    for (height, expected) in blocks.iter().enumerate() {
        let block = upgraded
            .db
            .block(Height(height.try_into().unwrap()).into())
            .unwrap()
            .unwrap();
        assert_eq!(block, *expected);
        for (index, transaction) in block.transactions.iter().enumerate() {
            let location = upgraded
                .db
                .transaction_location(transaction.hash())
                .unwrap();
            assert_eq!(
                location,
                TransactionLocation::from_usize(Height(height.try_into().unwrap()), index)
            );
        }
    }

    let next = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    upgraded
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(next.clone()).into(),
            None,
            "variable-height post-upgrade write",
        )
        .unwrap();
    blocks.push(next);

    let live = upgraded.db.disk_db().inner();
    let pools = live.cf_handle("tip_chain_value_pool").unwrap();
    assert_eq!(live.get_pinned_cf(&pools, []).unwrap().unwrap().len(), 56);
    let block_info = live.cf_handle("block_info").unwrap();
    assert_eq!(
        live.get_pinned_cf(&block_info, Height(3).as_bytes())
            .unwrap()
            .unwrap()
            .len(),
        60
    );
    assert_eq!(
        live.get_pinned_cf(&block_info, Height(2).as_bytes())
            .unwrap()
            .unwrap()
            .len(),
        52
    );
    drop(upgraded);

    let restarted = FinalizedState::new(
        &config,
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .unwrap();
    for (height, expected) in blocks.iter().enumerate() {
        assert_eq!(
            restarted
                .db
                .block(Height(height.try_into().unwrap()).into())
                .unwrap()
                .unwrap(),
            *expected
        );
    }
}
