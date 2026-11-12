//! Tests for committing a chain with fast checkpoint sync, like the finalized block writer does.
//!
//! The test chain activates Sapling at height 3, Heartwood at 5, and NU5 at 7, and ends at
//! height 12. Fast sync ends with trees embedded at height 9.

use std::{cmp::Ordering, sync::Arc};

use zebra_chain::{
    block::{Block, ChainHistoryBlockTxAuthCommitmentHash, Height},
    history_tree::HistoryTree,
    parallel::tree::{NoteCommitmentTrees, TreeRoots},
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
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

/// Returns a chain from the Mainnet genesis block with the shielded data in [`SHIELDED_COUNTS`]
/// and valid header commitments for `network`.
fn shielded_chain(network: &Network) -> Vec<CheckpointVerifiedBlock> {
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

    for (height, (sapling_outputs, orchard_actions)) in (1..).map(Height).zip(&SHIELDED_COUNTS[1..])
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
        let blocks = shielded_chain(&network);
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
    /// Not at all, because its roots failed verification while the trees were frozen.
    Rejected,
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
    fn new(state: FinalizedState, cache: Arc<TreeRootsCache>) -> Self {
        let fast_tree_sync = FastTreeSyncWriter::new(&state.db, cache, state.network());

        Self {
            state,
            fast_tree_sync,
            trees: None,
        }
    }

    /// Sends `blocks` to the writer, and returns how it committed them.
    ///
    /// Like the writer, holds each block that fast sync defers until the next block, so a deferred
    /// last block isn't committed, and stops at a rejected block, until the state service sends
    /// the blocks after the finalized tip again.
    fn commit_blocks(&mut self, blocks: &[CheckpointVerifiedBlock]) -> Vec<(Height, Commit)> {
        let mut commits = Vec::new();
        let mut held_block: Option<&CheckpointVerifiedBlock> = None;

        for block in blocks {
            if let Some(held_block) = held_block.take() {
                let commit = self.commit(held_block.clone(), Some(block));
                commits.push((held_block.height, commit));

                if commit == Commit::Rejected {
                    break;
                }
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

                // A writer that waits for missing roots gives up when it is shutting down.
                self.fast_tree_sync.checkpoint_commit(
                    block,
                    next,
                    &self.state.db.history_tree(),
                    trees,
                    || true,
                )
            }
            None => CheckpointCommit::Normal(block),
        };

        let (result, frozen_sizes, commit) = match checkpoint_commit {
            CheckpointCommit::Fast {
                block,
                frozen_sizes,
            } => (
                self.state.commit_finalized_fast(*block, self.trees.take()),
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
            CheckpointCommit::Rejected => return Commit::Rejected,
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
    let cache = Arc::new(TreeRootsCache::new(Some(embedded_trees.clone()), Height(0)));
    cache.supply(Height(0), chain.roots[..3].to_vec());
    cache.supply(Height(4), chain.roots[4..=9].to_vec());

    // The writer stops in the middle of the frozen heights, holding block 6.
    let mut writer = TestWriter::new(open(), cache);
    let mut all_commits = writer.commit_blocks(&chain.blocks[..=6]);
    assert_eq!(writer.state.db.finalized_tip_height(), Some(Height(5)));
    drop(writer);

    // After a restart, the writer still has frozen trees, with the sizes after its tip.
    let cache = Arc::new(TreeRootsCache::new(Some(embedded_trees.clone()), Height(6)));
    cache.supply(Height(6), chain.roots[6..=9].to_vec());
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
    let cache = Arc::new(TreeRootsCache::new(Some(embedded_trees), Height(13)));
    assert!(!FastTreeSyncWriter::new(db, cache, chain.network.clone()).is_frozen());
}

#[test]
fn fast_sync_rejects_wrong_roots() {
    use Commit::*;

    let _init_guard = zebra_test::init();
    let chain = TestChain::new();

    let cache = Arc::new(TreeRootsCache::new(
        Some(chain.embedded_trees(Height(9))),
        Height(0),
    ));
    let mut roots = chain.roots[..=9].to_vec();
    roots[3] = TreeRoots::empty();
    cache.supply(Height(0), roots);

    let state = FinalizedState::new(&Config::ephemeral(), &chain.network)
        .expect("opening an ephemeral database succeeds");
    let mut writer = TestWriter::new(state, cache.clone());

    // Before the trees are frozen, the writer drops wrong roots and the roots after them, and
    // updates the trees.
    assert_eq!(
        writer.commit_blocks(&chain.blocks[..=4]),
        commits(0, [Normal; 5]),
    );
    assert_eq!(cache.wanted(), Some(Height(5)..=Height(9)));

    // While the trees are frozen, the writer can't update them, so it commits nothing until it
    // gets the roots again.
    let mut roots = chain.roots[5..=9].to_vec();
    roots[1] = TreeRoots::empty();
    cache.supply(Height(5), roots);
    assert_eq!(
        writer.commit_blocks(&chain.blocks[5..]),
        commits(5, [Frozen, Rejected]),
    );
    assert_eq!(
        writer.state.db.tip(),
        Some((Height(5), chain.blocks[5].hash))
    );
    assert_eq!(cache.wanted(), Some(Height(6)..=Height(9)));

    cache.supply(Height(6), chain.roots[6..=9].to_vec());
    assert_eq!(
        writer.commit_blocks(&chain.blocks[6..]),
        commits(
            6,
            [Frozen, Frozen, Frozen, Embedded, Normal, Normal, Normal]
        ),
    );
    assert_eq!(
        writer.state.db.note_commitment_trees_for_tip(),
        chain.state.db.note_commitment_trees_for_tip(),
    );
}
