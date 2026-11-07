//! Import round trips on a populated Regtest database, and the snapshots the
//! importer refuses.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bincode::Options;

use zebra_chain::{
    amount::Amount,
    block::{self, Height},
    orchard, sapling,
    serialization::ZcashSerialize,
    sprout,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
    transaction::Transaction,
    transparent,
};

use crate::{
    constants::{DATABASE_FORMAT_VERSION_FILE_NAME, STATE_DATABASE_KIND},
    service::finalized_state::{FinalizedState, FormatCheckError, TransactionLocation, ZebraDb},
    state_database_format_version_in_code,
    state_snapshot::{
        format::{ComponentId, PieceManifest, PieceWriter},
        import::{
            build_piece_sst, decode_history_tree, import_dir, import_state_snapshot, is_singleton,
            records, sst_path, window_start, ImportCheck, SstMetadata,
        },
        SnapshotBase, SnapshotError,
    },
    CheckpointVerifiedBlock, Config, FromDisk, HistoryCapabilities, IntoDisk,
    MAX_BLOCK_REORG_HEIGHT,
};

use zebra_db::service::finalized_state::disk_format::chain::RawHistoryTree;

use super::fixture::{self, Records, HEIGHT};

/// Returns the check a refused import names.
fn refused_by(result: &Result<SnapshotBase, SnapshotError>) -> ImportCheck {
    match result {
        Err(SnapshotError::Refused { check, .. }) => *check,
        other => panic!("expected a refused import, got {other:?}"),
    }
}

/// Local format-check read failures do not permanently reject the snapshot, but invalid contents do.
#[test]
fn database_format_error_attribution_preserves_local_failures() {
    let local = crate::StateInitError::HistoryRead {
        path: PathBuf::from("snapshot-import-test"),
        source: "injected local read failure".into(),
    };
    let error = super::super::checks::database_format_error(FormatCheckError::HistoryRead(local));
    assert!(matches!(error, SnapshotError::HistoryRead(_)));
    assert!(!error.rejects_snapshot());

    let error = super::super::checks::database_format_error(FormatCheckError::Invalid(
        "invalid snapshot subtree".to_string(),
    ));
    assert_eq!(refused_by(&Err(error)), ImportCheck::DatabaseFormat);
}

/// An unreadable local database version is a retryable import failure, not a snapshot rejection.
#[test]
fn unreadable_database_version_does_not_reject_the_snapshot() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let import = fixture::import(&chain, &records, &chain.network, 0);
    let base = import.result.unwrap();
    let db = ZebraDb::open_without_format_change(&import.config, &chain.network).unwrap();
    let version_path = import
        .config
        .db_path(
            STATE_DATABASE_KIND,
            state_database_format_version_in_code().major,
            &chain.network,
        )
        .join(DATABASE_FORMAT_VERSION_FILE_NAME);
    std::fs::write(version_path, b"not a database format version").unwrap();

    let result = super::super::checks::check_open_database(&db, &base);
    assert!(
        matches!(&result, Err(SnapshotError::FormatVersion { .. })),
        "{result:?}"
    );
    assert!(!result.unwrap_err().rejects_snapshot());
}

/// A snapshot of a populated database imports into a database with the same
/// rows, which opens as the current format, describes the snapshot, serves an
/// output created below the window, and accepts the next block.
#[test]
fn import_round_trip() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let deferred = SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS;
    let import = fixture::import(&chain, &records, &chain.network, deferred);
    let base = import.result.unwrap();

    let (height, hash) = chain.state.db.tip().unwrap();
    assert_eq!(height, HEIGHT);
    assert_eq!(
        base,
        SnapshotBase {
            window_start: window_start(HEIGHT),
            height,
            block_hash: hash,
            manifest: base.manifest,
            // The Regtest fixture has no checkpoint at the snapshot height.
            deferred_checks: deferred,
        }
    );

    let mut state = FinalizedState::new_with_debug(
        &import.config,
        &chain.network,
        false,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    for component in ComponentId::ALL {
        assert_eq!(
            fixture::rows(&state, component),
            records[&component],
            "{component} rows are imported as exported"
        );
    }
    assert_eq!(
        state.db.history_capabilities().unwrap(),
        HistoryCapabilities {
            lowest_retained_height: HEIGHT,
            lowest_known_height: window_start(HEIGHT),
            lowest_header_height: window_start(HEIGHT),
            snapshot_base: Some(base),
        }
    );
    assert_eq!(state.db.tip(), Some((height, hash)));

    let below_window = transparent::OutPoint {
        hash: chain.coinbases[3],
        index: 0,
    };
    assert!(Height(3) < window_start(HEIGHT));
    assert_eq!(
        state.db.utxo(&below_window).map(|utxo| utxo.utxo.output),
        chain
            .state
            .db
            .utxo(&below_window)
            .map(|utxo| utxo.utxo.output),
    );
    assert_eq!(
        state
            .db
            .address_balance_location(&fixture::address(3))
            .map(|balance| balance.balance()),
        chain
            .state
            .db
            .address_balance_location(&fixture::address(3))
            .map(|balance| balance.balance()),
    );
    assert!(
        chain
            .state
            .db
            .transaction_hashes_for_block(Height(3).into())
            .is_some(),
        "the source has the complete block"
    );
    assert_eq!(
        state.db.transaction_hashes_for_block(Height(3).into()),
        None,
        "a UTXO-parent index row is not a complete below-window block"
    );

    let successor = fixture::successor(&chain);
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(successor.clone()).into(),
            None,
            "state snapshot import test",
        )
        .unwrap();
    assert_eq!(
        state.db.tip(),
        Some((Height(HEIGHT.0 + 1), successor.hash()))
    );
    assert!(state.db.utxo(&below_window).is_none());
}

/// Applies `tamper` to the honest records of `chain`, and returns the result of
/// importing them.
fn import_tampered(
    chain: &fixture::Chain,
    tamper: impl FnOnce(&mut Records),
) -> Result<SnapshotBase, SnapshotError> {
    let mut records = fixture::select(&chain.state);
    tamper(&mut records);
    for rows in records.values_mut() {
        rows.sort();
    }
    fixture::import(chain, &records, &chain.network, 0).result
}

/// The importer's value pool check requires the UTXO set to hold exactly the
/// transparent pool (design, "Trust model and verification"). That relation
/// holds on databases populated with the Mainnet and Testnet block vectors,
/// whose coinbases pay miners and founders, and not only on the fixture chain.
#[test]
fn utxo_set_value_is_the_transparent_pool_on_real_blocks() {
    use std::sync::Arc;

    use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};

    let _init_guard = zebra_test::init();
    for (network, blocks) in [
        (
            Network::Mainnet,
            &*zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS,
        ),
        (
            Network::new_default_testnet(),
            &*zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS,
        ),
    ] {
        let mut state = FinalizedState::new_with_debug(
            &Config::ephemeral(),
            &network,
            true,
            #[cfg(feature = "elasticsearch")]
            false,
            false,
        )
        .unwrap();
        for bytes in blocks.values() {
            let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
            state
                .commit_finalized_direct(
                    CheckpointVerifiedBlock::from(block).into(),
                    None,
                    "transparent pool test",
                )
                .unwrap();
        }
        assert!(
            blocks.len() > 1,
            "{network} has block vectors after genesis"
        );

        let utxos: u64 = fixture::rows(&state, ComponentId::Utxos)
            .iter()
            .map(|(_, value)| u64::from(transparent::Output::from_bytes(value).value))
            .sum();
        let pool = u64::from(state.db.finalized_value_pool().transparent_amount());
        assert_ne!(pool, 0, "{network} coinbases pay transparent outputs");
        assert_eq!(utxos, pool, "{network} UTXO set holds the transparent pool");
    }
}

/// Returns the record of `component` at `key`.
fn record<'a>(records: &'a mut Records, component: ComponentId, key: &[u8]) -> &'a mut Vec<u8> {
    records
        .get_mut(&component)
        .unwrap()
        .iter_mut()
        .find(|(record_key, _)| record_key == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("{component} has a record at {key:?}"))
}

/// Returns the history tree record `bytes`, whose current height is `from`,
/// with its current height set to `to`.
fn with_current_height(bytes: &[u8], from: Height, to: Height) -> Vec<u8> {
    // The height is the record's last field, which bincode writes as a variable-length integer:
    // a 0xfb tag and two little-endian bytes, for heights from 251 to 65535.
    let encode = |height: Height| {
        let [low, high] = u16::try_from(height.0).unwrap().to_le_bytes();
        [0xfb, low, high]
    };
    assert!(bytes.ends_with(&encode(from)));
    let mut bytes = bytes[..bytes.len() - 3].to_vec();
    bytes.extend(encode(to));
    bytes
}

/// A history tree that does not end at the snapshot block is refused, even
/// though it is a valid tree for the chain: whether its current height or its
/// rightmost peak is below the snapshot height.
#[test]
fn tampered_history_tree_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let below = Height(HEIGHT.0 - 1);
    let honest = fixture::singleton(&chain.state, ComponentId::HistoryTree);
    let earlier = chain.history_tree_below_tip.clone();
    for tree in [
        earlier.clone(),
        with_current_height(&honest, HEIGHT, below),
        with_current_height(&earlier, below, HEIGHT),
    ] {
        let result = import_tampered(&chain, |records| {
            *record(records, ComponentId::HistoryTree, &[]) = tree;
        });
        assert_eq!(refused_by(&result), ImportCheck::HistoryTree);
    }
}

/// A Sapling frontier at the snapshot height whose root is not the root the
/// history tree commits to is refused, even with a correct cached root and an
/// anchor for its root.
#[test]
fn tampered_frontier_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let result = import_tampered(&chain, |records| {
        let mut tree = sapling::tree::NoteCommitmentTree::default();
        // An uncommitted leaf would leave the root unchanged, so use another field element.
        let mut leaf = [0; 32];
        leaf[0] = 2;
        let leaf = Option::from(sapling::tree::NoteCommitmentUpdate::from_bytes(&leaf)).unwrap();
        tree.append(leaf).unwrap();
        records
            .get_mut(&ComponentId::SaplingAnchors)
            .unwrap()
            .push((tree.root().as_bytes().to_vec(), Vec::new()));
        records
            .get_mut(&ComponentId::SaplingTree)
            .unwrap()
            .push((HEIGHT.as_bytes().to_vec(), tree.as_bytes()));
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);
}

/// A note commitment tree whose cached root is not its root is refused, even
/// though the importer would recompute the root.
#[test]
fn tampered_cached_root_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let result = import_tampered(&chain, |records| {
        let genesis_tree = record(records, ComponentId::SaplingTree, &Height(0).as_bytes());
        let root = sapling::tree::NoteCommitmentTree::default().root();
        let root_bytes = <[u8; 32]>::from(root);
        let position = genesis_tree
            .windows(32)
            .position(|window| window == root_bytes)
            .expect("the genesis tree caches its root");
        // The lowest bit of a canonical field element can be flipped without leaving the field.
        genesis_tree[position] ^= 1;
    });
    assert_eq!(refused_by(&result), ImportCheck::Records);
}

/// A UTXO set with a missing output, or an output worth more than the
/// transparent pool allows, is refused.
#[test]
fn tampered_utxo_set_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();

    let result = import_tampered(&chain, |records| {
        records.get_mut(&ComponentId::Utxos).unwrap().remove(0);
    });
    assert_eq!(refused_by(&result), ImportCheck::ValuePools);

    let network = chain.network.clone();
    let result = import_tampered(&chain, |records| {
        let (_, value) = records
            .get_mut(&ComponentId::Utxos)
            .unwrap()
            .iter_mut()
            .find(|(_, value)| {
                transparent::Output::from_bytes(value)
                    .address(&network)
                    .is_none()
            })
            .expect("the fixture has outputs without an address");
        let mut output = transparent::Output::from_bytes(&value);
        output.value = (output.value + Amount::try_from(1).unwrap()).unwrap();
        *value = output.zcash_serialize_to_vec().unwrap();
    });
    assert_eq!(refused_by(&result), ImportCheck::ValuePools);
}

/// The importer rebuilds the address balance and UTXO indexes from the UTXO
/// set: each balance and UTXO row is the source's, except that the received
/// totals start at zero, and the address transaction index is empty below the
/// snapshot height.
#[test]
fn address_indexes_are_rebuilt_from_the_utxo_set() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let import = fixture::import(&chain, &records, &chain.network, 0);
    import.result.unwrap();
    let state = FinalizedState::new_with_debug(
        &import.config,
        &chain.network,
        false,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();

    let source_utxos = fixture::rows(&chain.state, ComponentId::Utxos);
    assert!(!source_utxos.is_empty());
    let cf_rows = |state: &FinalizedState, cf: &str| -> Vec<(Vec<u8>, Vec<u8>)> {
        let cf = state.db.cf_handle(cf).unwrap();
        state
            .db
            .zs_forward_range_iter::<_, crate::service::finalized_state::RawBytes, crate::service::finalized_state::RawBytes, _>(&cf, ..)
            .map(|(key, value)| (key.raw_bytes().clone(), value.raw_bytes().clone()))
            .collect()
    };
    let utxo_index = "utxo_loc_by_transparent_addr_loc";
    assert_eq!(
        cf_rows(&state, utxo_index),
        cf_rows(&chain.state, utxo_index)
    );

    for height in 0..5 {
        let address = fixture::address(height);
        let source = chain.state.db.address_balance_location(&address).unwrap();
        let imported = state.db.address_balance_location(&address).unwrap();
        assert_eq!(imported.balance(), source.balance(), "{address}");
        assert_eq!(imported.address_location(), source.address_location());
        assert!(source.received() > 0);
        assert_eq!(imported.received(), 0, "the received total is unknown");
    }
    assert!(cf_rows(&state, "tx_loc_by_transparent_addr_loc").is_empty());
}

/// The headers above the snapshot height that the bootstrap fetched are
/// checked against the window headers and stored in the header chain, so
/// headers-first sync continues after them; headers that do not follow the
/// snapshot block, or have an invalid time, are refused.
#[test]
fn headers_above_the_snapshot_are_checked_and_stored() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let (height, hash) = chain.state.db.tip().unwrap();
    let successor = fixture::successor(&chain);

    let import = |headers: &[Arc<block::Header>]| {
        let cache = tempfile::tempdir().unwrap();
        let config = Config {
            cache_dir: cache.path().to_path_buf(),
            prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
            ..Config::default()
        };
        let export = fixture::export(
            &records,
            &chain.network,
            height,
            hash,
            &cache.path().join("pieces"),
            300,
        );
        let result =
            fixture::import_export_with_headers(&export, &chain.network, &config, 0, headers);
        (cache, config, result)
    };

    let (_cache, config, result) = import(std::slice::from_ref(&successor.header));
    result.unwrap();
    let state = FinalizedState::new_with_debug(
        &config,
        &chain.network,
        false,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        state.db.stored_header_tip(),
        Some((Height(HEIGHT.0 + 1), successor.hash())),
        "the header chain continues after the snapshot block"
    );
    assert_eq!(state.db.tip(), Some((height, hash)));
    drop(state);

    let mut orphan = *successor.header;
    orphan.previous_block_hash = block::Hash([0x55; 32]);
    let (_cache, _config, result) = import(&[Arc::new(orphan)]);
    assert_eq!(refused_by(&result), ImportCheck::Headers);

    let mut early = *successor.header;
    early.time = chain.state.db.block_header(Height(0).into()).unwrap().time;
    let (_cache, _config, result) = import(&[Arc::new(early)]);
    assert_eq!(refused_by(&result), ImportCheck::Headers);
}

/// When the window starts at height one, the shipped genesis header continues
/// it, so the first header above a short snapshot is checked against a context
/// that includes genesis. Its time is above the median-time-past of heights
/// 0..=6, but not above that of heights 1..=6.
#[test]
fn headers_above_a_window_from_height_one_include_genesis_in_their_context() {
    let _init_guard = zebra_test::init();
    let network = fixture::network();
    let dir = tempfile::tempdir().unwrap();
    let db = super::super::open_raw(dir.path()).unwrap();

    let genesis = block::genesis::regtest_genesis_block().header.clone();
    let mut headers = vec![genesis.clone()];
    for height in 1..=6u32 {
        let previous = *headers[headers.len() - 1];
        let mut header = previous;
        header.previous_block_hash = previous.hash();
        header.time = genesis.time + chrono::Duration::seconds(75 * i64::from(height));
        header.nonce[0..4].copy_from_slice(&height.to_le_bytes());
        headers.push(Arc::new(header));
    }
    let shipped = db
        .cf_handle(ComponentId::Headers.column_family())
        .expect("the column family exists");
    for (height, header) in headers.iter().enumerate() {
        // The heights are at most 6.
        let height = Height(height as u32);
        db.put_cf(shipped, height.as_bytes(), header.as_bytes())
            .unwrap();
    }
    let base = SnapshotBase {
        window_start: Height(1),
        height: Height(6),
        block_hash: headers[6].hash(),
        manifest: ObjectHash([0; 32]),
        deferred_checks: 0,
    };
    assert_eq!(window_start(base.height), base.window_start);

    let mut above = *headers[6];
    above.previous_block_hash = headers[6].hash();
    above.time = genesis.time + chrono::Duration::seconds(75 * 4);
    super::super::headers::store_headers_above(
        &db,
        &base,
        &network,
        &[Arc::new(above)],
        crate::service::check::check_header_context,
    )
    .expect("the header's time is above the median-time-past from genesis");

    let stored = db
        .cf_handle(crate::service::finalized_state::HEADER_BY_HEIGHT)
        .expect("the column family exists");
    assert_eq!(
        db.get_cf(stored, Height(7).as_bytes()).unwrap(),
        Some(above.as_bytes().to_vec()),
        "the header above the snapshot is stored"
    );
}

/// Two transactions at one location are refused: the rebuilt hash index would
/// lose one of them.
#[test]
fn transactions_sharing_a_location_are_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let below_window = TransactionLocation::from_usize(Height(3), 0).as_bytes();
    let other = TransactionLocation::from_usize(Height(4), 0).as_bytes();
    let result = import_tampered(&chain, |records| {
        for (_, location) in records.get_mut(&ComponentId::TxLocByHash).unwrap() {
            if *location == other {
                *location = below_window.to_vec();
            }
        }
    });
    assert_eq!(refused_by(&result), ImportCheck::TransactionIndex);
}

/// A snapshot block body whose transactions do not have the header's merkle
/// root is refused, even when its transaction index rows match it.
#[test]
fn tampered_snapshot_block_body_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let location = TransactionLocation::from_usize(HEIGHT, 0).as_bytes();
    let result = import_tampered(&chain, |records| {
        let body = record(records, ComponentId::Transactions, &location);
        let coinbase = Transaction::from_bytes(&body);
        let old_hash = coinbase.hash();
        let mut outputs = coinbase.outputs();
        outputs[0].value = (outputs[0].value + Amount::try_from(1).unwrap()).unwrap();
        let tampered = coinbase.with_transparent_outputs(outputs);
        *body = tampered.zcash_serialize_to_vec().unwrap();
        let index = records.get_mut(&ComponentId::TxLocByHash).unwrap();
        index.retain(|(hash, _)| *hash != old_hash.as_bytes());
        index.push((tampered.hash().as_bytes().to_vec(), location.to_vec()));
    });
    assert_eq!(refused_by(&result), ImportCheck::Window);
}

/// A snapshot that ships a window body below the snapshot block, which the
/// importing node would never keep, is refused.
#[test]
fn window_body_below_the_snapshot_block_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let below = Height(HEIGHT.0 - 1);
    let body = fixture::rows(&chain.state, ComponentId::Transactions)
        .into_iter()
        .filter(|(key, _)| Height::from_bytes(&key[..3]) == below)
        .collect::<Vec<_>>();
    assert!(!body.is_empty(), "the fixture block below H has a body");
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::Transactions)
            .unwrap()
            .extend(body);
    });
    assert_eq!(refused_by(&result), ImportCheck::Window);
}

/// A piece whose bytes do not hash to its manifest entry is refused.
#[test]
fn tampered_piece_byte_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let (height, hash) = chain.state.db.tip().unwrap();
    let export = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );
    let piece = export.manifest.component(ComponentId::Utxos).pieces[1].hash;
    let path = &export.pieces[&piece];
    let mut bytes = std::fs::read(path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(path, bytes).unwrap();

    let result = fixture::import_export(&export, &chain.network, &config, 0);
    assert_eq!(refused_by(&result), ImportCheck::Piece);
    // The scheduler verified the piece, so a changed file is not the snapshot's fault.
    assert!(!result.unwrap_err().rejects_snapshot());
    assert!(!config
        .db_path(
            STATE_DATABASE_KIND,
            state_database_format_version_in_code().major,
            &chain.network
        )
        .exists());
}

/// The block hash at a checkpoint height must be the checkpoint, and the
/// importer then records it as checked, whatever the caller deferred.
#[test]
fn snapshot_block_is_checked_against_the_checkpoint() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let (height, hash) = chain.state.db.tip().unwrap();
    let deferred = SnapshotBase::DEFERRED_BLOCK_HASH | SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS;

    let above = fixture::checkpoint_above_snapshot();
    let wrong = fixture::network_with(vec![(height, chain.coinbases[1].0.into()), above]);
    let result = fixture::import(&chain, &records, &wrong, deferred).result;
    assert_eq!(refused_by(&result), ImportCheck::Checkpoint);

    let right = fixture::network_with(vec![(height, hash), above]);
    let base = fixture::import(&chain, &records, &right, deferred)
        .result
        .unwrap();
    assert_eq!(
        base.deferred_checks,
        SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS
    );
}

/// A manifest the commitment does not name, one for another block, or one
/// whose entry counts do not match its pieces, is refused, and an existing
/// state is never replaced. Only the manifest the commitment does not name is
/// this node's fault rather than the snapshot's.
#[test]
fn manifest_mismatches_and_existing_state_are_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let (height, hash) = chain.state.db.tip().unwrap();
    let export = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );

    let mut other = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );
    other.commitment.manifest = ObjectHash([1; 32]);
    let result = fixture::import_export(&other, &chain.network, &config, 0);
    assert_eq!(refused_by(&result), ImportCheck::ManifestHash);
    // The caller verified the manifest object against the commitment before importing it.
    assert!(!result.unwrap_err().rejects_snapshot());

    other.commitment = StateSnapshotCommitment {
        block_hash: chain.coinbases[1].0.into(),
        ..export.commitment
    };
    let result = fixture::import_export(&other, &chain.network, &config, 0);
    assert_eq!(refused_by(&result), ImportCheck::Manifest);
    assert!(result.unwrap_err().rejects_snapshot());

    let mut wrong_parameters = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );
    wrong_parameters.manifest.parameters_hash = Some([0xa5; 32]);
    wrong_parameters.commitment.manifest =
        ObjectHash::of(&wrong_parameters.manifest.encode().unwrap());
    let result = fixture::import_export(&wrong_parameters, &chain.network, &config, 0);
    assert_eq!(refused_by(&result), ImportCheck::Manifest);
    assert!(result.unwrap_err().rejects_snapshot());

    let mut miscounted = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );
    let utxos = usize::from(ComponentId::Utxos.to_u16());
    miscounted.manifest.components[utxos].entry_count += 1;
    miscounted.commitment.manifest = ObjectHash::of(&miscounted.manifest.encode().unwrap());
    let result = fixture::import_export(&miscounted, &chain.network, &config, 0);
    assert_eq!(refused_by(&result), ImportCheck::EntryCounts);

    fixture::import_export(&export, &chain.network, &config, 0).unwrap();
    let result = fixture::import_export(&export, &chain.network, &config, 0);
    assert!(
        matches!(result, Err(SnapshotError::Unsupported(_))),
        "{result:?}"
    );
}

/// A transaction with unspent outputs below the window needs both index rows,
/// pointing at each other, the window's parent needs its block info, and the
/// Ironwood pool must be empty before NU6.3.
#[test]
fn tampered_indexes_are_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let below_window = TransactionLocation::from_usize(Height(3), 0).as_bytes();
    let other = TransactionLocation::from_usize(Height(4), 0).as_bytes();
    let parent = Height(window_start(HEIGHT).0 - 1).as_bytes();

    // A missing location index row.
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::TxLocByHash)
            .unwrap()
            .retain(|(_, location)| *location != below_window);
    });
    assert_eq!(refused_by(&result), ImportCheck::TransactionIndex);

    // A location above the snapshot height.
    let above = TransactionLocation::from_usize(Height(HEIGHT.0 + 1), 0).as_bytes();
    let result = import_tampered(&chain, |records| {
        for (_, location) in records.get_mut(&ComponentId::TxLocByHash).unwrap() {
            if *location == other {
                *location = above.to_vec();
            }
        }
    });
    assert_eq!(refused_by(&result), ImportCheck::TransactionIndex);

    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::BlockInfo)
            .unwrap()
            .retain(|(height, _)| *height != parent);
    });
    assert_eq!(refused_by(&result), ImportCheck::Window);

    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::IronwoodNullifiers)
            .unwrap()
            .push((vec![7; 32], Vec::new()));
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);
}

/// A snapshot ships the Sapling trees at genesis, at each subtree's end height,
/// as of the window start, and in the window, and no others below the window.
/// The database format's subtree checks pass on that sparse database, and fail
/// without a subtree's end tree.
#[test]
fn sparse_subtree_rows_pass_the_subtree_checks() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain_with(fixture::inject_subtrees);
    let records = fixture::select(&chain.state);
    let tree_heights = |records: &Records| -> Vec<u32> {
        records[&ComponentId::SaplingTree]
            .iter()
            .map(|(key, _)| Height::from_bytes(key).0)
            .collect()
    };
    let dense: Vec<u32> = fixture::rows(&chain.state, ComponentId::SaplingTree)
        .iter()
        .map(|(key, _)| Height::from_bytes(key).0)
        .collect();
    assert_eq!(dense, [0, 4, 8, 15, 20, 30]);
    assert!(window_start(HEIGHT) > Height(20) && window_start(HEIGHT) < Height(30));
    assert_eq!(tree_heights(&records), [0, 8, 20, 30]);
    assert_eq!(records[&ComponentId::SaplingSubtrees].len(), 2);

    fixture::import(&chain, &records, &chain.network, 0)
        .result
        .unwrap();

    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::SaplingTree)
            .unwrap()
            .retain(|(key, _)| *key != Height(8).as_bytes());
    });
    assert_eq!(refused_by(&result), ImportCheck::DatabaseFormat);
    assert!(
        matches!(&result, Err(SnapshotError::Refused { detail, .. }) if detail.contains("subtree")),
        "{result:?}"
    );
}

/// Writes `records` of one component as a piece file in `dir`, and returns its
/// path and manifest entry.
fn piece_file(dir: &Path, records: &[(Vec<u8>, Vec<u8>)]) -> (PathBuf, PieceManifest) {
    let mut writer = PieceWriter::new(Vec::new());
    for (key, value) in records {
        writer.push(key, value).unwrap();
    }
    let bytes = writer.finish().unwrap();
    let piece = PieceManifest {
        size: bytes.len().try_into().unwrap(),
        hash: ObjectHash::of(&bytes),
    };
    let path = dir.join(piece.hash.to_string());
    std::fs::write(&path, bytes).unwrap();
    (path, piece)
}

/// A piece becomes an SST file only if it has its manifest entry's size and
/// hash and its records are valid for the component, and the SST file records
/// which piece it was built from.
#[test]
fn piece_ssts_are_built_from_matching_valid_pieces() {
    let _init_guard = zebra_test::init();
    let dir = tempfile::tempdir().unwrap();
    let hashes: Vec<_> = (1u8..=3)
        .map(|height| (Height(height.into()).as_bytes().to_vec(), vec![height; 32]))
        .collect();
    let (path, piece) = piece_file(dir.path(), &hashes);
    let sst = sst_path(dir.path(), ComponentId::HashByHeight, 0);

    let check = |result: Result<u64, SnapshotError>| match result {
        Err(SnapshotError::Refused { check, .. }) => check,
        other => panic!("expected a refused piece, got {other:?}"),
    };
    let wrong_size = PieceManifest {
        size: piece.size + 1,
        ..piece
    };
    assert_eq!(
        check(build_piece_sst(
            &path,
            ComponentId::HashByHeight,
            wrong_size,
            &sst
        )),
        ImportCheck::Piece
    );
    let wrong_hash = PieceManifest {
        hash: ObjectHash([0; 32]),
        ..piece
    };
    assert_eq!(
        check(build_piece_sst(
            &path,
            ComponentId::HashByHeight,
            wrong_hash,
            &sst
        )),
        ImportCheck::Piece
    );
    // Height keys are not block hashes, so these records are invalid height index rows.
    assert_eq!(
        check(build_piece_sst(
            &path,
            ComponentId::HeightByHash,
            piece,
            &sst
        )),
        ImportCheck::Records
    );
    assert!(!sst.exists());

    assert_eq!(
        build_piece_sst(&path, ComponentId::HashByHeight, piece, &sst).unwrap(),
        3
    );
    assert!(sst.is_file());
    assert_eq!(
        SstMetadata::read(&sst).unwrap(),
        Some(SstMetadata {
            piece: piece.hash,
            records: 3,
            first_key: hashes[0].0.clone(),
            last_key: hashes[2].0.clone(),
        })
    );
}

/// The importer ingests the SST files the caller built from the manifest's
/// pieces, and rebuilds one that was built from another piece.
#[test]
fn caller_built_ssts_are_reused_only_for_their_piece() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let (height, hash) = chain.state.db.tip().unwrap();
    let export = fixture::export(
        &records,
        &chain.network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );

    let temp_root = import_dir(&config).join("bootstrap");
    for (component, index, piece) in export.manifest.pieces() {
        if !is_singleton(component) {
            build_piece_sst(
                &export.pieces[&piece.hash],
                component,
                piece,
                &sst_path(&temp_root, component, index),
            )
            .unwrap();
        }
    }
    // The first UTXO SST file now holds the second piece's records.
    let utxos = &export.manifest.component(ComponentId::Utxos).pieces;
    std::fs::remove_file(sst_path(&temp_root, ComponentId::Utxos, 0)).unwrap();
    build_piece_sst(
        &export.pieces[&utxos[1].hash],
        ComponentId::Utxos,
        utxos[1],
        &sst_path(&temp_root, ComponentId::Utxos, 0),
    )
    .unwrap();

    import_state_snapshot(
        &temp_root,
        &export.manifest,
        export.commitment,
        0,
        &[],
        &chain.network,
        &config,
        &export.pieces,
    )
    .unwrap();
    let state = FinalizedState::new_with_debug(
        &config,
        &chain.network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    assert_eq!(
        fixture::rows(&state, ComponentId::Utxos),
        records[&ComponentId::Utxos]
    );
}

/// The tip tree's root must be an anchor, so transactions above the snapshot
/// height can spend from it.
#[test]
fn tip_root_without_an_anchor_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain_with(fixture::inject_subtrees);
    let tip_root = chain.state.db.sapling_tree_for_tip().root().as_bytes();
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::SaplingAnchors)
            .unwrap()
            .retain(|(anchor, _)| *anchor != tip_root);
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);
}

/// Populated Orchard and Sprout snapshots pass their matching-root checks, and refuse a wrong
/// Orchard frontier or a missing tip or empty anchor.
#[test]
fn non_sapling_tip_roots_and_anchors_are_checked() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain_with(fixture::inject_orchard_and_sprout_trees);
    assert!(chain.state.db.orchard_tree_for_tip().count() > 0);
    assert!(chain.state.db.sprout_tree_for_tip().count() > 0);
    let records = fixture::select(&chain.state);
    fixture::import(&chain, &records, &chain.network, 0)
        .result
        .unwrap();

    let result = import_tampered(&chain, |records| {
        let tree = fixture::orchard_tree(&[9]);
        let root = <[u8; 32]>::from(tree.root()).to_vec();
        records
            .get_mut(&ComponentId::OrchardAnchors)
            .unwrap()
            .push((root, Vec::new()));
        records
            .get_mut(&ComponentId::OrchardTree)
            .unwrap()
            .push((HEIGHT.as_bytes().to_vec(), tree.as_bytes()));
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);

    let orchard_tip = <[u8; 32]>::from(chain.state.db.orchard_tree_for_tip().root()).to_vec();
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::OrchardAnchors)
            .unwrap()
            .retain(|(anchor, _)| *anchor != orchard_tip);
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);

    let sprout_tip = <[u8; 32]>::from(chain.state.db.sprout_tree_for_tip().root()).to_vec();
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::SproutAnchors)
            .unwrap()
            .retain(|(anchor, _)| *anchor != sprout_tip);
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);

    let empty_sprout =
        <[u8; 32]>::from(sprout::tree::NoteCommitmentTree::default().recalculate_root()).to_vec();
    let empty_orchard =
        <[u8; 32]>::from(orchard::tree::NoteCommitmentTree::default().recalculate_root()).to_vec();
    for (component, empty_root) in [
        (ComponentId::SproutAnchors, empty_sprout),
        (ComponentId::OrchardAnchors, empty_orchard),
    ] {
        let result = import_tampered(&chain, |records| {
            records
                .get_mut(&component)
                .unwrap()
                .retain(|(anchor, _)| *anchor != empty_root);
        });
        assert_eq!(refused_by(&result), ImportCheck::TreeRoots);
    }
}

/// A populated post-NU6.3 Ironwood snapshot passes its matching-root check, and refuses a wrong
/// frontier or a missing tip or empty anchor.
#[test]
fn active_ironwood_tip_root_and_anchors_are_checked() {
    let _init_guard = zebra_test::init();
    let chain =
        fixture::chain_with_network(fixture::ironwood_network(), fixture::inject_ironwood_tree);
    assert!(chain.state.db.ironwood_tree_for_tip().count() > 0);
    let records = fixture::select(&chain.state);
    fixture::import(&chain, &records, &chain.network, 0)
        .result
        .unwrap();

    let result = import_tampered(&chain, |records| {
        let tree = fixture::orchard_tree(&[10]);
        let root = <[u8; 32]>::from(tree.root()).to_vec();
        records
            .get_mut(&ComponentId::IronwoodAnchors)
            .unwrap()
            .push((root, Vec::new()));
        records
            .get_mut(&ComponentId::IronwoodTree)
            .unwrap()
            .push((HEIGHT.as_bytes().to_vec(), tree.as_bytes()));
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);

    let ironwood_tip = <[u8; 32]>::from(chain.state.db.ironwood_tree_for_tip().root()).to_vec();
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::IronwoodAnchors)
            .unwrap()
            .retain(|(anchor, _)| *anchor != ironwood_tip);
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);

    let empty =
        <[u8; 32]>::from(orchard::tree::NoteCommitmentTree::default().recalculate_root()).to_vec();
    let result = import_tampered(&chain, |records| {
        records
            .get_mut(&ComponentId::IronwoodAnchors)
            .unwrap()
            .retain(|(anchor, _)| *anchor != empty);
    });
    assert_eq!(refused_by(&result), ImportCheck::TreeRoots);
}

/// A snapshot is refused before any chain check when its height is not below
/// the network's last checkpoint, so the block after it would not be
/// checkpoint-verified, or when NU5 is not active at its height (design, "Trust
/// model and verification").
#[test]
fn snapshot_failing_a_precondition_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let records = fixture::select(&chain.state);
    let (height, hash) = chain.state.db.tip().unwrap();
    for network in [
        // Only genesis is a checkpoint.
        fixture::network_with(vec![]),
        // The snapshot height is the last checkpoint.
        fixture::network_with(vec![(height, hash)]),
        // NU5 activates above the snapshot height.
        fixture::network_with_nu5_at(HEIGHT.0 + 10),
    ] {
        let result = fixture::import(&chain, &records, &network, 0).result;
        assert_eq!(refused_by(&result), ImportCheck::Preconditions);
        assert!(result.unwrap_err().rejects_snapshot());
    }
}

/// A note commitment tree that zebra-chain's deserializer would panic on is
/// refused: a frontier with the wrong number of ommers for its position, a node
/// that is not a field element, and a cached root that is not one.
#[test]
fn malformed_note_commitment_tree_is_refused() {
    use zebra_chain::sapling::tree::legacy::{
        LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyNoteCommitmentTree,
        LegacyPosition, Node,
    };

    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let node = |seed: u8| {
        let mut bytes = [0; 32];
        bytes[0] = seed;
        Node(Option::from(sapling_crypto::Node::from_bytes(bytes)).unwrap())
    };

    // Position 5 has two set bits, and its right leaf holds one ommer, so one more is expected.
    let mut legacy = LegacyNoteCommitmentTree::from(sapling::tree::NoteCommitmentTree::default());
    legacy.inner = LegacyFrontier {
        frontier: Some(LegacyNonEmptyFrontier {
            position: LegacyPosition(5),
            leaf: LegacyLeaf::Right(node(3), node(4)),
            ommers: vec![node(10), node(11)],
        }),
    };
    let too_many_ommers = bincode::DefaultOptions::new().serialize(&legacy).unwrap();

    // The same tree with one ommer, as `sapling_tree_at` builds it, is valid; replace its leaf,
    // then its cached root, with bytes above the field modulus.
    let valid = fixture::sapling_tree_at(5).as_bytes();
    let replace = |from: [u8; 32]| {
        let position = valid
            .windows(32)
            .position(|window| window == from)
            .expect("the tree contains the field element");
        let mut bytes = valid.clone();
        bytes[position..position + 32].fill(0xff);
        bytes
    };
    let not_a_node = replace(node(4).0.to_bytes());
    let not_a_root = replace(fixture::sapling_tree_at(5).root().into());

    for tree in [too_many_ommers, not_a_node, not_a_root] {
        let result = import_tampered(&chain, |records| {
            records
                .get_mut(&ComponentId::SaplingTree)
                .unwrap()
                .push((HEIGHT.as_bytes().to_vec(), tree));
        });
        assert_eq!(refused_by(&result), ImportCheck::Records);
    }
}

/// The mirror of the stored tree layout accepts the Orchard and Sprout trees
/// zebra-chain writes with a non-empty frontier, ending in a right leaf and in
/// a left leaf with an ommer; the other tests ship such Sapling trees.
#[test]
fn non_empty_orchard_and_sprout_trees_pass_the_layout_check() {
    use halo2::pasta::{group::ff::PrimeField, pallas};
    use zebra_chain::{orchard, sprout};

    let _init_guard = zebra_test::init();
    let leaf = |seed: u8| {
        let mut bytes = [0; 32];
        bytes[0] = seed;
        bytes
    };
    for leaves in [2u8, 3] {
        let mut orchard_tree = orchard::tree::NoteCommitmentTree::default();
        let mut sprout_tree = sprout::tree::NoteCommitmentTree::default();
        for seed in 1..=leaves {
            orchard_tree
                .append(pallas::Base::from_repr(leaf(seed)).unwrap())
                .unwrap();
            sprout_tree
                .append(sprout::NoteCommitment::from(leaf(seed)))
                .unwrap();
        }
        // The database writes trees with their roots cached.
        orchard_tree.root();
        sprout_tree.root();

        let decoded =
            records::orchard_tree(&orchard_tree.as_bytes()).expect("a valid Orchard tree");
        assert_eq!(decoded.count(), u64::from(leaves));
        let decoded = records::sprout_tree(&sprout_tree.as_bytes()).expect("a valid Sprout tree");
        assert_eq!(decoded.count(), u64::from(leaves));
    }
}

/// A history tree whose peaks' total work overflows when the tree combines
/// them, which panics in `zcash_history`, is refused.
#[test]
fn history_tree_with_overflowing_work_is_refused() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let honest = fixture::singleton(&chain.state, ComponentId::HistoryTree);
    let mut raw: RawHistoryTree = bincode::DefaultOptions::new().deserialize(&honest).unwrap();
    assert!(raw.peaks.len() >= 2, "the tree has peaks to combine");
    for entry in raw.peaks.values_mut() {
        // An entry is its kind (a leaf, or a node with two child indexes), then its node data,
        // whose total work follows the subtree commitment, two times, two targets and two Sapling
        // roots.
        let header = if entry.inner[0] == 1 { 1 } else { 1 + 2 * 4 };
        let work = header + 32 + 2 * 4 + 2 * 4 + 2 * 32;
        entry.inner[work..work + 32].fill(0xff);
    }
    let tampered = bincode::DefaultOptions::new().serialize(&raw).unwrap();
    assert_early_history_tree_refusal(&chain, &tampered, "total work");

    let result = import_tampered(&chain, |records| {
        *record(records, ComponentId::HistoryTree, &[]) = tampered;
    });
    assert_eq!(refused_by(&result), ImportCheck::HistoryTree);
}

/// The early history-tree decode used by bootstrap rejects unsafe peaks before rebuilding them:
/// no peaks, and Sapling or Orchard transaction counts that overflow when peaks are combined.
#[test]
fn early_history_tree_decode_rejects_unsafe_peaks() {
    let _init_guard = zebra_test::init();
    let chain = fixture::chain();
    let honest = fixture::singleton(&chain.state, ComponentId::HistoryTree);

    let mut empty: RawHistoryTree = bincode::DefaultOptions::new().deserialize(&honest).unwrap();
    empty.peaks.clear();
    let empty = bincode::DefaultOptions::new().serialize(&empty).unwrap();
    assert_early_history_tree_refusal(&chain, &empty, "no peaks");

    for orchard in [false, true] {
        let mut raw: RawHistoryTree = bincode::DefaultOptions::new().deserialize(&honest).unwrap();
        assert!(raw.peaks.len() >= 2, "the tree has peaks to combine");
        for entry in raw.peaks.values_mut() {
            let header = if entry.inner[0] == 1 { 1 } else { 1 + 2 * 4 };
            let branch_id = zebra_chain::parameters::NetworkUpgrade::Nu5
                .branch_id()
                .unwrap()
                .into();
            let mut data =
                zcash_history::NodeDataV2::read(branch_id, &mut &entry.inner[header..]).unwrap();
            if orchard {
                data.orchard_tx = u64::MAX;
            } else {
                data.v1.sapling_tx = u64::MAX;
            }
            let mut bytes = Vec::new();
            data.write(&mut bytes).unwrap();
            entry.inner[header..header + bytes.len()].copy_from_slice(&bytes);
        }
        let tampered = bincode::DefaultOptions::new().serialize(&raw).unwrap();
        assert_early_history_tree_refusal(&chain, &tampered, "transaction counts");
    }
}

/// Writes `tree` as the early bootstrap history-tree piece and checks its decode error.
fn assert_early_history_tree_refusal(chain: &fixture::Chain, tree: &[u8], expected: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = piece_file(dir.path(), &[(Vec::new(), tree.to_vec())]);
    let result = decode_history_tree(&path, &chain.network);
    assert!(
        matches!(result, Err(SnapshotError::InvalidHistoryTree(ref detail)) if detail.contains(expected)),
        "expected an early decode refusal containing {expected:?}, got {result:?}"
    );
}
