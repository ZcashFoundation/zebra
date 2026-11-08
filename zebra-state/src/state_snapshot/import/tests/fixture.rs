//! A populated Regtest database, and a test exporter that writes it as state
//! snapshot pieces with the format module.
//!
//! The chain pays every coinbase to one of a few transparent addresses, and
//! every seventh block spends an earlier coinbase into an address output and
//! an output without an address. So the snapshot has unspent outputs below its
//! window, spent outputs, several transactions per block, and outputs the
//! address indexes skip.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use zebra_chain::{
    amount::Amount,
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    orchard,
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, RegtestParameters},
        Network, NetworkKind, NetworkUpgrade,
    },
    sapling, sprout,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{self, LockTime, Transaction},
    transparent,
};

use crate::{
    service::finalized_state::{
        height_from_bytes_prefix, DiskWriteBatch, FinalizedState, OutputLocation, RawBytes,
        WriteDisk,
    },
    state_database_format_version_in_code,
    state_snapshot::{
        format::{
            ComponentId, ComponentManifest, Manifest, PieceManifest, PieceWriter,
            INDEX_MODE_DEFAULT,
        },
        import::{import_dir, import_state_snapshot, window_start},
        SnapshotBase, SnapshotError,
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config, FromDisk, IntoDisk, MAX_BLOCK_REORG_HEIGHT,
};

/// The snapshot height: high enough that the window starts above height one.
pub(super) const HEIGHT: Height = Height(2_330);

/// The number of transparent addresses the chain pays to.
const ADDRESSES: u32 = 5;

/// The records of each component, in key order.
pub(super) type Records = BTreeMap<ComponentId, Vec<(Vec<u8>, Vec<u8>)>>;

/// A Regtest network with NU5 active from height one, and a checkpoint above
/// the snapshot height, which the import preconditions require.
pub(super) fn network() -> Network {
    network_with(vec![checkpoint_above_snapshot()])
}

/// Returns the fixture network's checkpoint above the snapshot height. The
/// importer checks only the checkpoint at the snapshot height, so its hash
/// stands in for a block the fixture never builds.
pub(super) fn checkpoint_above_snapshot() -> (Height, block::Hash) {
    (Height(HEIGHT.0 + 1), block::Hash([0xcc; 32]))
}

/// The fixture network with `checkpoints` after genesis, in height order. The
/// snapshot meets the import preconditions only if one is above its height.
pub(super) fn network_with(checkpoints: Vec<(Height, block::Hash)>) -> Network {
    regtest(1, checkpoints)
}

/// The fixture network with NU5 activating at `nu5` instead of height one.
pub(super) fn network_with_nu5_at(nu5: u32) -> Network {
    regtest(nu5, vec![checkpoint_above_snapshot()])
}

/// The fixture network with NU6.3 active from height one.
pub(super) fn ironwood_network() -> Network {
    regtest_with_nu6_3(1, Some(1), vec![checkpoint_above_snapshot()])
}

fn regtest(nu5: u32, checkpoints: Vec<(Height, block::Hash)>) -> Network {
    regtest_with_nu6_3(nu5, None, checkpoints)
}

fn regtest_with_nu6_3(
    nu5: u32,
    nu6_3: Option<u32>,
    checkpoints: Vec<(Height, block::Hash)>,
) -> Network {
    let genesis = block::genesis::regtest_genesis_block().hash();
    Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(nu5),
            nu6: nu6_3.map(|_| 1),
            nu6_3,
            ..Default::default()
        },
        funding_streams: Some(Vec::new()),
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(
            std::iter::once((Height(0), genesis))
                .chain(checkpoints)
                .collect(),
        )),
        ..Default::default()
    })
}

/// Returns the address that the coinbase at `height` pays.
pub(super) fn address(height: u32) -> transparent::Address {
    // The remainder is below `ADDRESSES`, which fits in a byte.
    transparent::Address::from_pub_key_hash(
        NetworkKind::Regtest,
        [(height % ADDRESSES) as u8 + 1; 20],
    )
}

/// Returns the value the coinbase at `height` pays.
pub(super) fn coinbase_value(height: u32) -> u64 {
    1_000 + u64::from(height)
}

fn output(value: u64, address: &transparent::Address) -> transparent::Output {
    transparent::Output {
        value: Amount::try_from(value).expect("fixture values are valid amounts"),
        lock_script: address.script(),
    }
}

/// An output whose script has no address.
fn bare_output(value: u64) -> transparent::Output {
    transparent::Output {
        value: Amount::try_from(value).expect("fixture values are valid amounts"),
        lock_script: transparent::Script::new(&[0x51]),
    }
}

/// A source chain at the snapshot height.
pub(super) struct Chain {
    pub(super) network: Network,
    pub(super) state: FinalizedState,
    /// The coinbase transaction hash of each block, by height.
    pub(super) coinbases: Vec<transaction::Hash>,
    /// The history tree record of the block below the snapshot height.
    pub(super) history_tree_below_tip: Vec<u8>,
    _cache: tempfile::TempDir,
}

/// Builds the source chain up to [`HEIGHT`].
pub(super) fn chain() -> Chain {
    chain_with(|_, _| {})
}

/// Builds the source chain up to [`HEIGHT`], calling `after_commit` with the
/// state after each block is committed.
pub(super) fn chain_with(after_commit: impl FnMut(&FinalizedState, Height)) -> Chain {
    chain_with_network(network(), after_commit)
}

/// Builds a source chain on `network`, calling `after_commit` after each block.
pub(super) fn chain_with_network(
    network: Network,
    mut after_commit: impl FnMut(&FinalizedState, Height),
) -> Chain {
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        ..Config::default()
    };
    let mut state = FinalizedState::new_with_debug(
        &config,
        &network,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )
    .unwrap();
    state
        .db
        .update_format_version_on_disk(&state_database_format_version_in_code())
        .unwrap();
    state.db.mark_finished_format_upgrades();

    let genesis = block::genesis::regtest_genesis_block();
    commit(&mut state, genesis.clone());
    let mut coinbases = vec![genesis.transactions[0].hash()];
    let mut parent = genesis;
    let mut history_tree_below_tip = Vec::new();
    for height in 1..=HEIGHT.0 {
        if height == HEIGHT.0 {
            history_tree_below_tip = singleton(&state, ComponentId::HistoryTree);
        }
        let mut transactions = Vec::new();
        if height % 7 == 0 && height > 10 {
            let spent = height - 5;
            transactions.push(spend(
                &network,
                height,
                // `spent` is a height below `HEIGHT`, which fits in a `usize`.
                coinbases[spent as usize],
                vec![
                    output(coinbase_value(spent) - 100, &address(height + 1)),
                    bare_output(100),
                ],
            ));
        }
        let block = child(
            &network,
            &state,
            &parent,
            vec![output(coinbase_value(height), &address(height))],
            transactions,
        );
        coinbases.push(block.transactions[0].hash());
        commit(&mut state, block.clone());
        after_commit(&state, Height(height));
        parent = block;
    }

    Chain {
        network,
        state,
        coinbases,
        history_tree_below_tip,
        _cache: cache,
    }
}

/// Returns the block above the snapshot height, which spends the coinbase at
/// height 3, below the window.
pub(super) fn successor(chain: &Chain) -> Arc<Block> {
    let parent = chain.state.db.tip_block().unwrap().unwrap();
    let height = HEIGHT.0 + 1;
    let spend = spend(
        &chain.network,
        height,
        chain.coinbases[3],
        vec![output(coinbase_value(3), &address(0))],
    );
    child(
        &chain.network,
        &chain.state,
        &parent,
        vec![output(coinbase_value(height), &address(height))],
        vec![spend],
    )
}

fn commit(state: &mut FinalizedState, block: Arc<Block>) {
    state
        .commit_finalized_direct(
            CheckpointVerifiedBlock::from(block).into(),
            None,
            "state snapshot import fixture",
        )
        .unwrap();
}

fn spend(
    network: &Network,
    height: u32,
    spent: transaction::Hash,
    outputs: Vec<transparent::Output>,
) -> Transaction {
    Transaction::test_v5(
        NetworkUpgrade::current(network, Height(height)),
        vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: spent,
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        outputs,
        LockTime::unlocked(),
        Height(height),
    )
}

/// Returns the child of `parent` with a coinbase paying `coinbase`, then
/// `transactions`, committing to the history tree of `state`.
fn child(
    network: &Network,
    state: &FinalizedState,
    parent: &Block,
    coinbase: Vec<transparent::Output>,
    transactions: Vec<Transaction>,
) -> Arc<Block> {
    let height = parent.coinbase_height().unwrap().next().unwrap();
    let coinbase = Transaction::test_v5(
        NetworkUpgrade::current(network, height),
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        coinbase,
        LockTime::unlocked(),
        height,
    );
    let mut block = Block {
        header: parent.header.clone(),
        transactions: std::iter::once(coinbase)
            .chain(transactions)
            .map(Arc::new)
            .collect(),
    };
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = parent.hash();
    header.time += chrono::Duration::seconds(75);
    header.merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    let root = state
        .db
        .history_tree()
        .hash()
        .unwrap_or_else(|| block::CHAIN_HISTORY_ACTIVATION_RESERVED.into());
    let commitment =
        ChainHistoryBlockTxAuthCommitmentHash::from_commitments(&root, &block.auth_data_root());
    Arc::new(block).set_block_commitment(commitment.into())
}

/// Returns every record of `component` in `state`, in key order.
pub(super) fn rows(state: &FinalizedState, component: ComponentId) -> Vec<(Vec<u8>, Vec<u8>)> {
    let cf = state.db.cf_handle(component.column_family()).unwrap();
    state
        .db
        .zs_forward_range_iter::<_, RawBytes, RawBytes, _>(&cf, ..)
        .map(|(key, value)| (key.raw_bytes().clone(), value.raw_bytes().clone()))
        .collect()
}

/// Returns the value of the singleton `component` in `state`.
pub(super) fn singleton(state: &FinalizedState, component: ComponentId) -> Vec<u8> {
    let mut rows = rows(state, component);
    assert_eq!(rows.len(), 1, "{component} is a singleton");
    rows.remove(0).1
}

fn height_of(bytes: &[u8]) -> Height {
    height_from_bytes_prefix(bytes).unwrap().0
}

/// Selects the rows a snapshot at the tip of `state` ships, by the design's
/// row-selection rules ("Snapshot contents"): the hashes and headers of
/// genesis and the window `L..=H`, with the block info of `L - 1`; the bodies
/// of genesis and `H`; the transaction index rows of those bodies and of every
/// transaction with an unspent output; the trees at genesis, at each
/// subtree end, as of `L`, and above `L`; and every other component whole.
pub(super) fn select(state: &FinalizedState) -> Records {
    use ComponentId::*;

    let (height, _) = state.db.tip().unwrap();
    let start = window_start(height);
    let in_window = |height: Height| height.is_min() || height >= start;
    let stored = |block: Height| block.is_min() || block == height;
    let utxo_transactions: BTreeSet<Vec<u8>> = rows(state, Utxos)
        .into_iter()
        .map(|(key, _)| {
            OutputLocation::from_bytes(key)
                .transaction_location()
                .as_bytes()
        })
        .collect();

    let mut records = Records::new();
    for component in ComponentId::ALL {
        let all = rows(state, component);
        let selected: Vec<_> = match component {
            HashByHeight | Headers => all
                .into_iter()
                .filter(|(key, _)| in_window(height_of(key)))
                .collect(),
            Transactions => all
                .into_iter()
                .filter(|(key, _)| stored(height_of(key)))
                .collect(),
            HeightByHash => all
                .into_iter()
                .filter(|(_, value)| in_window(height_of(value)))
                .collect(),
            BlockInfo => all
                .into_iter()
                .filter(|(key, _)| {
                    let height = height_of(key);
                    height.is_min() || height.0 >= start.0 - 1
                })
                .collect(),
            TxLocByHash => all
                .into_iter()
                .filter(|(_, value)| stored(height_of(value)) || utxo_transactions.contains(value))
                .collect(),
            SaplingTree | OrchardTree | IronwoodTree => {
                let subtrees = match component {
                    SaplingTree => SaplingSubtrees,
                    OrchardTree => OrchardSubtrees,
                    _ => IronwoodSubtrees,
                };
                let ends: BTreeSet<Height> = rows(state, subtrees)
                    .into_iter()
                    .map(|(_, value)| height_of(&value))
                    .collect();
                let as_of_start = all
                    .iter()
                    .rfind(|(key, _)| height_of(key) <= start)
                    .map(|(key, _)| key.clone());
                all.into_iter()
                    .filter(|(key, _)| {
                        let height = height_of(key);
                        height.is_min()
                            || ends.contains(&height)
                            || height > start
                            || Some(key) == as_of_start.as_ref()
                    })
                    .collect()
            }
            _ => all,
        };
        records.insert(component, selected);
    }
    records
}

/// A snapshot written as pieces, with its manifest and commitment.
pub(super) struct Export {
    pub(super) manifest: Manifest,
    pub(super) commitment: StateSnapshotCommitment,
    pub(super) pieces: HashMap<ObjectHash, PathBuf>,
}

/// Writes `records` as the pieces of a snapshot at `height` into `dir`, at
/// most `per_piece` records to a piece, and returns its manifest and
/// commitment.
pub(super) fn export(
    records: &Records,
    network: &Network,
    height: Height,
    block_hash: block::Hash,
    dir: &Path,
    per_piece: usize,
) -> Export {
    fs::create_dir_all(dir).unwrap();
    let mut components = Vec::new();
    let mut pieces = HashMap::new();
    for component in ComponentId::ALL {
        let rows = &records[&component];
        let mut entries = Vec::new();
        for chunk in rows.chunks(per_piece) {
            let mut writer = PieceWriter::new(Vec::new());
            for (key, value) in chunk {
                writer.push(key, value).unwrap();
            }
            let bytes = writer.finish().unwrap();
            let hash = ObjectHash::of(&bytes);
            let path = dir.join(hash.to_string());
            fs::write(&path, &bytes).unwrap();
            pieces.insert(hash, path);
            entries.push(PieceManifest {
                size: bytes.len().try_into().unwrap(),
                hash,
            });
        }
        components.push(ComponentManifest {
            id: component,
            entry_count: rows.len().try_into().unwrap(),
            pieces: entries,
        });
    }

    let manifest = Manifest {
        genesis_hash: network.genesis_hash(),
        height,
        block_hash,
        window_start: window_start(height),
        db_format_major: state_database_format_version_in_code()
            .major
            .try_into()
            .unwrap(),
        index_mode: INDEX_MODE_DEFAULT,
        parameters_hash: network.state_snapshot_parameters_hash(),
        components,
    };
    let commitment = StateSnapshotCommitment {
        height,
        block_hash,
        manifest: ObjectHash::of(&manifest.encode().unwrap()),
    };
    Export {
        manifest,
        commitment,
        pieces,
    }
}

/// An import into a new cache directory, and its result.
pub(super) struct Import {
    pub(super) config: Config,
    pub(super) result: Result<SnapshotBase, SnapshotError>,
    _cache: tempfile::TempDir,
}

/// Exports `records` from `chain` into a new cache directory and imports them
/// there, with `deferred_checks`, for `network`.
pub(super) fn import(
    chain: &Chain,
    records: &Records,
    network: &Network,
    deferred_checks: u8,
) -> Import {
    let cache = tempfile::tempdir().unwrap();
    let config = Config {
        cache_dir: cache.path().to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let (height, hash) = chain.state.db.tip().unwrap();
    let export = export(
        records,
        network,
        height,
        hash,
        &cache.path().join("pieces"),
        300,
    );
    let result = import_export(&export, network, &config, deferred_checks);
    Import {
        config,
        result,
        _cache: cache,
    }
}

/// Imports `export` into the state `config` names.
pub(super) fn import_export(
    export: &Export,
    network: &Network,
    config: &Config,
    deferred_checks: u8,
) -> Result<SnapshotBase, SnapshotError> {
    import_export_with_headers(export, network, config, deferred_checks, &[])
}

/// Imports `export` into the state `config` names, with the headers `headers_above` from the
/// block after the snapshot height on.
pub(super) fn import_export_with_headers(
    export: &Export,
    network: &Network,
    config: &Config,
    deferred_checks: u8,
    headers_above: &[Arc<block::Header>],
) -> Result<SnapshotBase, SnapshotError> {
    import_state_snapshot(
        &import_dir(config).join("bootstrap"),
        &export.manifest,
        export.commitment,
        deferred_checks,
        headers_above,
        network,
        config,
        &export.pieces,
    )
}

/// Returns a Sapling note commitment tree whose last leaf is at `position`,
/// with a cached root, without appending the leaves before it.
pub(super) fn sapling_tree_at(position: u64) -> sapling::tree::NoteCommitmentTree {
    use sapling::tree::legacy::{
        LegacyFrontier, LegacyLeaf, LegacyNonEmptyFrontier, LegacyNoteCommitmentTree,
        LegacyPosition, Node,
    };

    let node = |seed: u8| {
        let mut bytes = [0; 32];
        bytes[0] = seed;
        Node(Option::from(sapling_crypto::Node::from_bytes(bytes)).unwrap())
    };
    // A frontier has one ommer for each set bit of its position, and a right
    // leaf holds the first of them.
    let ommers = position.count_ones() - u32::from(position % 2 == 1);
    let leaf = if position % 2 == 1 {
        LegacyLeaf::Right(node(3), node(4))
    } else {
        LegacyLeaf::Left(node(3))
    };
    let mut legacy = LegacyNoteCommitmentTree::from(sapling::tree::NoteCommitmentTree::default());
    legacy.inner = LegacyFrontier {
        frontier: Some(LegacyNonEmptyFrontier {
            position: LegacyPosition(position.try_into().unwrap()),
            leaf,
            ommers: (0..ommers)
                .map(|ommer| node(10 + u8::try_from(ommer).unwrap()))
                .collect(),
        }),
    };
    let tree = sapling::tree::NoteCommitmentTree::from(legacy);
    tree.root();
    tree
}

/// The Sapling tree positions [`inject_subtrees`] writes, by height: subtree 0
/// completes with the last note of block 8, and subtree 1 in the middle of
/// block 20, both below the window, and the tip tree is in subtree 2.
pub(super) const SAPLING_TREES: [(u32, u64); 5] = [
    (4, 100),
    (8, (1 << 16) - 1),
    (15, (1 << 16) + 50),
    (20, (2 << 16) + 3),
    (30, (2 << 16) + 400),
];

/// Writes the Sapling trees of [`SAPLING_TREES`] after their blocks, with their
/// anchors and subtrees, so the chain has subtrees that end below the window.
/// Later blocks carry each tree forward, so the history tree commits to the
/// tip tree's root.
pub(super) fn inject_subtrees(state: &FinalizedState, height: Height) {
    let Some((_, position)) = SAPLING_TREES
        .iter()
        .find(|(tree_height, _)| *tree_height == height.0)
    else {
        return;
    };
    let tree = sapling_tree_at(*position);
    let mut batch = DiskWriteBatch::new();
    batch.zs_insert(
        &state.db.cf_handle("sapling_note_commitment_tree").unwrap(),
        height,
        &tree,
    );
    batch.zs_insert(
        &state.db.cf_handle("sapling_anchors").unwrap(),
        tree.root(),
        (),
    );
    let subtree = match height.0 {
        8 => tree
            .completed_subtree_index_and_root()
            .map(|(index, root)| (index, NoteCommitmentSubtreeData::new(height, root))),
        // A subtree that completes in the middle of a block has a root no tree records.
        20 => Some((
            NoteCommitmentSubtreeIndex(1),
            NoteCommitmentSubtreeData::new(
                height,
                Option::from(sapling_crypto::Node::from_bytes([9; 32])).unwrap(),
            ),
        )),
        _ => None,
    };
    if let Some((index, data)) = subtree {
        batch.zs_insert(
            &state
                .db
                .cf_handle("sapling_note_commitment_subtree")
                .unwrap(),
            index,
            data,
        );
    }
    state.db.write_batch(batch).unwrap();
}

/// Returns an Orchard-shaped tree with one small field element per seed.
pub(super) fn orchard_tree(seeds: &[u8]) -> orchard::tree::NoteCommitmentTree {
    use halo2::pasta::{group::ff::PrimeField, pallas};

    let mut tree = orchard::tree::NoteCommitmentTree::default();
    for seed in seeds {
        let mut bytes = [0; 32];
        bytes[0] = *seed;
        tree.append(pallas::Base::from_repr(bytes).unwrap())
            .unwrap();
    }
    tree.root();
    tree
}

/// Returns a Sprout tree with one note commitment per seed.
pub(super) fn sprout_tree(seeds: &[u8]) -> sprout::tree::NoteCommitmentTree {
    let mut tree = sprout::tree::NoteCommitmentTree::default();
    for seed in seeds {
        tree.append(sprout::NoteCommitment::from([*seed; 32]))
            .unwrap();
    }
    tree.root();
    tree
}

/// Gives the chain non-empty Orchard and Sprout tip trees, carried forward by later blocks.
pub(super) fn inject_orchard_and_sprout_trees(state: &FinalizedState, height: Height) {
    if height != Height(30) {
        return;
    }
    let mut batch = DiskWriteBatch::new();
    batch.create_orchard_tree(&state.db, &height, &orchard_tree(&[1, 2]));
    batch.update_sprout_tree(&state.db, &sprout_tree(&[3, 4]));
    state.db.write_batch(batch).unwrap();
}

/// Gives a NU6.3 chain a non-empty Ironwood tip tree, carried forward by later blocks.
pub(super) fn inject_ironwood_tree(state: &FinalizedState, height: Height) {
    if height != Height(30) {
        return;
    }
    let mut batch = DiskWriteBatch::new();
    batch.create_ironwood_tree(&state.db, &height, &orchard_tree(&[5, 6]));
    state.db.write_batch(batch).unwrap();
}
