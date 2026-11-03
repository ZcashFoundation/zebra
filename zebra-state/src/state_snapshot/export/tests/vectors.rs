//! Exports of small test states: the rows each component ships, determinism
//! across databases and encodings, piece splitting, and the refusals.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};

use bincode::Options;
use semver::Version;
use serde_big_array::BigArray;

use zebra_chain::{
    block::{Block, Height},
    history_tree::NonEmptyHistoryTree,
    parameters::{testnet, Network, NetworkKind},
    primitives::zcash_history::BlockCommitmentTreeRoots,
    sapling,
    serialization::ZcashDeserializeInto,
    state_snapshot::ObjectHash,
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::{
    constants::state_database_format_version_in_code,
    service::finalized_state::{
        DiskWriteBatch, FinalizedState, HistoryTreeParts, IntoDisk, RawBytes, ReadDisk, WriteDisk,
    },
    state_snapshot::{
        export::{
            export_state_snapshot, export_with_window, inconsistent, ComponentWriter, Exporter,
        },
        format::{
            canonical_value, ComponentId, FormatError, Manifest, PieceReader, PieceRecord,
            INDEX_MODE_DEFAULT, MAX_PIECE_BYTES,
        },
        store::ArtifactStore,
        SnapshotError,
    },
    tests::FakeChainHelper,
    write_state_database_format_version_to_disk, Config,
};

/// Asserts that `$result` is an error matching `$pattern`.
macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)? $(,)?) => {{
        let error = $result.expect_err(concat!("expected an error matching ", stringify!($pattern)));
        assert!(
            matches!(&error, $pattern $(if $guard)?),
            "expected {}, got {error:?}",
            stringify!($pattern)
        );
    }};
}

/// The tip of the test chain, which the tests snapshot.
const TIP: Height = Height(14);

/// The window the tests export, short enough to leave blocks below it.
const WINDOW_START: Height = Height(10);

/// The rows of each component, in key order.
type Rows = BTreeMap<ComponentId, Vec<(Vec<u8>, Vec<u8>)>>;

/// Returns the test chain: the Mainnet blocks 0 to 10, then fake blocks 11 to
/// 14.
///
/// Block 12 also spends both outputs of block 1's coinbase, so block 1 has no
/// unspent outputs at the tip, while the coinbases of blocks 2 to 9 do.
fn test_chain() -> Vec<Arc<Block>> {
    let mut blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .range(0..=10)
        .map(|(_height, bytes)| {
            bytes
                .zcash_deserialize_into()
                .expect("the vectors are valid")
        })
        .collect();

    let child = blocks
        .last()
        .expect("the chain is not empty")
        .make_fake_child();
    blocks.push(child);

    let coinbase = blocks[1].transactions[0].clone();
    let outputs = coinbase.outputs();
    let spend = Transaction::test_v1(
        (0..outputs.len())
            .map(|index| transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: coinbase.hash(),
                    index: u32::try_from(index).expect("two outputs"),
                },
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            })
            .collect(),
        vec![outputs[1].clone()],
        LockTime::unlocked(),
    );
    let mut with_spend = blocks
        .last()
        .expect("the chain is not empty")
        .make_fake_child();
    Arc::make_mut(&mut with_spend)
        .transactions
        .push(Arc::new(spend));
    blocks.push(with_spend);

    for _ in 13..=14 {
        let mut child = blocks
            .last()
            .expect("the chain is not empty")
            .make_fake_child();
        // The fake child copies its parent's transactions, but must not spend block 1 again.
        Arc::make_mut(&mut child).transactions.truncate(1);
        blocks.push(child);
    }

    assert_eq!(
        blocks.last().and_then(|block| block.coinbase_height()),
        Some(TIP)
    );
    blocks
}

/// Returns a state config for `cache_dir`.
fn state_config(cache_dir: &Path) -> Config {
    Config {
        cache_dir: cache_dir.to_path_buf(),
        ephemeral: false,
        ..Config::default()
    }
}

/// Opens the state in `config` for writing, without format checks.
fn open_state(config: &Config, network: &Network) -> FinalizedState {
    FinalizedState::new_with_debug(
        config,
        network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .expect("opens the state")
}

/// Writes `blocks` to a new state in `cache_dir`, with the running format
/// version, and returns its config.
fn new_state(cache_dir: &Path, network: &Network, blocks: &[Arc<Block>]) -> Config {
    let config = state_config(cache_dir);
    let mut state = open_state(&config, network);
    for block in blocks {
        state
            .commit_finalized_direct(block.clone().into(), None, "state snapshot export tests")
            .expect("the test chain is valid");
    }
    drop(state);

    write_version(&config, network, &state_database_format_version_in_code());
    config
}

/// Writes `version` as the format version of the state in `config`.
fn write_version(config: &Config, network: &Network, version: &Version) {
    write_state_database_format_version_to_disk(config, version, network)
        .expect("writes the version file");
}

/// Applies `change` to the state in `config` in one batch.
fn change_state(
    config: &Config,
    network: &Network,
    change: impl FnOnce(&FinalizedState, &mut DiskWriteBatch),
) {
    let state = open_state(config, network);
    let mut batch = DiskWriteBatch::new();
    change(&state, &mut batch);
    state.db.write_batch(batch).expect("writes the batch");
}

/// Inserts the raw row `key` → `value` into `cf`.
fn insert(state: &FinalizedState, batch: &mut DiskWriteBatch, cf: &str, key: &[u8], value: &[u8]) {
    let handle = state
        .db
        .db()
        .cf_handle(cf)
        .expect("the column family exists");
    batch.zs_insert(&handle, key, value);
}

/// Deletes the raw row `key` from `cf`.
fn delete(state: &FinalizedState, batch: &mut DiskWriteBatch, cf: &str, key: &[u8]) {
    let handle = state
        .db
        .db()
        .cf_handle(cf)
        .expect("the column family exists");
    batch.zs_delete(&handle, key);
}

/// Returns every row of the column family `cf`.
fn cf_rows(state: &FinalizedState, cf: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
    let handle = state
        .db
        .db()
        .cf_handle(cf)
        .expect("the column family exists");
    state
        .db
        .db()
        .zs_items_in_range_ordered::<_, RawBytes, RawBytes, _>(&handle, ..)
        .into_iter()
        .map(|(key, value)| (key.raw_bytes().clone(), value.raw_bytes().clone()))
        .collect()
}

/// Returns the on-disk transaction location of transaction `index` at
/// `height`.
fn tx_loc(height: u32, index: u16) -> Vec<u8> {
    [&Height(height).as_bytes()[..], &index.to_be_bytes()].concat()
}

/// Returns the height in the first three bytes of an on-disk key or value.
fn height_of(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]])
}

/// Reads the snapshot `manifest` back from `store`: checks every piece's hash
/// and size, reads its records with the strict piece reader, and returns the
/// manifest and the records of each component.
fn read_snapshot(
    store: &ArtifactStore,
    network: &Network,
    manifest: ObjectHash,
) -> (Manifest, Rows) {
    assert!(store.contains_verified(manifest).expect("readable"));
    let bytes = std::fs::read(store.object_path(manifest)).expect("the manifest is stored");
    let manifest = Manifest::decode(&bytes, network).expect("the manifest is valid");

    let mut components = BTreeMap::new();
    for component in &manifest.components {
        let mut records = Vec::new();
        for piece in &component.pieces {
            assert!(store.contains_verified(piece.hash).expect("readable"));
            let bytes = std::fs::read(store.object_path(piece.hash)).expect("the piece is stored");
            assert_eq!(bytes.len(), usize::try_from(piece.size).expect("fits"));

            let mut reader = PieceReader::new(bytes.as_slice());
            while let Some(PieceRecord { key, value }) =
                reader.next_record().expect("the piece is valid")
            {
                records.push((key, value));
            }
            assert_eq!(reader.bytes_read(), bytes.len());
        }

        assert_eq!(
            u64::try_from(records.len()).expect("fits"),
            component.entry_count,
            "{}",
            component.id,
        );
        assert!(
            records.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "{} keys increase across pieces",
            component.id,
        );
        components.insert(component.id, records);
    }

    (manifest, components)
}

/// Returns the rows a snapshot of the state in `config` at [`TIP`] with the
/// window from `window_start` ships, from full scans of the state: the
/// selection rules written out directly, independently of the exporter's
/// merge joins and targeted reads.
fn expected_rows(config: &Config, network: &Network, window_start: Height) -> Rows {
    let state = open_state(config, network);

    let in_window = |height: u32| height == 0 || (window_start.0..=TIP.0).contains(&height);
    let unspent: BTreeSet<Vec<u8>> = cf_rows(&state, "utxo_by_out_loc")
        .keys()
        .map(|key| key[..5].to_vec())
        .collect();
    let shipped_tx = |tx_loc: &[u8]| in_window(height_of(tx_loc)) || unspent.contains(tx_loc);

    let mut expected = BTreeMap::new();
    for component in ComponentId::ALL {
        let rows = cf_rows(&state, component.column_family());
        let selected: Vec<(Vec<u8>, Vec<u8>)> = match component {
            ComponentId::HashByHeight | ComponentId::Headers | ComponentId::Transactions => rows
                .into_iter()
                .filter(|(key, _)| in_window(height_of(key)))
                .collect(),
            ComponentId::BlockInfo => rows
                .into_iter()
                .filter(|(key, _)| {
                    let height = height_of(key);
                    height == 0 || (window_start.0 - 1..=TIP.0).contains(&height)
                })
                .collect(),
            ComponentId::HeightByHash => rows
                .into_iter()
                .filter(|(_, value)| in_window(height_of(value)))
                .collect(),
            ComponentId::HashByTxLoc => rows
                .into_iter()
                .filter(|(key, _)| shipped_tx(key))
                .collect(),
            ComponentId::TxLocByHash => rows
                .into_iter()
                .filter(|(_, value)| shipped_tx(value))
                .collect(),
            ComponentId::AddressTxs => rows
                .into_iter()
                .filter(|(key, _)| in_window(height_of(&key[8..])))
                .collect(),
            ComponentId::SaplingTree | ComponentId::OrchardTree | ComponentId::IronwoodTree => {
                let subtrees = match component {
                    ComponentId::SaplingTree => "sapling_note_commitment_subtree",
                    ComponentId::OrchardTree => "orchard_note_commitment_subtree",
                    _ => "ironwood_note_commitment_subtree",
                };
                let end_heights: BTreeSet<u32> = cf_rows(&state, subtrees)
                    .values()
                    .map(|value| height_of(value))
                    .collect();
                let last_below = rows
                    .keys()
                    .filter(|key| height_of(key) < window_start.0)
                    .max()
                    .cloned();
                rows.into_iter()
                    .filter(|(key, _)| {
                        let height = height_of(key);
                        in_window(height)
                            || end_heights.contains(&height)
                            || Some(key) == last_below.as_ref()
                    })
                    .collect()
            }
            // Every other component is shipped whole, and the singletons are keyed by `()`.
            _ => rows.into_iter().collect(),
        };

        let selected = selected
            .into_iter()
            .map(|(key, value)| {
                if component.is_canonicalized() {
                    let canonical = canonical_value(component, &key, &value).expect("valid");
                    (key, canonical)
                } else {
                    (key, value)
                }
            })
            .collect();
        expected.insert(component, selected);
    }

    expected
}

/// Returns a two-block Heartwood history tree from the Mainnet vectors.
fn heartwood_history_tree() -> NonEmptyHistoryTree {
    let network = Network::Mainnet;
    let (blocks, sapling_roots) = network.block_sapling_roots_map();
    let block = |height: u32| -> Arc<Block> {
        blocks
            .get(&height)
            .expect("vector exists")
            .zcash_deserialize_into()
            .expect("valid block")
    };
    let root = |height: u32| {
        sapling::tree::Root::try_from(**sapling_roots.get(&height).expect("vector exists"))
            .expect("valid root")
    };

    let mut tree = NonEmptyHistoryTree::from_block(
        &network,
        block(903_000),
        BlockCommitmentTreeRoots {
            sapling: &root(903_000),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .expect("valid tree");
    tree.push(
        block(903_001),
        BlockCommitmentTreeRoots {
            sapling: &root(903_001),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .expect("valid tree");
    tree
}

/// The layout of a history tree written before NU6.3, with narrower entries.
#[derive(serde::Serialize)]
struct LegacyParts {
    network_kind: NetworkKind,
    size: u32,
    peaks: BTreeMap<u32, LegacyEntry>,
    current_height: Height,
}

/// A history tree entry written before NU6.3.
#[derive(serde::Serialize)]
struct LegacyEntry {
    #[serde(with = "BigArray")]
    inner: [u8; 253],
}

/// Returns `tree` in the layout written before NU6.3.
fn legacy_history_tree_bytes(tree: &NonEmptyHistoryTree) -> Vec<u8> {
    let options = bincode::DefaultOptions::new();
    let legacy = LegacyParts {
        network_kind: tree.network().kind(),
        size: tree.size(),
        peaks: tree
            .peaks()
            .iter()
            .map(|(index, entry)| {
                let bytes = options.serialize(entry).expect("serializes");
                assert!(bytes[253..].iter().all(|byte| *byte == 0));
                let inner = bytes[..253].try_into().expect("253 bytes");
                (*index, LegacyEntry { inner })
            })
            .collect(),
        current_height: tree.current_height(),
    };
    options.serialize(&legacy).expect("serializes")
}

/// Returns the encoding of a note commitment tree whose cached root was
/// dropped, as trees were written before roots were cached.
fn without_cached_root(tree: &[u8]) -> Vec<u8> {
    let (frontier, cached) = tree.split_at(tree.len() - 33);
    assert_eq!(cached[0], 1, "the tree has a cached root");
    [frontier, &[0]].concat()
}

#[test]
fn export_ships_the_window_genesis_and_the_unspent_transaction_index() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let blocks = test_chain();
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &blocks);
    let store = ArtifactStore::at(dir.path().join("store"));

    let commitment = export_with_window(&config, &Network::Mainnet, TIP, WINDOW_START, &store)
        .expect("exports the state");
    assert_eq!(commitment.height, TIP);
    assert_eq!(commitment.block_hash, blocks[14].hash());

    let (manifest, exported) = read_snapshot(&store, &Network::Mainnet, commitment.manifest);
    assert_eq!(manifest.genesis_hash, Network::Mainnet.genesis_hash());
    assert_eq!(manifest.height, TIP);
    assert_eq!(manifest.block_hash, commitment.block_hash);
    assert_eq!(manifest.window_start, WINDOW_START);
    assert_eq!(
        u64::from(manifest.db_format_major),
        state_database_format_version_in_code().major
    );
    assert_eq!(manifest.index_mode, INDEX_MODE_DEFAULT);
    assert_eq!(manifest.parameters_hash, None);

    // Every component ships exactly the rows the selection rules name.
    let expected = expected_rows(&config, &Network::Mainnet, WINDOW_START);
    for component in ComponentId::ALL {
        assert_eq!(exported[&component], expected[&component], "{component}");
    }

    // A fresh state's values are already canonical.
    let state = open_state(&config, &Network::Mainnet);
    for component in ComponentId::ALL
        .into_iter()
        .filter(|c| c.is_canonicalized())
    {
        for (key, value) in cf_rows(&state, component.column_family()) {
            assert_eq!(
                canonical_value(component, &key, &value).expect("valid"),
                value,
                "{component}",
            );
        }
    }
    drop(state);

    // The selection is not trivial. Below the window, only transactions with unspent outputs
    // have index rows: block 1's coinbase was spent in the window, and blocks 2 to 9 were not.
    let tx_locs: BTreeSet<&Vec<u8>> = exported[&ComponentId::HashByTxLoc]
        .iter()
        .map(|(key, _)| key)
        .collect();
    assert!(!tx_locs.contains(&tx_loc(1, 0)));
    for height in 2..=9 {
        assert!(tx_locs.contains(&tx_loc(height, 0)), "{height}");
    }
    assert!(tx_locs.contains(&tx_loc(0, 0)), "genesis is shipped");
    assert!(tx_locs.contains(&tx_loc(12, 1)), "the window's spend");
    // Genesis, eight unspent coinbases, and six window transactions.
    assert_eq!(tx_locs.len(), 1 + 8 + 6);
    assert_eq!(exported[&ComponentId::TxLocByHash].len(), tx_locs.len());
    assert_eq!(exported[&ComponentId::Transactions].len(), 1 + 6);

    // `block_info` ships the window's parent, which has no hash rows.
    let heights = |component: ComponentId| -> Vec<u32> {
        exported[&component]
            .iter()
            .map(|(key, _)| height_of(key))
            .collect()
    };
    assert_eq!(heights(ComponentId::BlockInfo), [0, 9, 10, 11, 12, 13, 14]);
    assert_eq!(heights(ComponentId::HashByHeight), [0, 10, 11, 12, 13, 14]);
    assert_eq!(heights(ComponentId::Headers), [0, 10, 11, 12, 13, 14]);
    assert_eq!(exported[&ComponentId::HeightByHash].len(), 6);

    // The address transactions of the window only, while the UTXO and balance indexes are whole.
    assert!(!exported[&ComponentId::AddressTxs].is_empty());
    assert!(exported[&ComponentId::AddressTxs]
        .iter()
        .all(|(key, _)| height_of(&key[8..]) >= WINDOW_START.0));
    assert!(exported[&ComponentId::AddressUtxos]
        .iter()
        .any(|(key, _)| height_of(&key[8..]) < WINDOW_START.0));

    // The genesis trees are below the window, and the tip tree is the genesis tree.
    for component in [
        ComponentId::SaplingTree,
        ComponentId::OrchardTree,
        ComponentId::IronwoodTree,
    ] {
        assert_eq!(heights(component), [0], "{component}");
    }
    assert!(
        exported[&ComponentId::HistoryTree].is_empty(),
        "pre-Heartwood"
    );
    for component in [ComponentId::SproutTipTree, ComponentId::ValuePools] {
        assert_eq!(exported[&component].len(), 1, "{component}");
        assert!(exported[&component][0].0.is_empty(), "keyed by ()");
    }
}

#[test]
fn export_ships_the_trees_the_format_checks_and_the_window_read() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &test_chain());

    // The test chain has no shielded notes, so give the pools tree rows: Sapling changes at 2, 3,
    // 5, 11 and 13 with a subtree ending at 3, and Orchard changes at 6 only. The genesis trees
    // are valid trees for every row.
    let height_key = |height: u32| Height(height).as_bytes().to_vec();
    let subtree = |end_height: u32| [height_key(end_height), vec![0x11; 32]].concat();
    change_state(&config, &Network::Mainnet, |state, batch| {
        for (cf, heights) in [
            ("sapling_note_commitment_tree", &[2, 3, 5, 11, 13][..]),
            ("orchard_note_commitment_tree", &[6][..]),
        ] {
            let genesis = cf_rows(state, cf)[&height_key(0)].clone();
            for height in heights {
                insert(state, batch, cf, &height_key(*height), &genesis);
            }
        }
        insert(
            state,
            batch,
            "sapling_note_commitment_subtree",
            &0_u16.to_be_bytes(),
            &subtree(3),
        );
    });

    let store = ArtifactStore::at(dir.path().join("store"));
    let commitment =
        export_with_window(&config, &network, TIP, WINDOW_START, &store).expect("exports");
    let (_manifest, exported) = read_snapshot(&store, &Network::Mainnet, commitment.manifest);

    let expected = expected_rows(&config, &Network::Mainnet, WINDOW_START);
    for component in ComponentId::ALL {
        assert_eq!(exported[&component], expected[&component], "{component}");
    }

    // Genesis, the subtree's end, the tree in effect below the window, and the window; not the
    // tree at 2, which nothing reads.
    let heights = |component: ComponentId| -> Vec<u32> {
        exported[&component]
            .iter()
            .map(|(key, _)| height_of(key))
            .collect()
    };
    assert_eq!(heights(ComponentId::SaplingTree), [0, 3, 5, 11, 13]);
    // The tree in effect below the window is also the tip tree.
    assert_eq!(heights(ComponentId::OrchardTree), [0, 6]);
    assert_eq!(heights(ComponentId::IronwoodTree), [0]);
    assert_eq!(exported[&ComponentId::SaplingSubtrees].len(), 1);

    // A subtree whose end height has no tree is refused.
    change_state(&config, &Network::Mainnet, |state, batch| {
        insert(
            state,
            batch,
            "sapling_note_commitment_subtree",
            &1_u16.to_be_bytes(),
            &subtree(4),
        );
    });
    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::Inconsistent {
            component: ComponentId::SaplingTree,
            ..
        },
    );
}

#[test]
fn export_state_snapshot_ships_every_block_of_a_short_chain() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &test_chain());
    let store = ArtifactStore::at(dir.path().join("store"));

    // The chain is shorter than the window, so the window starts at block 1.
    let commitment =
        export_state_snapshot(&config, &Network::Mainnet, TIP, &store).expect("exports");
    let (manifest, exported) = read_snapshot(&store, &Network::Mainnet, commitment.manifest);
    assert_eq!(manifest.window_start, Height(1));

    let expected = expected_rows(&config, &Network::Mainnet, Height(1));
    for component in ComponentId::ALL {
        assert_eq!(exported[&component], expected[&component], "{component}");
    }
    assert_eq!(exported[&ComponentId::Transactions].len(), 16);
    assert_eq!(exported[&ComponentId::HashByTxLoc].len(), 16);
}

#[test]
fn exports_are_identical_across_databases_with_legacy_encodings() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let blocks = test_chain();
    let history_tree = heartwood_history_tree();

    // Database A is written in the current encodings, with a history tree, and with address
    // balances whose `received` totals are zero, which the layout before the total also holds.
    let balances_cf = "balance_by_transparent_addr";
    let current = new_state(&dir.path().join("current"), &Network::Mainnet, &blocks);
    change_state(&current, &Network::Mainnet, |state, batch| {
        insert(
            state,
            batch,
            "history_tree",
            &[],
            &HistoryTreeParts::from(&history_tree).as_bytes(),
        );
        let balances = cf_rows(state, balances_cf);
        assert!(!balances.is_empty(), "the test chain has address balances");
        for (key, value) in balances {
            insert(
                state,
                batch,
                balances_cf,
                &key,
                &[&value[..16], &[0; 8]].concat(),
            );
        }
    });

    // Database B holds the same state with the legacy encodings the state still reads: narrower
    // value pools and block info, a history tree with pre-NU6.3 entries, and trees without
    // cached roots.
    let legacy = new_state(&dir.path().join("legacy"), &Network::Mainnet, &blocks);
    let source = open_state(&legacy, &Network::Mainnet);
    let pools = cf_rows(&source, "tip_chain_value_pool");
    let block_info = cf_rows(&source, "block_info");
    let trees: Vec<(&str, BTreeMap<Vec<u8>, Vec<u8>>)> = [
        "sapling_note_commitment_tree",
        "orchard_note_commitment_tree",
        "ironwood_note_commitment_tree",
        "sprout_note_commitment_tree",
        "sprout_anchors",
    ]
    .into_iter()
    .map(|cf| (cf, cf_rows(&source, cf)))
    .collect();
    drop(source);

    change_state(&legacy, &Network::Mainnet, |state, batch| {
        for (key, value) in &pools {
            // The newer pools are empty in this chain, so the older layouts hold the same value.
            assert!(value[48..].iter().all(|byte| *byte == 0));
            insert(state, batch, "tip_chain_value_pool", key, &value[..48]);
        }
        for (index, (key, value)) in block_info.iter().enumerate() {
            assert!(value[40..48]
                .iter()
                .chain(&value[52..])
                .all(|byte| *byte == 0));
            let legacy_value = if index % 2 == 0 {
                // Format 28: six pools and the size.
                value[..52].to_vec()
            } else {
                // Before NU6.3: five pools and the size.
                [&value[..40], &value[48..52]].concat()
            };
            insert(state, batch, "block_info", key, &legacy_value);
        }
        for (cf, rows) in &trees {
            for (key, value) in rows {
                insert(state, batch, cf, key, &without_cached_root(value));
            }
        }
        insert(
            state,
            batch,
            "history_tree",
            &[],
            &legacy_history_tree_bytes(&history_tree),
        );
        // Before the `received` total, address balances were the balance and the location.
        for (key, value) in cf_rows(state, balances_cf) {
            insert(state, batch, balances_cf, &key, &value[..16]);
        }

        // Formats before 1.4.0 keyed the Sprout tip tree by the heights it changed at, and the
        // state reads the last of those rows, so the earlier row is never read.
        let sprout_cf = "sprout_note_commitment_tree";
        let sprout_tip = trees
            .iter()
            .find(|(cf, _rows)| *cf == sprout_cf)
            .map(|(_cf, rows)| without_cached_root(&rows[&Vec::new()]))
            .expect("the Sprout tip tree is listed above");
        delete(state, batch, sprout_cf, &[]);
        insert(state, batch, sprout_cf, &Height(2).as_bytes(), &[0xFF; 3]);
        insert(state, batch, sprout_cf, &Height(7).as_bytes(), &sprout_tip);
    });

    // The rewrites took effect.
    let source = open_state(&legacy, &Network::Mainnet);
    assert!(cf_rows(&source, "tip_chain_value_pool")
        .values()
        .all(|value| value.len() == 48));
    assert!(cf_rows(&source, "block_info")
        .values()
        .all(|value| value.len() < 60));
    assert!(cf_rows(&source, "sprout_note_commitment_tree")
        .keys()
        .all(|key| key.len() == 3));
    assert!(cf_rows(&source, balances_cf)
        .values()
        .all(|value| value.len() == 16));
    assert_eq!(
        source.db.sprout_tree_for_tip(),
        open_state(&current, &Network::Mainnet)
            .db
            .sprout_tree_for_tip(),
        "the state reads the same Sprout tip tree from both databases",
    );
    drop(source);

    let network = Network::Mainnet;
    let first = export_with_window(
        &current,
        &network,
        TIP,
        WINDOW_START,
        &ArtifactStore::at(dir.path().join("store-1")),
    )
    .expect("exports database A");
    let again = export_with_window(
        &current,
        &network,
        TIP,
        WINDOW_START,
        &ArtifactStore::at(dir.path().join("store-2")),
    )
    .expect("exports database A again");
    let legacy_store = ArtifactStore::at(dir.path().join("store-3"));
    let from_legacy = export_with_window(&legacy, &network, TIP, WINDOW_START, &legacy_store)
        .expect("exports database B");

    assert_eq!(first, again, "exports of one database are identical");
    assert_eq!(
        first, from_legacy,
        "exports of the same state are identical whatever its encodings",
    );

    // The history tree and the address balances were shipped, so their canonicalization was
    // exercised.
    let (_manifest, exported) =
        read_snapshot(&legacy_store, &Network::Mainnet, from_legacy.manifest);
    assert_eq!(
        exported[&ComponentId::HistoryTree],
        [(Vec::new(), HistoryTreeParts::from(&history_tree).as_bytes())],
    );
    assert!(!exported[&ComponentId::AddressBalances].is_empty());
    assert!(exported[&ComponentId::AddressBalances]
        .iter()
        .all(|(_key, value)| value.len() == 24 && value[16..] == [0; 8]));
}

/// A Regtest snapshot carries the network's parameters hash, so its manifest
/// decodes on Regtest and nowhere else.
#[test]
fn regtest_exports_carry_the_parameters_hash() {
    let _init_guard = zebra_test::init();

    // Heartwood activates above the tip, so the fake blocks need no chain history commitment.
    let network = Network::new_regtest(testnet::RegtestParameters {
        activation_heights: testnet::ConfiguredActivationHeights {
            heartwood: Some(100),
            ..Default::default()
        },
        ..Default::default()
    });
    let dir = tempfile::tempdir().expect("creates a temporary directory");

    // The Regtest genesis block, then fake blocks up to the tip.
    let mut blocks = vec![zebra_chain::block::genesis::regtest_genesis_block()];
    while blocks.len() <= usize::try_from(TIP.0).expect("fits") {
        let child = blocks
            .last()
            .expect("the chain is not empty")
            .make_fake_child();
        blocks.push(child);
    }
    let config = new_state(&dir.path().join("cache"), &network, &blocks);
    let store = ArtifactStore::at(dir.path().join("store"));

    let commitment = export_state_snapshot(&config, &network, TIP, &store).expect("exports");
    assert_eq!(commitment.height, TIP);
    assert_eq!(commitment.block_hash, blocks[14].hash());

    let (manifest, exported) = read_snapshot(&store, &network, commitment.manifest);
    assert_eq!(manifest.genesis_hash, network.genesis_hash());
    assert_eq!(manifest.window_start, Height(1));
    assert!(manifest.parameters_hash.is_some());
    assert_eq!(
        manifest.parameters_hash,
        network.state_snapshot_parameters_hash()
    );
    assert_eq!(exported[&ComponentId::HashByHeight].len(), 15);

    // Without the parameters hash, a Mainnet manifest has a different layout.
    let bytes = std::fs::read(store.object_path(commitment.manifest)).expect("stored");
    assert!(Manifest::decode(&bytes, &Network::Mainnet).is_err());
}

/// A failed component stops the other components' scans, so a refusal known
/// in seconds does not wait for hours of reads, and the failure, not the
/// cancellation, is what the export reports.
#[test]
fn a_failed_component_stops_the_other_scans() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &test_chain());
    let store = ArtifactStore::at(dir.path().join("store"));
    let state = open_state(&config, &Network::Mainnet);
    let exporter = Exporter {
        db: &state.db,
        store: &store,
        height: TIP,
        window_start: WINDOW_START,
        cancelled: AtomicBool::new(false),
    };

    // A scan whose row fails returns the failure, and stops the others before they read a row.
    let mut rows = 0;
    assert_err!(
        exporter.scan("hash_by_height", None, None, |_key, _value| {
            rows += 1;
            Err(inconsistent(ComponentId::HashByHeight, "a test failure"))
        }),
        SnapshotError::Inconsistent {
            component: ComponentId::HashByHeight,
            ..
        },
    );
    assert_eq!(rows, 1);
    assert_err!(
        exporter.scan("block_info", None, None, |_key, _value| {
            panic!("a cancelled scan reads no rows")
        }),
        SnapshotError::Cancelled,
    );

    // An export whose components fail concurrently reports a failure, not a cancellation: see
    // `export_refuses_missing_and_contradictory_rows`.
}

/// The exporter writes the network's parameters hash into the manifest, and the
/// decoder reads one only on the networks that need it, so the two must agree
/// on every kind of network for exported manifests to decode.
#[test]
fn manifests_carry_a_parameters_hash_exactly_where_the_decoder_reads_one() {
    let _init_guard = zebra_test::init();

    let custom_testnet = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("the parameters are valid");
    assert!(!custom_testnet.is_default_testnet());

    for network in [
        Network::Mainnet,
        Network::new_default_testnet(),
        Network::new_regtest(Default::default()),
        custom_testnet,
    ] {
        assert_eq!(
            network.state_snapshot_parameters_hash().is_some(),
            Manifest::needs_parameters_hash(&network),
            "{network}",
        );
    }
}

#[test]
fn components_split_into_pieces_at_record_boundaries() {
    let _init_guard = zebra_test::init();

    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let store = ArtifactStore::at(dir.path().to_path_buf());

    // Two of these records fit in a piece, and a third does not.
    let value = vec![0xAB; 6 << 20];
    let record_len = PieceRecord::encoded_len(1, value.len()).expect("fits in a piece");
    assert!(2 * record_len <= MAX_PIECE_BYTES && 3 * record_len > MAX_PIECE_BYTES);

    let mut writer = ComponentWriter::new(ComponentId::Transactions, &store);
    for key in [b"a", b"b", b"c"] {
        writer.push(key, &value).expect("the record is valid");
    }
    assert_err!(
        writer.push(b"b", b""),
        SnapshotError::Format(FormatError::KeyNotIncreasing),
    );
    let manifest = writer.finish().expect("stores the pieces");

    assert_eq!(manifest.id, ComponentId::Transactions);
    assert_eq!(manifest.entry_count, 3);
    let sizes: Vec<usize> = manifest
        .pieces
        .iter()
        .map(|piece| usize::try_from(piece.size).expect("fits"))
        .collect();
    assert_eq!(sizes, [2 * record_len, record_len]);
    for piece in &manifest.pieces {
        assert!(store.contains_verified(piece.hash).expect("readable"));
    }

    // A record that fits in no piece cannot be exported.
    let mut writer = ComponentWriter::new(ComponentId::Transactions, &store);
    assert_err!(
        writer.push(b"a", &vec![0; MAX_PIECE_BYTES]),
        SnapshotError::Format(FormatError::RecordTooLarge { .. }),
    );

    // An empty component has no pieces.
    let empty = ComponentWriter::new(ComponentId::Utxos, &store)
        .finish()
        .expect("finishes");
    assert_eq!(empty.entry_count, 0);
    assert!(empty.pieces.is_empty());
}

#[test]
fn export_refuses_the_wrong_height_and_unreadable_formats() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &test_chain());
    let store = ArtifactStore::at(dir.path().join("store"));

    // The snapshot height must be the tip, above genesis.
    assert_err!(
        export_with_window(&config, &network, Height(13), Height(9), &store),
        SnapshotError::TipMismatch { height, tip } if *height == Height(13) && *tip == TIP,
    );
    assert_err!(
        export_state_snapshot(&config, &network, Height(0), &store),
        SnapshotError::GenesisHeight,
    );

    // There must be a state.
    assert_err!(
        export_with_window(
            &state_config(&dir.path().join("missing")),
            &network,
            TIP,
            WINDOW_START,
            &store,
        ),
        SnapshotError::StateOpen(_),
    );

    // A spending index, an older format that needs a migration, and a newer format.
    let running = state_database_format_version_in_code();
    let mut indexed = running.clone();
    indexed.build = semver::BuildMetadata::new("indexer").expect("valid metadata");
    write_version(&config, &Network::Mainnet, &indexed);
    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::SpendingIndex { disk_version } if *disk_version == indexed,
    );

    let migrating = Version::new(27, 0, 0);
    write_version(&config, &Network::Mainnet, &migrating);
    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::NeedsMigration { disk_version, .. } if *disk_version == migrating,
    );

    let newer = Version::new(running.major, running.minor + 1, 0);
    write_version(&config, &Network::Mainnet, &newer);
    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::NewerFormat { disk_version, .. } if *disk_version == newer,
    );

    // Nothing was written by the refused exports.
    assert!(!store.objects_dir().exists());

    // The running format exports.
    write_version(&config, &Network::Mainnet, &running);
    export_with_window(&config, &network, TIP, WINDOW_START, &store).expect("exports");
}

#[test]
fn export_refuses_a_state_pruned_above_the_window() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let blocks = test_chain();
    let config = new_state(&dir.path().join("cache"), &Network::Mainnet, &blocks);
    let store = ArtifactStore::at(dir.path().join("store"));

    // Prune the bodies of blocks 1 to 10, as the pruning worker does.
    change_state(&config, &Network::Mainnet, |state, batch| {
        for (key, _value) in cf_rows(state, "tx_by_loc") {
            if (1..=10).contains(&height_of(&key)) {
                delete(state, batch, "tx_by_loc", &key);
            }
        }
    });

    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::PrunedBelowWindow { window_start, lowest_retained_height }
            if *window_start == WINDOW_START && *lowest_retained_height == Height(11),
    );

    // A window that starts at the first retained body exports.
    let commitment =
        export_with_window(&config, &network, TIP, Height(11), &store).expect("exports");
    let (manifest, _exported) = read_snapshot(&store, &Network::Mainnet, commitment.manifest);
    assert_eq!(manifest.window_start, Height(11));
}

#[test]
fn export_refuses_missing_and_contradictory_rows() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let dir = tempfile::tempdir().expect("creates a temporary directory");
    let blocks = test_chain();

    let height_key = |height: u32| Height(height).as_bytes().to_vec();
    let cases = [
        ("block_info", height_key(12), ComponentId::BlockInfo),
        ("block_info", height_key(9), ComponentId::BlockInfo),
        (
            "block_header_by_height",
            height_key(11),
            ComponentId::Headers,
        ),
        ("tx_by_loc", tx_loc(13, 0), ComponentId::Transactions),
        ("hash_by_tx_loc", tx_loc(2, 0), ComponentId::HashByTxLoc),
        ("hash_by_tx_loc", tx_loc(11, 0), ComponentId::HashByTxLoc),
        (
            "height_by_hash",
            blocks[10].hash().0.to_vec(),
            ComponentId::HeightByHash,
        ),
    ];

    for (index, (cf, key, component)) in cases.into_iter().enumerate() {
        let config = new_state(
            &dir.path().join(format!("cache-{index}")),
            &Network::Mainnet,
            &blocks,
        );
        change_state(&config, &Network::Mainnet, |state, batch| {
            delete(state, batch, cf, &key)
        });

        let store = ArtifactStore::at(dir.path().join(format!("store-{index}")));
        assert_err!(
            export_with_window(&config, &network, TIP, WINDOW_START, &store),
            SnapshotError::Inconsistent { component: found, .. } if *found == component,
        );
    }

    // A malformed key in place of a missing row is refused too: the row count is right, but each
    // key must be the next height.
    let config = new_state(&dir.path().join("cache-key"), &Network::Mainnet, &blocks);
    change_state(&config, &Network::Mainnet, |state, batch| {
        let value = cf_rows(state, "block_info")[&height_key(13)].clone();
        delete(state, batch, "block_info", &height_key(12));
        let malformed = [height_key(12), vec![0]].concat();
        insert(state, batch, "block_info", &malformed, &value);
    });
    assert_err!(
        export_with_window(
            &config,
            &network,
            TIP,
            WINDOW_START,
            &ArtifactStore::at(dir.path().join("store-key")),
        ),
        SnapshotError::Inconsistent {
            component: ComponentId::BlockInfo,
            ..
        },
    );

    // A set row with a value, as a spending index stores for nullifiers, is refused.
    let config = new_state(&dir.path().join("cache-set"), &Network::Mainnet, &blocks);
    change_state(&config, &Network::Mainnet, |state, batch| {
        insert(
            state,
            batch,
            "sapling_nullifiers",
            &[0x11; 32],
            &tx_loc(12, 1),
        );
    });
    assert_err!(
        export_with_window(
            &config,
            &network,
            TIP,
            WINDOW_START,
            &ArtifactStore::at(dir.path().join("store-set")),
        ),
        SnapshotError::Inconsistent {
            component: ComponentId::SaplingNullifiers,
            ..
        },
    );

    // A state whose genesis block is not the network's is refused before any row is read.
    let config = new_state(
        &dir.path().join("cache-genesis"),
        &Network::Mainnet,
        &blocks,
    );
    change_state(&config, &Network::Mainnet, |state, batch| {
        insert(state, batch, "hash_by_height", &height_key(0), &[0x22; 32]);
    });
    let store = ArtifactStore::at(dir.path().join("store-genesis"));
    assert_err!(
        export_with_window(&config, &network, TIP, WINDOW_START, &store),
        SnapshotError::Inconsistent {
            component: ComponentId::HashByHeight,
            ..
        },
    );
    assert!(!store.objects_dir().exists());

    // The state reads the value pools only from the `()` key, so a row under another key is
    // refused rather than shipped in its place.
    let config = new_state(&dir.path().join("cache-pools"), &Network::Mainnet, &blocks);
    change_state(&config, &Network::Mainnet, |state, batch| {
        let value = cf_rows(state, "tip_chain_value_pool")[&Vec::new()].clone();
        delete(state, batch, "tip_chain_value_pool", &[]);
        insert(
            state,
            batch,
            "tip_chain_value_pool",
            &height_key(14),
            &value,
        );
    });
    assert_err!(
        export_with_window(
            &config,
            &network,
            TIP,
            WINDOW_START,
            &ArtifactStore::at(dir.path().join("store-pools")),
        ),
        SnapshotError::Inconsistent {
            component: ComponentId::ValuePools,
            ..
        },
    );

    // A tree with a wrong cached root is refused, rather than shipped or silently fixed.
    let config = new_state(&dir.path().join("cache-tree"), &Network::Mainnet, &blocks);
    change_state(&config, &Network::Mainnet, |state, batch| {
        let rows = cf_rows(state, "sapling_note_commitment_tree");
        let (key, value) = rows.iter().next().expect("the genesis tree");
        let mut wrong = value.clone();
        let wrong_root: [u8; 32] = **Network::Mainnet
            .block_sapling_roots_map()
            .1
            .get(&419_201)
            .expect("vector exists");
        let len = wrong.len();
        wrong[len - 32..].copy_from_slice(&wrong_root);
        insert(state, batch, "sapling_note_commitment_tree", key, &wrong);
    });
    assert_err!(
        export_with_window(
            &config,
            &network,
            TIP,
            WINDOW_START,
            &ArtifactStore::at(dir.path().join("store-tree")),
        ),
        SnapshotError::Format(FormatError::WrongCachedRoot {
            component: ComponentId::SaplingTree
        }),
    );
}
