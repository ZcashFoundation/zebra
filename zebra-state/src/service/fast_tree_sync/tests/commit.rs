//! Tests for committing a chain with fast checkpoint sync, like the finalized block writer does.
//!
//! The test chain activates Sapling at height 3, Heartwood at 5, and NU5 at 7, and ends at
//! height 12. Fast sync ends with trees embedded at height 9.

use std::{cmp::Ordering, sync::Arc};

use zebra_chain::{
    block::{Block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    history_tree::HistoryTree,
    parallel::tree::{NoteCommitmentTrees, TreeRoots, TreeRootsProof},
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{arbitrary::insert_fake_orchard_shielded_data, LockTime, Transaction},
};
use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_GENESIS_BYTES};

use crate::{
    service::{
        fast_tree_sync::{CheckpointCommit, EmbeddedTrees, FastTreeSyncWriter, TreeRootsCache},
        finalized_state::{FastSyncTreeRoots, FinalizedState, ZebraDb},
    },
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config,
};

/// The number of Sapling outputs and Orchard actions in each block of the test chain, by height.
const SHIELDED_COUNTS: [(u8, u8); 13] = [
    (0, 0),
    (0, 0),
    (0, 0),
    (2, 0),
    (1, 0),
    (3, 0),
    (0, 0),
    (1, 1),
    (0, 2),
    (2, 1),
    (1, 0),
    (0, 1),
    (1, 0),
];

/// Returns the test network.
fn network() -> Network {
    ParametersBuilder::default()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            before_overwinter: Some(1),
            overwinter: Some(2),
            sapling: Some(3),
            blossom: Some(4),
            heartwood: Some(5),
            canopy: Some(6),
            nu5: Some(7),
            ..Default::default()
        })
        .expect("the activation heights are in order")
        .clear_funding_streams()
        .to_network()
        .expect("the test network is valid")
}

/// Returns a transaction with `count` Sapling outputs, whose note commitments are unique numbers
/// taken from `seed`.
///
/// The outputs are well-formed, but their proofs and signatures are invalid, which the finalized
/// state doesn't check.
fn sapling_transaction(seed: &mut u32, count: u8) -> Transaction {
    let point = jubjub::AffinePoint::from(jubjub::ExtendedPoint::from(
        sapling_crypto::constants::SPENDING_KEY_GENERATOR,
    ))
    .to_bytes();

    let mut bytes = Vec::new();
    // An overwintered V4 transaction with the Sapling version group ID.
    bytes.extend_from_slice(&0x8000_0004_u32.to_le_bytes());
    bytes.extend_from_slice(&0x892F_2085_u32.to_le_bytes());
    // No transparent inputs or outputs, the lock time, the expiry height, and valueBalanceSapling.
    bytes.extend_from_slice(&[0; 2 + 4 + 4 + 8]);
    // No spends, then the output count, which is a one-byte CompactSize below 253.
    bytes.extend_from_slice(&[0, count]);
    for _ in 0..count {
        *seed += 1;
        let mut cmu = [0; 32];
        cmu[..4].copy_from_slice(&seed.to_le_bytes());

        bytes.extend_from_slice(&point);
        bytes.extend_from_slice(&cmu);
        // The ephemeral key, ciphertexts, and proof.
        bytes.extend_from_slice(&[0; 32 + 580 + 80 + 192]);
    }
    // No JoinSplits, then the binding signature.
    bytes.extend_from_slice(&[0; 1 + 64]);

    bytes
        .as_slice()
        .zcash_deserialize_into()
        .expect("the transaction is well-formed")
}

/// Returns a transaction with an Orchard action, which `seed` makes unique.
fn orchard_transaction(seed: &mut u32) -> Transaction {
    *seed += 1;

    // The fake action is the same in each transaction, so the expiry height makes it unique.
    insert_fake_orchard_shielded_data(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        Vec::new(),
        Vec::new(),
        LockTime::unlocked(),
        Height(*seed),
    ))
}

/// Returns a chain from the Mainnet genesis block with per-height shielded data and valid
/// header commitments for `network`.
fn shielded_chain(network: &Network, shielded_counts: &[(u8, u8)]) -> Vec<CheckpointVerifiedBlock> {
    let genesis: Arc<Block> = BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid blocks");
    let block_1: Arc<Block> = BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors are valid blocks");
    let is_active = |upgrade: NetworkUpgrade, height: Height| {
        upgrade
            .activation_height(network)
            .is_some_and(|activation| height >= activation)
    };
    let heartwood = NetworkUpgrade::Heartwood
        .activation_height(network)
        .expect("the test network activates Heartwood");

    let mut blocks = vec![genesis];
    let mut trees = NoteCommitmentTrees::default();
    let mut history_tree = HistoryTree::default();
    let mut seed = 0;

    for (height, (sapling_outputs, orchard_actions)) in (1..).map(Height).zip(&shielded_counts[1..])
    {
        let mut block = if height == Height(1) {
            block_1.clone()
        } else {
            blocks
                .last()
                .expect("the chain starts with the genesis block")
                .make_fake_child()
        };

        // Fake children copy their parent's transactions, so keep only the coinbase.
        let transactions = &mut Arc::make_mut(&mut block).transactions;
        transactions.truncate(1);
        if *sapling_outputs > 0 {
            transactions.push(Arc::new(sapling_transaction(&mut seed, *sapling_outputs)));
        }
        for _ in 0..*orchard_actions {
            transactions.push(Arc::new(orchard_transaction(&mut seed)));
        }

        trees
            .update_trees_parallel(&block)
            .expect("the test trees have room");

        // Before Heartwood, headers commit to the Sapling root after their block, and from
        // Heartwood to the chain history before it.
        let commitment: Option<[u8; 32]> = match height.cmp(&heartwood) {
            Ordering::Less => {
                is_active(NetworkUpgrade::Sapling, height).then(|| trees.sapling.root().into())
            }
            Ordering::Equal => Some([0; 32]),
            Ordering::Greater => {
                let history_root = history_tree
                    .hash()
                    .expect("the history tree exists after Heartwood activation");

                Some(if is_active(NetworkUpgrade::Nu5, height) {
                    ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                        &history_root,
                        &block.auth_data_root(),
                    )
                    .into()
                } else {
                    history_root.into()
                })
            }
        };

        let block_mut = Arc::make_mut(&mut block);
        let header = Arc::make_mut(&mut block_mut.header);
        header.merkle_root = block_mut.transactions.iter().collect();
        if let Some(commitment) = commitment {
            header.commitment_bytes = commitment.into();
        }

        history_tree
            .push(
                network,
                block.clone(),
                trees.roots().as_block_commitment_tree_roots(),
            )
            .expect("the test chain has valid history");
        blocks.push(block);
    }

    blocks
        .into_iter()
        .map(CheckpointVerifiedBlock::from)
        .collect()
}

/// The test chain, committed with tree updates.
struct TestChain {
    /// The test network.
    network: Network,
    /// The blocks.
    blocks: Vec<CheckpointVerifiedBlock>,
    /// The state with the blocks committed with tree updates.
    state: FinalizedState,
    /// The roots after each block.
    roots: Vec<TreeRoots>,
    /// The Sapling, Orchard, and Ironwood tree sizes after each block.
    sizes: Vec<[u64; 3]>,
}

impl TestChain {
    /// Returns the test chain, committed to an ephemeral state with tree updates.
    fn new() -> Self {
        let network = network();
        let blocks = shielded_chain(&network, &SHIELDED_COUNTS);
        let mut state = FinalizedState::new(&Config::ephemeral(), &network)
            .expect("opening an ephemeral database succeeds");

        let mut trees = None;
        let mut roots = Vec::new();
        let mut sizes = Vec::new();
        for block in &blocks {
            let (_, block_trees) = state
                .commit_finalized(block.clone(), trees.take())
                .expect("the test chain is valid");
            roots.push(block_trees.roots());
            sizes.push([
                block_trees.sapling.count(),
                block_trees.orchard.count(),
                block_trees.ironwood.count(),
            ]);
            trees = Some(block_trees);
        }

        assert!(
            sizes[4][0] < sizes[8][0] && sizes[4][1] < sizes[8][1],
            "the Sapling and Orchard trees change while fast sync freezes them"
        );

        Self {
            network,
            blocks,
            state,
            roots,
            sizes,
        }
    }

    /// Returns proofs derived from the actual finalized successors in the reference state.
    fn proofs(&self, range: std::ops::RangeInclusive<u32>) -> Vec<TreeRootsProof> {
        crate::service::read::tree::tree_roots(
            &self.state.db,
            Height(*range.start()),
            Height(*range.end()),
        )
        .expect("the reference chain has every required successor")
    }

    /// Returns the roots and tree sizes after the block at `height`.
    fn fast_sync_tree_roots(&self, height: Height) -> FastSyncTreeRoots {
        let index = usize::try_from(height.0).expect("test heights fit in usize");
        let [sapling_size, orchard_size, ironwood_size] = self.sizes[index];

        FastSyncTreeRoots {
            roots: self.roots[index],
            sapling_size,
            orchard_size,
            ironwood_size,
        }
    }

    /// Returns the trees in the test chain's state after the block at `height`, as embedded trees.
    fn embedded_trees(&self, height: Height) -> Arc<EmbeddedTrees> {
        let db: &ZebraDb = &self.state.db;

        Arc::new(EmbeddedTrees {
            height,
            sapling: db
                .sapling_tree_by_height(&height)
                .expect("committed heights have trees"),
            orchard: db
                .orchard_tree_by_height(&height)
                .expect("committed heights have trees"),
            ironwood: db
                .ironwood_tree_by_height(&height)
                .expect("committed heights have trees"),
            // The test trees have fewer than 2^16 leaves, so they have no completed subtrees.
            sapling_subtrees: Vec::new(),
            orchard_subtrees: Vec::new(),
            ironwood_subtrees: Vec::new(),
        })
    }
}

/// How the block writer committed a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Commit {
    /// With tree updates.
    Normal,
    /// With its verified roots, leaving the trees frozen.
    Frozen,
    /// With its verified roots, switching to the embedded trees.
    Embedded,
}

/// Returns `commits` for consecutive heights from `start`.
fn commits(start: u32, commits: impl IntoIterator<Item = Commit>) -> Vec<(Height, Commit)> {
    (start..).map(Height).zip(commits).collect()
}

/// The checkpoint path of the finalized block writer, `WriteBlockWorkerTask::run`.
struct TestWriter {
    /// The state the writer commits to.
    state: FinalizedState,
    /// The fast sync state.
    fast_tree_sync: FastTreeSyncWriter,
    /// The note commitment trees after the last committed block, if they are known.
    trees: Option<NoteCommitmentTrees>,
}

impl TestWriter {
    /// Returns a writer that starts committing to `state`, using the roots in `cache`.
    fn new(state: FinalizedState, cache: TreeRootsCache) -> Self {
        let fast_tree_sync = FastTreeSyncWriter::new(&state.db, cache, state.network());

        Self {
            state,
            fast_tree_sync,
            trees: None,
        }
    }

    /// Sends `blocks` to the writer, and returns how it committed them.
    ///
    /// Like the writer, retains a deferred block and its successor while authenticating roots.
    fn commit_blocks(&mut self, blocks: &[CheckpointVerifiedBlock]) -> Vec<(Height, Commit)> {
        let mut commits = Vec::new();
        let mut held_block: Option<&CheckpointVerifiedBlock> = None;

        for block in blocks {
            if let Some(held_block) = held_block.take() {
                let commit = self.commit(held_block.clone(), Some(block));
                commits.push((held_block.height, commit));
            }

            if self.fast_tree_sync.defers(block) {
                held_block = Some(block);
                continue;
            }

            commits.push((block.height, self.commit(block.clone(), None)));
        }

        commits
    }

    /// Commits `block` like `commit_checkpoint_verified`, verifying its roots against `next`.
    fn commit(
        &mut self,
        block: CheckpointVerifiedBlock,
        next: Option<&CheckpointVerifiedBlock>,
    ) -> Commit {
        let height = block.height;
        let checkpoint_commit = match next {
            Some(next) => {
                let trees = self
                    .trees
                    .get_or_insert_with(|| self.state.db.note_commitment_trees_for_tip());

                // The real owner retries roots while retaining the authenticated block work.
                self.fast_tree_sync.checkpoint_commit(
                    block,
                    next,
                    &self.state.db.history_tree(),
                    trees,
                    || false,
                )
            }
            None => CheckpointCommit::Normal(block),
        };

        let (result, frozen_sizes, commit) = match checkpoint_commit {
            CheckpointCommit::Fast {
                block,
                frozen_sizes,
                successor_error,
            } => (
                {
                    assert!(successor_error.is_none(), "test chain bodies are valid");
                    self.state.commit_finalized_fast(*block, self.trees.take())
                },
                frozen_sizes,
                if frozen_sizes.is_some() {
                    Commit::Frozen
                } else {
                    Commit::Embedded
                },
            ),
            CheckpointCommit::Normal(block) => {
                assert!(
                    !self.fast_tree_sync.is_frozen(),
                    "blocks are never committed with tree updates from frozen trees",
                );
                (
                    self.state.commit_finalized(block, self.trees.take()),
                    None,
                    Commit::Normal,
                )
            }
            CheckpointCommit::Stalled(height) => panic!("missing roots at {height:?}"),
            CheckpointCommit::Shutdown => panic!("the tests supply the roots of frozen blocks"),
        };

        let (_, trees) = result.expect("the test chain is valid");
        self.trees = Some(trees);
        self.fast_tree_sync.committed(height, frozen_sizes);

        commit
    }
}

#[test]
fn fast_sync_commits_verified_roots_then_the_embedded_trees() {
    use Commit::*;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let embedded_trees = chain.embedded_trees(Height(9));

    let cache_dir = tempfile::tempdir().expect("temporary directories can be created");
    let config = Config {
        cache_dir: cache_dir.path().to_path_buf(),
        ..Config::default()
    };
    let open = || FinalizedState::new(&config, &chain.network).expect("the test database opens");

    // Blocks below Sapling commit normally even with roots, so do blocks without roots, and the
    // first block with roots freezes the trees.
    let mut cache = TreeRootsCache::new(Some(embedded_trees.clone()), Height(0));
    cache.supply(Height(0), chain.proofs(0..=2));
    cache.supply(Height(4), chain.proofs(4..=9));

    // The writer stops in the middle of the frozen heights, holding block 6.
    let mut writer = TestWriter::new(open(), cache);
    let mut all_commits = writer.commit_blocks(&chain.blocks[..=6]);
    assert_eq!(writer.state.db.finalized_tip_height(), Some(Height(5)));
    drop(writer);

    // After a restart, the writer still has frozen trees, with the sizes after its tip.
    let mut cache = TreeRootsCache::new(Some(embedded_trees.clone()), Height(6));
    cache.supply(Height(6), chain.proofs(6..=9));
    let mut writer = TestWriter::new(open(), cache);
    assert!(writer.fast_tree_sync.is_frozen());
    assert_eq!(writer.fast_tree_sync.frozen_sizes, Some(chain.sizes[5]));

    all_commits.extend(writer.commit_blocks(&chain.blocks[6..]));
    assert_eq!(
        all_commits,
        commits(
            0,
            [
                Normal, Normal, Normal, Normal, Frozen, Frozen, Frozen, Frozen, Frozen, Embedded,
                Normal, Normal, Normal,
            ],
        ),
    );

    let db = &writer.state.db;
    for (height, roots) in chain.roots.iter().enumerate() {
        assert!(
            db.contains_sapling_anchor(&roots.sapling),
            "the Sapling root after block {height} is an anchor",
        );
        assert!(
            db.contains_orchard_anchor(&roots.orchard),
            "the Orchard root after block {height} is an anchor",
        );
    }
    assert_eq!(
        db.history_tree().hash(),
        chain.state.db.history_tree().hash()
    );
    assert_eq!(
        db.note_commitment_trees_for_tip(),
        chain.state.db.note_commitment_trees_for_tip(),
    );

    // The database has roots and sizes instead of trees for the frozen heights.
    let frozen = Height(4)..=Height(8);
    assert_eq!(db.fast_sync_tree_heights(), Some(frozen.clone()));
    for height in (4..=8).map(Height) {
        assert_eq!(
            db.fast_sync_tree_roots(height),
            Some(chain.fast_sync_tree_roots(height)),
        );
    }
    assert_eq!(db.sapling_tree_by_height_range(frozen.clone()).count(), 0);
    assert_eq!(db.orchard_tree_by_height_range(frozen).count(), 0);

    assert_eq!(
        db.tree_roots_by_height_range(Height(2), Height(12)),
        Some(chain.roots[2..].to_vec()),
    );
    assert_eq!(
        db.tree_roots_by_height_range(Height(6), Height(10)),
        Some(chain.roots[6..=10].to_vec()),
    );
    assert_eq!(
        chain
            .state
            .db
            .tree_roots_by_height_range(Height(0), Height(12)),
        Some(chain.roots.clone()),
    );

    // A writer after fast sync updates its trees.
    let cache = TreeRootsCache::new(Some(embedded_trees), Height(13));
    assert!(!FastTreeSyncWriter::new(db, cache, chain.network.clone()).is_frozen());
}

#[test]
fn fast_sync_rejects_wrong_roots() {
    use Commit::*;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();

    let mut cache = TreeRootsCache::new(Some(chain.embedded_trees(Height(9))), Height(0));
    let mut roots = chain.proofs(0..=9);
    roots[3].roots = TreeRoots::empty();
    cache.supply(Height(0), roots);

    let state = FinalizedState::new(&Config::ephemeral(), &chain.network)
        .expect("opening an ephemeral database succeeds");
    let mut writer = TestWriter::new(state, cache);

    // Before the trees are frozen, the writer drops wrong roots and the roots after them, and
    // updates the trees.
    assert_eq!(
        writer.commit_blocks(&chain.blocks[..=4]),
        commits(0, [Normal; 5]),
    );
    assert_eq!(
        writer.fast_tree_sync.cache.wanted(),
        Some(Height(5)..=Height(9))
    );
}

#[test]
fn fast_sync_freezes_only_after_enough_distinct_root_sources() {
    use Commit::*;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let mut cache = TreeRootsCache::new(Some(chain.embedded_trees(Height(9))), Height(0));
    cache.min_sources = super::super::MIN_TREE_ROOT_SOURCES;

    // One root-serving peer, even on several ports, can't freeze the trees, so losing it can't
    // stall sync.
    cache.receive((
        Height(0),
        chain.proofs(0..=5),
        "127.0.0.1:8233".parse().unwrap(),
    ));
    cache.receive((
        Height(6),
        chain.proofs(6..=9),
        "127.0.0.1:18233".parse().unwrap(),
    ));
    let state = FinalizedState::new(&Config::ephemeral(), &chain.network).unwrap();
    let mut writer = TestWriter::new(state, cache);
    assert_eq!(
        writer.commit_blocks(&chain.blocks[..=4]),
        commits(0, [Normal; 5]),
    );

    for source in ["127.0.0.2:8233", "127.0.0.3:8233"] {
        writer.fast_tree_sync.cache.receive((
            Height(5),
            chain.proofs(5..=5),
            source.parse().unwrap(),
        ));
    }
    assert_eq!(
        writer.commit_blocks(&chain.blocks[5..]),
        commits(
            5,
            [Frozen, Frozen, Frozen, Frozen, Embedded, Normal, Normal, Normal]
        ),
    );
    assert_eq!(
        writer.state.db.note_commitment_trees_for_tip(),
        chain.state.db.note_commitment_trees_for_tip()
    );
}

#[test]
fn bad_roots_blame_the_source_and_retry_without_restarting_blocks() {
    use Commit::*;
    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let mut cache = TreeRootsCache::new(Some(chain.embedded_trees(Height(9))), Height(0));
    let bad = "127.0.0.1:8233".parse().unwrap();
    let good = "127.0.0.2:8233".parse().unwrap();
    let (sender, wanted, rejected) = cache.channels();
    let mut roots = chain.proofs(0..=9);
    roots[6].roots = TreeRoots::empty();
    cache.receive((Height(0), roots, bad));
    let state = FinalizedState::new(&Config::ephemeral(), &chain.network).unwrap();
    let blocks = chain.blocks.clone();
    let owner = std::thread::spawn(move || {
        let mut writer = TestWriter::new(state, cache);
        // Submit the authenticated bodies exactly once. Root retry must not return early.
        let committed = writer.commit_blocks(&blocks);
        (writer, committed)
    });
    assert_eq!(
        rejected
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        bad
    );
    assert_eq!(*wanted.borrow().as_ref().unwrap().start(), Height(6));
    sender.send((Height(6), chain.proofs(6..=9), good)).unwrap();
    let (writer, committed) = owner.join().unwrap();
    assert_eq!(
        committed,
        commits(
            0,
            [
                Normal, Normal, Normal, Frozen, Frozen, Frozen, Frozen, Frozen, Frozen, Embedded,
                Normal, Normal, Normal,
            ]
        )
    );
    assert!(
        rejected.try_recv().is_err(),
        "only the bad-root source is blamed"
    );
    assert_eq!(
        writer.state.db.note_commitment_trees_for_tip(),
        chain.state.db.note_commitment_trees_for_tip()
    );
}

#[test]
fn frozen_restart_with_option_off_times_out_and_cancels_without_losing_state() {
    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let embedded = chain.embedded_trees(Height(9));
    let directory = tempfile::tempdir().unwrap();
    let mut config = Config {
        cache_dir: directory.path().to_path_buf(),
        fast_tree_sync: true,
        ..Config::default()
    };
    let state = FinalizedState::new(&config, &chain.network).unwrap();
    assert!(
        super::super::embedded_trees_for_state(
            Some(embedded.clone()),
            &state.db,
            false,
            Height(12),
        )
        .is_none(),
        "the option off cannot start fast sync"
    );
    let mut cache = TreeRootsCache::new(Some(embedded.clone()), Height(0));
    cache.supply(Height(0), chain.proofs(0..=5));
    let mut writer = TestWriter::new(state, cache);
    writer.commit_blocks(&chain.blocks[..=6]);
    assert_eq!(writer.state.db.finalized_tip_height(), Some(Height(5)));
    drop(writer);

    config.fast_tree_sync = false;
    let state = FinalizedState::new(&config, &chain.network).unwrap();
    let trees = super::super::embedded_trees_for_state(
        Some(embedded),
        &state.db,
        config.fast_tree_sync,
        Height(12),
    )
    .expect("a persisted frozen tip must resume even with the option off");
    let mut cache = TreeRootsCache::new(Some(trees), Height(6));
    let (_sender, wanted, _rejected) = cache.channels();
    let mut writer = TestWriter::new(state, cache);
    assert!(writer.fast_tree_sync.is_frozen());
    let trees = writer.state.db.note_commitment_trees_for_tip();
    let history = writer.state.db.history_tree();
    let started = std::time::Instant::now();
    assert!(matches!(
        writer.fast_tree_sync.checkpoint_commit(
            chain.blocks[6].clone(),
            &chain.blocks[7],
            &history,
            &trees,
            || false,
        ),
        CheckpointCommit::Stalled(Height(6))
    ));
    assert!(started.elapsed() >= super::super::ROOTS_STALL_TIMEOUT);
    assert_eq!(writer.state.db.finalized_tip_height(), Some(Height(5)));
    assert_eq!(*wanted.borrow().as_ref().unwrap().start(), Height(6));
    let started = std::time::Instant::now();
    assert!(matches!(
        writer.fast_tree_sync.checkpoint_commit(
            chain.blocks[6].clone(),
            &chain.blocks[7],
            &history,
            &trees,
            || true,
        ),
        CheckpointCommit::Shutdown
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_proof_recovers_only_the_forged_successor_through_state_routing() {
    use crate::service::{
        chain_tip::ChainTipSender, non_finalized_state::NonFinalizedState,
        watch_receiver::WatchReceiver, write::BlockWriteSender, ReadStateService, StateService,
    };
    use std::time::{Duration, Instant};
    use tokio::sync::{watch, Semaphore};
    use tower::ServiceExt;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let valid = &chain.blocks[7];
    let mut forged = valid.block.clone();
    let tx = Arc::make_mut(&mut forged)
        .transactions
        .iter_mut()
        .find(|tx| tx.version() == 5 && tx.orchard_bundle().is_some())
        .expect("block 7 contains an Orchard V5 transaction");
    let mut bytes = tx.zcash_serialize_to_vec().unwrap();
    // The final byte belongs to the Orchard binding signature, not to transaction effects.
    *bytes.last_mut().unwrap() ^= 1;
    *tx = bytes.zcash_deserialize_into::<Arc<Transaction>>().unwrap();
    let forged = CheckpointVerifiedBlock::from(forged).with_precomputed_auth_data_root();
    assert_eq!(forged.hash, valid.hash);
    assert_eq!(forged.transaction_hashes, valid.transaction_hashes);
    assert_eq!(forged.block.header, valid.block.header);
    assert_ne!(forged.block.auth_data_root(), valid.block.auth_data_root());

    // With roots from height 3, block 7 is an unauthenticated successor. With roots only from
    // height 7, it is the first fast current block. Both must use the exact-body recovery path.
    for first_fast in [3, 7] {
        let finalized = FinalizedState::new(&Config::ephemeral(), &chain.network).unwrap();
        let db = finalized.db.clone();
        let non_finalized = NonFinalizedState::new(&chain.network);
        let (tip_sender, latest_chain_tip, _change) = ChainTipSender::new(None, &chain.network);
        let (non_finalized_sender, non_finalized_receiver) = watch::channel(non_finalized.clone());
        let mut cache = TreeRootsCache::new(Some(chain.embedded_trees(Height(9))), Height(0));
        let (tree_roots_sender, wanted_tree_roots, rejected_tree_roots) = cache.channels();
        cache.receive((
            Height(first_fast),
            chain.proofs(first_fast..=9),
            "127.0.0.2:8233".parse().unwrap(),
        ));
        let (
            block_write_sender,
            invalid_block_write_reset_receiver,
            non_finalized_rejected_receiver,
            worker,
        ) = BlockWriteSender::spawn(
            finalized.clone(),
            non_finalized,
            tip_sender,
            non_finalized_sender,
            true,
            None,
            cache,
            None,
        );
        let read_service = ReadStateService::new(
            &finalized,
            worker,
            WatchReceiver::new(non_finalized_receiver),
        );
        // Use the actual service admission/order/recovery code with this deterministic embedded tree.
        let mut state = StateService {
            network: chain.network.clone(),
            max_checkpoint_height: Height::MAX,
            full_verifier_utxo_lookahead: Height(0),
            non_finalized_state_queued_blocks: Default::default(),
            finalized_state_queued_blocks: Default::default(),
            block_write_sender,
            finalized_block_write_last_sent_hash: db.finalized_tip_hash(),
            finalized_reset_next_height: None,
            non_finalized_block_write_sent_hashes: Default::default(),
            invalid_block_write_reset_receiver,
            finalized_body_replacement: None,
            non_finalized_rejected_receiver,
            pending_utxos: Default::default(),
            last_prune: Instant::now(),
            read_service,
            latest_chain_tip,
            mining_candidate_slot: Arc::new(Semaphore::new(1)),
            mining_candidate_lifetime: crate::service::arbitrary::TEST_MINING_CANDIDATE_LIFETIME,
            tree_roots_sender,
            wanted_tree_roots,
            rejected_tree_roots,
            pending_rejected_tree_roots: None,
            pruning_task: None,
            max_finalized_queue_height: f64::NAN,
            metrics_exporter: None,
        };
        let mut completions = Vec::new();
        for (index, block) in chain.blocks.iter().enumerate() {
            completions.push(state.queue_and_commit_to_finalized_state(
                if index == 7 {
                    forged.clone()
                } else {
                    block.clone()
                },
                None,
            ));
        }
        for (index, completion) in completions.drain(..7).enumerate() {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), completion)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                chain.blocks[index].hash,
            );
        }
        let error = tokio::time::timeout(Duration::from_secs(10), completions.remove(0))
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.is_targeted_body_recovery());
        assert!(error.inner().is_auth_commitment_mismatch());
        assert_eq!(
            error.inner().misbehavior_score(),
            100,
            "only the forged body's supplier is scored"
        );
        assert_eq!(db.finalized_tip_height(), Some(Height(6)));
        assert_eq!(
            db.fast_sync_tree_roots(Height(6)),
            (first_fast < 7).then(|| chain.fast_sync_tree_roots(Height(6))),
        );
        assert_eq!(
            db.block(Height(6).into()).unwrap(),
            Some(chain.blocks[6].block.clone())
        );
        assert!(state.drain_finalized_resets());
        assert!(
            state.finalized_reset_next_height.is_none(),
            "there was no queue-wide reset"
        );
        let known = tokio::time::timeout(
            Duration::from_secs(10),
            (&mut state).oneshot(crate::Request::KnownBlock(valid.hash)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(known, crate::Response::KnownBlock(None)));
        let known = tokio::time::timeout(
            Duration::from_secs(10),
            (&mut state).oneshot(crate::Request::KnownBlocks(vec![
                valid.hash,
                chain.blocks[8].hash,
            ])),
        )
        .await
        .unwrap()
        .unwrap();
        match known {
            crate::Response::KnownBlocks(locations) => {
                assert_eq!(locations, vec![None, Some(crate::KnownBlock::WriteChannel)]);
            }
            response => panic!("unexpected known-block response: {response:?}"),
        }
        for completion in &mut completions {
            assert!(matches!(
                completion.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
        }
        // Repeated authorization twins keep their individual errors and supplier scores.
        for _ in 0..3 {
            let replay = state.queue_and_commit_to_finalized_state(forged.clone(), None);
            let error = tokio::time::timeout(Duration::from_secs(10), replay)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(error.is_targeted_body_recovery());
            assert!(error.inner().is_auth_commitment_mismatch());
            assert_eq!(error.inner().misbehavior_score(), 100);
            assert!(state.drain_finalized_resets());
            assert!(state.finalized_reset_next_height.is_none());
        }
        let last_sent = state.finalized_block_write_last_sent_hash;
        let replacement = state.queue_and_commit_to_finalized_state(valid.clone(), None);
        assert_eq!(state.finalized_block_write_last_sent_hash, last_sent);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), replacement)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            valid.hash,
        );
        for (index, completion) in (8..).zip(completions) {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(10), completion)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                chain.blocks[index].hash,
            );
        }
        assert!(
            state.rejected_tree_roots.try_recv().is_err(),
            "the valid root source is not rejected"
        );
        assert_eq!(
            db.block(Height(7).into()).unwrap(),
            Some(valid.block.clone())
        );
        assert_eq!(
            db.history_tree().hash(),
            chain.state.db.history_tree().hash()
        );
        assert_eq!(
            db.note_commitment_trees_for_tip(),
            chain.state.db.note_commitment_trees_for_tip()
        );
    }
}

#[test]
fn zip244_proofs_require_the_suppliers_own_successor_digest() {
    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let mut history = HistoryTree::default();
    for (block, roots) in chain.blocks[..6].iter().zip(&chain.roots) {
        history
            .push(
                &chain.network,
                block.block.clone(),
                roots.as_block_commitment_tree_roots(),
            )
            .unwrap();
    }
    let history = Arc::new(history);
    let mut proof = chain.proofs(6..=6)[0];
    super::super::verify_tree_roots(
        &chain.network,
        &chain.blocks[6],
        &proof,
        &history,
        &chain.blocks[7],
    )
    .unwrap();
    proof.successor_auth_data_root = None;
    assert!(matches!(
        super::super::verify_tree_roots(
            &chain.network,
            &chain.blocks[6],
            &proof,
            &history,
            &chain.blocks[7],
        ),
        Err(super::super::TreeRootsError::MissingAuthDataRoot { .. }),
    ));
    let mut forged_digest: [u8; 32] = chain.proofs(6..=6)[0]
        .successor_auth_data_root
        .unwrap()
        .into();
    forged_digest[0] ^= 1;
    proof.successor_auth_data_root = Some(forged_digest.into());
    assert!(matches!(
        super::super::verify_tree_roots(
            &chain.network,
            &chain.blocks[6],
            &proof,
            &history,
            &chain.blocks[7],
        ),
        Err(super::super::TreeRootsError::Commitment { .. }),
    ));
    assert!(
        crate::service::read::tree::tree_roots(&chain.state.db, Height(12), Height(12)).is_none(),
        "a post-NU5 tip without a finalized successor is refused neutrally"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn historical_root_proofs_return_bounded_contiguous_prefixes() {
    use crate::service::{
        non_finalized_state::NonFinalizedState, read::tree::MAX_TREE_ROOT_PROOFS_PER_READ,
        watch_receiver::WatchReceiver, ReadStateService,
    };
    use std::time::Duration;
    use tower::ServiceExt;

    let _init_guard = zebra_test::init();
    let network = network();
    let limit = MAX_TREE_ROOT_PROOFS_PER_READ;
    let length = usize::try_from(limit + 3).expect("the small serving bound fits usize");
    let counts: Vec<_> = SHIELDED_COUNTS
        .iter()
        .copied()
        .cycle()
        .take(length)
        .collect();
    let blocks = shielded_chain(&network, &counts);
    let mut state = FinalizedState::new(&Config::ephemeral(), &network).unwrap();
    let mut trees = None;
    for block in blocks.iter().take(length - 1) {
        let (_, updated) = state.commit_finalized(block.clone(), trees.take()).unwrap();
        trees = Some(updated);
    }
    let (_sender, receiver) = tokio::sync::watch::channel(NonFinalizedState::new(&network));
    let read = ReadStateService::new(&state, None, WatchReceiver::new(receiver));
    let ask = |start_height, count, final_hash| {
        read.clone().oneshot(crate::ReadRequest::TreeRoots {
            start_height,
            count,
            final_hash,
        })
    };
    let end = &blocks[length - 2];
    let response =
        tokio::time::timeout(Duration::from_secs(10), ask(Height(1), limit + 1, end.hash))
            .await
            .unwrap()
            .unwrap();
    let crate::ReadResponse::TreeRoots(Some(prefix)) = response else {
        panic!("available prefix must not depend on an unrequested successor past its work bound");
    };
    assert_eq!(prefix.len(), usize::try_from(limit).unwrap());
    assert_eq!(
        prefix.iter().map(|proof| proof.roots).collect::<Vec<_>>(),
        state
            .db
            .tree_roots_by_height_range(Height(1), Height(limit))
            .unwrap(),
    );

    let wrong_chain = tokio::time::timeout(
        Duration::from_secs(10),
        ask(Height(1), limit + 1, zebra_chain::block::Hash([0; 32])),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(wrong_chain, crate::ReadResponse::TreeRoots(None)));

    state
        .commit_finalized(blocks[length - 1].clone(), trees.take())
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), ask(end.height, 1, end.hash))
        .await
        .unwrap()
        .unwrap();
    let crate::ReadResponse::TreeRoots(Some(rest)) = response else {
        panic!("the next prefix becomes available once its finalized successor exists");
    };
    let combined: Vec<_> = prefix
        .into_iter()
        .chain(rest)
        .map(|proof| proof.roots)
        .collect();
    assert_eq!(
        combined,
        state
            .db
            .tree_roots_by_height_range(Height(1), end.height)
            .unwrap(),
    );
}

/// The indexed byte budget stops successor decoding before a second maximum-size block.
#[test]
fn historical_root_proofs_bound_successor_body_bytes() {
    use zebra_chain::block_info::BlockInfo;

    use crate::service::finalized_state::DiskWriteBatch;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();
    let db = &chain.state.db;
    let mut batch = DiskWriteBatch::new();
    // Model large stored bodies using their size index, without building large proof fixtures.
    for height in [Height(7), Height(8)] {
        let info = db.block_info(height.into()).unwrap();
        let _ = db
            .block_info_cf()
            .with_batch_for_writing(&mut batch)
            .zs_insert(
                &height,
                &BlockInfo::new(
                    *info.value_pools(),
                    u32::try_from(zebra_chain::block::MAX_BLOCK_BYTES).unwrap(),
                ),
            );
    }
    db.write_batch(batch).unwrap();
    assert_eq!(
        crate::service::read::tree::tree_roots(db, Height(6), Height(10)).unwrap(),
        chain.proofs(6..=6),
    );
    assert_eq!(
        crate::service::read::tree::tree_roots(db, Height(7), Height(10)).unwrap(),
        chain.proofs(7..=7),
        "the next request still serves a maximum-size successor",
    );
}
