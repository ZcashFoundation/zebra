//! Regression tests for the fixed-width to variable-width v33 migration.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use crossbeam_channel::bounded;
use semver::Version;
use zebra_chain::{
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
                AddressLocation, AddressTransaction, AddressUnspentOutput, OutputLocation,
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
];

type Records = BTreeMap<String, Vec<(Vec<u8>, Vec<u8>)>>;

fn test_config(cache: &tempfile::TempDir) -> Config {
    Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    }
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
        &Version::new(33, 0, 0),
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
    let initial_tip = prepare(db).unwrap();
    let (_sender, receiver) = bounded(1);
    let change =
        DbFormatChange::open_database(&Version::new(33, 0, 0), Some(Version::new(30, 0, 0)));
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
fn migration_rewrites_all_records_collisions_and_survives_reopen() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let legacy = seed_legacy_records(&db);
    let expected = expected_records(&legacy);
    db.update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();

    apply_height_upgrade(&db);

    assert_eq!(
        db.format_version_on_disk().unwrap(),
        Some(Version::new(33, 0, 0))
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
fn read_only_open_rejects_unmigrated_v30_records() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    seed_legacy_records(&db);
    db.update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();
    drop(db);

    let error = ZebraDb::new(
        &config,
        STATE_DATABASE_KIND,
        &Version::new(33, 0, 0),
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
        "30.0.0"
    );
}

#[test]
fn read_only_open_and_refresh_preserve_completed_migration_checkpoint() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let primary = open_test_db(&config);
    seed_legacy_records(&primary);
    primary
        .update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();
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
fn interrupted_migration_restarts_from_its_immutable_checkpoint() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    let legacy = seed_legacy_records(&db);
    let expected = expected_records(&legacy);
    db.update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();

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
    assert_eq!(
        db.format_version_on_disk().unwrap(),
        Some(Version::new(30, 0, 0))
    );
    assert!(checkpoint_paths(&db).0.exists());

    apply_height_upgrade(&db);
    assert_records(&db, &expected);
}

#[test]
fn checkpoint_from_a_different_database_is_rejected_before_mutation() {
    let _guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let config = test_config(&cache);
    let db = open_test_db(&config);
    seed_legacy_records(&db);
    db.update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();
    let _initial_tip = prepare(&db).unwrap();
    let live_path = db.path().to_path_buf();
    let checkpoint = checkpoint_paths(&db).0;
    drop(db);

    std::fs::remove_dir_all(live_path).unwrap();
    let replacement = open_test_db(&config);
    seed_legacy_records(&replacement);
    replacement
        .update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();
    let error = prepare(&replacement).unwrap_err();
    assert!(
        matches!(error, crate::StateInitError::HistoryRead { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(
        replacement.format_version_on_disk().unwrap(),
        Some(Version::new(30, 0, 0))
    );
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
    db.update_format_version_on_disk(&Version::new(30, 0, 0))
        .unwrap();

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
    let old_version = Version::new(30, 0, 0);
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
        upgraded.db.format_version_on_disk().unwrap().unwrap().major,
        33
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
