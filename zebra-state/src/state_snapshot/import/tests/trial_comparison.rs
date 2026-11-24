//! Tests of the trial's independent comparison of snapshot-selected state.

use zebra_chain::{
    block::{self, Height},
    state_snapshot::ObjectHash,
};

use crate::{
    constants::STATE_DATABASE_KIND,
    service::finalized_state::ZebraDb,
    state_database_format_version_in_code,
    state_snapshot::{
        compare::{compare_states, open_snapshot_source, validate_snapshot_source},
        format::{window_start, ComponentId},
        SnapshotBase,
    },
    Config, DiskWriteBatch, IntoDisk, MAX_BLOCK_REORG_HEIGHT,
};

use super::fixture::{self, HEIGHT};

#[test]
fn source_admission_rejects_malformed_tip_and_checkpoint_rows() {
    let _init_guard = zebra_test::init();
    let network = fixture::network();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Default::default()
    };
    let version = state_database_format_version_in_code();
    let path = config.db_path(STATE_DATABASE_KIND, version.major, &network);
    let version_path = config.version_file_path(STATE_DATABASE_KIND, version.major, &network);
    let tip = (Height(1), block::Hash([1; 32]));
    let key = tip.0.as_bytes();

    let write = |family, bytes: &[u8]| {
        let db = super::super::open_raw(&path).unwrap();
        db.put_cf(db.cf_handle(family).unwrap(), key, bytes)
            .unwrap();
    };
    write("hash_by_height", &[1]);
    std::fs::write(version_path, version.to_string()).unwrap();
    let check = || {
        validate_snapshot_source(&config, &network, tip, &[tip])
            .unwrap_err()
            .to_string()
    };
    assert!(check().contains("invalid finalized tip hash"));
    write("hash_by_height", &tip.1.as_bytes());
    write(ComponentId::Headers.column_family(), &[1]);
    assert!(check().contains("invalid prepared source header"));
    write("default", &[1]);
    assert!(check().contains("nonempty default column family"));
}

/// Comparing absent states must not create a primary state directory.
#[test]
fn comparison_does_not_create_missing_state() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let source = chain.state.db.config();
    let (height, block_hash) = chain.state.db.tip().unwrap();
    let base = SnapshotBase {
        window_start: window_start(height),
        height,
        block_hash,
        manifest: ObjectHash([0; 32]),
        deferred_checks: 0,
    };
    let secondary_dir = tempfile::tempdir().unwrap();
    let opened = open_snapshot_source(source, &chain.network, secondary_dir.path()).unwrap();
    assert_eq!(opened.tip(), Some((height, block_hash)));
    drop(opened);

    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().join("absent"),
        ..Config::default()
    };
    assert!(open_snapshot_source(&config, &chain.network, secondary_dir.path()).is_err());
    assert!(!config.cache_dir.exists());
    for (expected, imported) in [(&config, source), (source, &config)] {
        assert!(compare_states(expected, imported, &chain.network, base).is_err());
        assert!(!config.cache_dir.exists());
    }

    // Even a directory claiming the current format is not enough: no RocksDB
    // files may be created when the existing database itself is missing.
    let version = state_database_format_version_in_code();
    let path = config.db_path(STATE_DATABASE_KIND, version.major, &chain.network);
    std::fs::create_dir_all(&path).unwrap();
    let version_path = config.version_file_path(STATE_DATABASE_KIND, version.major, &chain.network);
    std::fs::write(&version_path, version.to_string()).unwrap();
    let error = open_snapshot_source(&config, &chain.network, secondary_dir.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a database"), "{error}");
    let error = compare_states(source, &config, &chain.network, base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a database"), "{error}");
    let entries = std::fs::read_dir(&path)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries, vec![version_path]);

    // A RocksDB with the current version but without Zebra's column families is also refused.
    drop(rocksdb::DB::open_default(&path).unwrap());
    let error = open_snapshot_source(&config, &chain.network, secondary_dir.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("missing column family"), "{error}");
}

/// The comparison ignores a genuine imported header tail, but not a selected row change.
#[test]
fn comparison_round_trip_with_header_tail() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let network = &chain.network;
    let source = chain.state.db.config();
    let records = fixture::select(&chain.state);
    let successor = fixture::successor(&chain);
    let tip = chain.state.db.tip().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let export = fixture::export(
        &records,
        network,
        tip.0,
        tip.1,
        &cache.path().join("pieces"),
        300,
    );
    let base = fixture::import_export_with_headers(
        &export,
        network,
        &config,
        0,
        std::slice::from_ref(&successor.header),
    )
    .unwrap();

    let imported = ZebraDb::open_without_format_change(&config, network).unwrap();
    assert_eq!(chain.state.db.stored_header_tip(), None);
    assert_eq!(imported.tip(), Some(tip));
    assert_eq!(
        imported.stored_header_tip(),
        Some((Height(HEIGHT.0 + 1), successor.hash())),
        "the import keeps the header above its finalized tip"
    );

    // The independent oracle must own B's primary lock, not read a live secondary.
    let error = compare_states(source, &config, network, base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("exclusively open imported state"), "{error}");
    assert!(error.contains("stop its primary"), "{error}");
    // No timing or retry is involved: dropping the last owner releases RocksDB's lock.
    drop(imported);

    let summary = compare_states(source, &config, network, base).unwrap();
    assert_eq!(summary.height, HEIGHT);
    assert_eq!(summary.block_hash, tip.1.to_string());
    for component in ComponentId::ALL {
        assert_eq!(
            summary.component_entries[component.column_family()],
            u64::try_from(records[&component].len()).unwrap(),
            "{component} count includes exactly the selected rows"
        );
    }
    assert_eq!(
        summary.sprout_root,
        hex::encode(Into::<[u8; 32]>::into(
            chain.state.db.sprout_tree_for_tip().root(),
        ))
    );
    assert_eq!(
        summary.sapling_root,
        hex::encode(Into::<[u8; 32]>::into(
            chain.state.db.sapling_tree_for_tip().root(),
        ))
    );
    assert_eq!(
        summary.orchard_root,
        hex::encode(Into::<[u8; 32]>::into(
            chain.state.db.orchard_tree_for_tip().root(),
        ))
    );
    assert_eq!(
        summary.ironwood_root,
        hex::encode(Into::<[u8; 32]>::into(
            chain.state.db.ironwood_tree_for_tip().root(),
        ))
    );
    assert_eq!(
        summary.history_root,
        chain
            .state
            .db
            .history_tree()
            .hash()
            .map(|root| root.to_string())
    );

    // Equal selected rows are not enough: the marker must match even its deferred flags.
    let wrong_base = SnapshotBase {
        deferred_checks: base.deferred_checks ^ SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS,
        ..base
    };
    let error = compare_states(source, &config, network, wrong_base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("snapshot base marker mismatch"), "{error}");

    // An unmarked database with the same rows must not count as a successful import.
    let imported = ZebraDb::open_without_format_change(&config, network).unwrap();
    let mut batch = DiskWriteBatch::new();
    let _ = imported
        .snapshot_base_cf()
        .with_batch_for_writing(&mut batch)
        .zs_delete(&());
    imported.write_batch(batch).unwrap();
    drop(imported);
    let error = compare_states(source, &config, network, base)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("missing its snapshot base marker"),
        "{error}"
    );

    let imported = ZebraDb::open_without_format_change(&config, network).unwrap();
    let mut batch = DiskWriteBatch::new();
    batch.update_snapshot_base(&imported, &base);
    batch.update_sprout_tree(&imported, &fixture::sprout_tree(&[3, 4]));
    imported.write_batch(batch).unwrap();
    drop(imported);
    let error = compare_states(source, &config, network, base)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&ComponentId::SproutTipTree.to_string())
            || error.contains(&ComponentId::SproutAnchors.to_string()),
        "{error}"
    );
    assert!(error.contains("key"), "{error}");

    // A comparison must not launch a format upgrade or rewrite the stored version.
    let version = state_database_format_version_in_code();
    let version_path = config.version_file_path(STATE_DATABASE_KIND, version.major, network);
    std::fs::write(&version_path, "0.0.0").unwrap();
    let error = compare_states(source, &config, network, base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("must already use current format"), "{error}");
    let secondary_dir = tempfile::tempdir().unwrap();
    let error = open_snapshot_source(&config, network, secondary_dir.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("must already use current format"), "{error}");
    assert_eq!(std::fs::read_to_string(version_path).unwrap(), "0.0.0");

    // Comparing A to itself must fail even when its primary is stopped.
    let source = source.clone();
    drop(chain.state);
    let error = compare_states(&source, &source, network, base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("same database"), "{error}");
    let relative_alias = Config {
        cache_dir: source.cache_dir.join("."),
        ..source.clone()
    };
    let error = compare_states(&source, &relative_alias, network, base)
        .unwrap_err()
        .to_string();
    assert!(error.contains("same database"), "{error}");

    #[cfg(unix)]
    {
        let aliases = tempfile::tempdir().unwrap();
        let cache_dir = aliases.path().join("source");
        std::os::unix::fs::symlink(&source.cache_dir, &cache_dir).unwrap();
        let alias = Config {
            cache_dir,
            ..source
        };
        let error = compare_states(&alias, &relative_alias, network, base)
            .unwrap_err()
            .to_string();
        assert!(error.contains("same database"), "{error}");
    }
}
