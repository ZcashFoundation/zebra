//! Tests for starting the note commitment tree updates before the rest of a block commit.

use std::{cell::Cell, env, sync::Arc};

use chrono::Duration;
use proptest::{
    strategy::{BoxedStrategy, ValueTree},
    test_runner::{TestCaseError, TestRunner},
};
use serde::Serialize;

use zebra_chain::{
    amount::{NonNegative, MAX_MONEY},
    block::{Block, Height},
    orchard,
    parallel::tree::NoteCommitmentTreeError,
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    serialization::ZcashDeserializeInto,
    value_balance::ValueBalance,
    LedgerState,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::{
        arbitrary::PreparedChain,
        check::difficulty::AdjustedDifficulty,
        finalized_state::{calculate_deferred_pool_balance_change, FinalizedState},
        non_finalized_state::{chain::FORK_COUNT, Chain, NonFinalizedState, SpendingTransactionId},
        write::validate_and_commit_non_finalized,
    },
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    Config, ContextuallyVerifiedBlock, SemanticallyVerifiedBlock, ValidateContextError,
};

use super::prop::spent_utxos;

/// The default number of proptest cases for the tree-first commit equivalence test.
const DEFAULT_TREE_FIRST_PROPTEST_CASES: u32 = 2;

/// Returns a new state and an empty finalized state with a populated value pool.
fn new_test_state(network: &Network) -> (NonFinalizedState, FinalizedState) {
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

    (NonFinalizedState::new(network), finalized_state)
}

/// Commits `block` as a new chain if it is the first block, or onto its parent chain otherwise.
fn commit(
    state: &mut NonFinalizedState,
    finalized_state: &FinalizedState,
    block: SemanticallyVerifiedBlock,
    is_first_block: bool,
) -> Result<(), ValidateContextError> {
    if is_first_block {
        state.commit_new_chain(block, finalized_state)
    } else {
        state.commit_block(block, finalized_state)
    }
}

/// Adds `key` to one of a chain's spent output or revealed nullifier indexes.
fn insert_spend<K: Eq + std::hash::Hash>(
    index: &mut std::collections::HashMap<K, SpendingTransactionId>,
    key: K,
) {
    #[cfg(feature = "indexer")]
    index.insert(key, zebra_chain::transaction::Hash([0; 32]));

    #[cfg(not(feature = "indexer"))]
    index.insert(key, ());
}

/// Returns a ledger strategy for a Testnet that activates Heartwood at height 5 and NU5 at height 7,
/// so almost every block of a generated chain commits to a non-empty history tree.
fn heartwood_onward_ledger_strategy() -> BoxedStrategy<LedgerState> {
    // Keep this early-upgrade schedule within the monetary cap.
    let network = ParametersBuilder::default()
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
        .expect("the activation heights are in upgrade order")
        .extend_funding_streams()
        .expect("the subsidy schedule is valid")
        .to_network()
        .expect("the parameters make a valid network");

    LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, 5, false)
}

/// Commits `blocks` through `commit_new_chain` and `commit_block`, which update the trees in
/// parallel with the rest of the commit, and checks that the committed chain is the same at every
/// height as pushing the blocks one after another with `Chain::push`.
///
/// The first block of `blocks` is the finalized tip's stand-in, so it isn't committed.
fn check_tree_first_commit_equals_sequential_push(
    blocks: &[SemanticallyVerifiedBlock],
    network: &Network,
) -> std::result::Result<(), TestCaseError> {
    let (mut state, finalized_state) = new_test_state(network);
    let mut sequential = Chain::new(
        network,
        Height(0),
        finalized_state.note_commitment_trees_for_tip(),
        finalized_state.history_tree(),
        finalized_state.finalized_value_pool(),
    );

    for (index, block) in blocks.iter().skip(1).cloned().enumerate() {
        // Allows anchor checks to pass
        finalized_state.populate_with_anchors(&block.block);

        let spent_outputs = spent_utxos(&block, &sequential);
        let deferred_pool_balance_change = calculate_deferred_pool_balance_change(
            block.height,
            network,
            sequential.chain_value_pools,
        )?;
        let contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            block.clone(),
            spent_outputs,
            deferred_pool_balance_change,
            network,
            sequential.chain_value_pools,
        )?;
        sequential = sequential
            .push(contextual)
            .expect("generated blocks are valid");

        let height = block.height;
        prop_assert_eq!(
            commit(&mut state, &finalized_state, block, index == 0),
            Ok(()),
            "the block at {:?} must commit",
            height,
        );
        prop_assert_eq!(state.chain_count(), 1);
        let committed = state.best_chain().expect("the state has a chain");
        prop_assert_eq!(
            committed.history_block_commitment_tree(),
            sequential.history_block_commitment_tree(),
            "the committed history tree differs from the pushed one at {:?}",
            height,
        );
        prop_assert!(
            committed.eq_internal_state(&sequential),
            "the committed chain differs from the pushed chain at {:?}",
            height,
        );
    }

    Ok(())
}

/// Committing a chain through `commit_new_chain` and `commit_block`, which update the trees in
/// parallel with the rest of the commit, gives the same chain at every height as pushing its
/// blocks one after another with `Chain::push`.
#[test]
fn tree_first_commit_equals_sequential_push() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(DEFAULT_TREE_FIRST_PROPTEST_CASES)),
    |((chain, count, network, _history_tree) in PreparedChain::default())| {
        check_tree_first_commit_equals_sequential_push(&chain[..count], &network)?;
    });

    Ok(())
}

/// Like [`tree_first_commit_equals_sequential_push`], for a chain whose blocks from Heartwood
/// onward commit to a non-empty history tree, and carry note commitments that change its roots.
///
/// The chain generator computes each block's commitment from its own trees, so a wrong history
/// tree in the committed chain also fails the next block's commitment check.
#[test]
fn tree_first_commit_equals_sequential_push_with_history() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
                               .ok()
                               .and_then(|v| v.parse().ok())
                               .unwrap_or(1)),
    |((chain, _count, network, _history_tree) in PreparedChain::default()
        .with_ledger_strategy(heartwood_onward_ledger_strategy())
        .with_valid_commitments()
        .no_shrink())| {
        let heartwood_height = NetworkUpgrade::Heartwood
            .activation_height(&network)
            .expect("the test network activates Heartwood");
        let blocks_with_history = chain
            .iter()
            .filter(|block| block.height > heartwood_height)
            .count();
        let shielded_blocks_with_history = chain
            .iter()
            .filter(|block| {
                block.height > heartwood_height
                    && (block.block.sapling_note_commitments().next().is_some()
                        || block.block.orchard_note_commitments().next().is_some())
            })
            .count();
        prop_assert!(
            blocks_with_history > 1 && shielded_blocks_with_history > 0,
            "the chain must have blocks that commit to note commitment tree roots: \
             {blocks_with_history} blocks above Heartwood, {shielded_blocks_with_history} shielded",
        );

        check_tree_first_commit_equals_sequential_push(&chain, &network)?;
    });

    Ok(())
}

/// Returns an Orchard note commitment tree with all its leaves filled, so any append fails.
fn full_orchard_tree() -> orchard::tree::NoteCommitmentTree {
    // These mirror the legacy serialization format of `orchard::tree::NoteCommitmentTree`.
    #[derive(Serialize)]
    struct Tree {
        inner: Frontier,
        cached_root: Option<[u8; 32]>,
    }

    #[derive(Serialize)]
    struct Frontier {
        frontier: Option<NonEmptyFrontier>,
    }

    #[derive(Serialize)]
    struct NonEmptyFrontier {
        position: u64,
        leaf: Leaf,
        ommers: Vec<[u8; 32]>,
    }

    #[derive(Serialize)]
    enum Leaf {
        #[allow(dead_code)]
        Left([u8; 32]),
        Right([u8; 32], [u8; 32]),
    }

    use bincode::Options;

    // Zero is a valid Pallas field element, and so a valid node.
    let node = [0; 32];
    let last_position = u64::from(u32::MAX);
    let tree = Tree {
        inner: Frontier {
            frontier: Some(NonEmptyFrontier {
                position: last_position,
                leaf: Leaf::Right(node, node),
                // The last leaf has an ommer at each of the 31 levels above the leaves.
                ommers: vec![node; 31],
            }),
        },
        cached_root: None,
    };

    let bytes = bincode::DefaultOptions::new()
        .serialize(&tree)
        .expect("the mirror types serialize");
    let tree: orchard::tree::NoteCommitmentTree = bincode::DefaultOptions::new()
        .deserialize(&bytes)
        .expect("the mirror types match the Orchard tree format");

    assert_eq!(tree.position(), Some(last_position));
    let mut appended = tree.clone();
    assert_eq!(
        appended.append(Default::default()),
        Err(orchard::tree::NoteCommitmentTreeError::FullTree),
    );

    tree
}

/// A generated chain committed up to the parent of `block`, a block with Orchard note commitments.
struct BeforeOrchardBlock {
    state: NonFinalizedState,
    finalized_state: FinalizedState,
    block: SemanticallyVerifiedBlock,
}

impl BeforeOrchardBlock {
    /// Returns a test whose block also spends an output created by an earlier block.
    fn new() -> Self {
        Self::from_chain(PreparedChain::default(), |block, _network| {
            Self::earlier_spend(block).is_some()
        })
    }

    /// Returns a test whose block has two transactions that spend outputs created by earlier
    /// blocks.
    fn with_two_earlier_spends() -> Self {
        Self::from_chain(PreparedChain::default(), |block, _network| {
            Self::two_transactions_spending_earlier_outputs(block).is_some()
        })
    }

    /// Returns a test in a chain with valid block commitments, whose block's parent, and the block
    /// below that, have non-empty history trees.
    fn with_history() -> Self {
        let chain = PreparedChain::default()
            .with_ledger_strategy(heartwood_onward_ledger_strategy())
            .with_valid_commitments();

        Self::from_chain(chain, |block, network| {
            let heartwood_height = NetworkUpgrade::Heartwood
                .activation_height(network)
                .expect("the test network activates Heartwood");
            block.height.0 >= heartwood_height.0 + 3
        })
    }

    /// Returns a test from the first block of a deterministic `chain` that has Orchard note
    /// commitments and satisfies `is_target`.
    fn from_chain(
        chain: PreparedChain,
        is_target: impl Fn(&SemanticallyVerifiedBlock, &Network) -> bool,
    ) -> Self {
        let (chain, _count, network, _history_tree) = chain
            .new_tree(&mut TestRunner::deterministic())
            .expect("the prepared chain strategy generates a chain")
            .current();

        // The generated genesis block is the finalized tip's stand-in, so it isn't committed.
        let blocks: Vec<_> = chain.iter().skip(1).cloned().collect();

        // The block's parent must be a non-finalized chain tip, so the block isn't the first.
        let target = blocks
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_index, block)| {
                block.block.orchard_note_commitments().next().is_some()
                    && is_target(block, &network)
            })
            .map(|(index, _block)| index)
            .expect("the deterministic chain has an Orchard block that the test can use");

        let (mut state, finalized_state) = new_test_state(&network);
        for (index, block) in blocks[..target].iter().cloned().enumerate() {
            // Allows anchor checks to pass
            finalized_state.populate_with_anchors(&block.block);
            commit(&mut state, &finalized_state, block, index == 0)
                .expect("the prepared chain generates valid blocks");
        }

        Self {
            state,
            finalized_state,
            block: blocks[target].clone(),
        }
    }

    /// Returns an outpoint that `block` spends from an earlier block.
    fn earlier_spend(
        block: &SemanticallyVerifiedBlock,
    ) -> Option<zebra_chain::transparent::OutPoint> {
        block
            .block
            .transactions
            .iter()
            .flat_map(|transaction| transaction.spent_outpoints())
            .find(|outpoint| !block.new_outputs.contains_key(outpoint))
    }

    /// Changes the chain that `block` extends.
    fn change_parent_chain(&mut self, change: impl FnOnce(&mut Chain)) {
        let mut chain = self
            .state
            .chain_set
            .pop_last()
            .expect("the state has a chain");
        change(Arc::make_mut(&mut chain));
        self.state.chain_set.insert(chain);
    }

    /// Fills the parent chain's tip Orchard tree, so the block's tree update fails.
    fn fill_orchard_tree(&mut self) {
        self.change_parent_chain(|chain| {
            let tip_height = chain.non_finalized_tip_height();
            chain
                .orchard_trees_by_height
                .insert(tip_height, Arc::new(full_orchard_tree()));
        });
    }

    /// Adds one of the block's Orchard nullifiers to the parent chain, so its chain update fails.
    fn reveal_orchard_nullifier(&mut self) {
        let nullifier = self
            .block
            .block
            .orchard_nullifiers()
            .next()
            .expect("Orchard actions reveal nullifiers");
        self.change_parent_chain(|chain| {
            insert_spend(&mut chain.orchard_nullifiers, nullifier);
        });
    }

    /// Returns the outpoints spent by each of the first two transactions of `block` that spend only
    /// outputs created by earlier blocks, if it has two.
    fn two_transactions_spending_earlier_outputs(
        block: &SemanticallyVerifiedBlock,
    ) -> Option<[Vec<zebra_chain::transparent::OutPoint>; 2]> {
        let mut spends = block
            .block
            .transactions
            .iter()
            .filter(|transaction| !transaction.is_coinbase())
            .map(|transaction| transaction.spent_outpoints().collect::<Vec<_>>())
            .filter(|outpoints| {
                !outpoints.is_empty()
                    && outpoints
                        .iter()
                        .all(|outpoint| !block.new_outputs.contains_key(outpoint))
            });

        Some([spends.next()?, spends.next()?])
    }

    /// Changes the values of the outputs that two of the block's transactions spend, in the parent
    /// chain, so each transaction spends the maximum value. Each transaction's value balance is
    /// valid, but the block's total overflows, so the block's value balance check fails.
    fn overflow_block_value_balance(&mut self) {
        let spends = Self::two_transactions_spending_earlier_outputs(&self.block)
            .expect("the block has two transactions that spend earlier outputs");
        self.change_parent_chain(|chain| {
            for outpoints in spends {
                for (input, outpoint) in outpoints.into_iter().enumerate() {
                    let mut utxo = chain
                        .created_utxos
                        .get(&outpoint)
                        .expect("the parent chain created the outputs the block spends")
                        .as_ref()
                        .clone();
                    let value = if input == 0 { MAX_MONEY } else { 0 };
                    utxo.utxo.output.value = value.try_into().expect("the value is a valid amount");
                    chain.created_utxos.insert(outpoint, Arc::new(utxo));
                }
            }
        });
    }

    /// Replaces the parent chain's tip history tree with the one below it, so pushing the block onto
    /// it panics.
    fn make_history_tree_stale(&mut self) {
        self.change_parent_chain(|chain| {
            let tip_height = chain.non_finalized_tip_height();
            let below_tip = (tip_height - 1).expect("the parent chain has a block below its tip");
            let stale = chain
                .history_trees_by_height
                .get(&below_tip)
                .expect("the parent chain has a history tree below its tip")
                .clone();
            chain.history_trees_by_height.insert(tip_height, stale);
        });
    }

    /// Changes the block's commitment, so its block commitment check fails.
    fn corrupt_block_commitment(&mut self) {
        self.block = self
            .block
            .block
            .clone()
            .set_block_commitment([0x42; 32])
            .prepare();
    }

    /// Spends one of the block's transparent inputs in the parent chain, so its spend check fails.
    fn spend_transparent_input(&mut self) {
        let outpoint = Self::earlier_spend(&self.block).expect("the block has an earlier spend");
        self.change_parent_chain(|chain| {
            insert_spend(&mut chain.spent_utxos, outpoint);
        });
    }

    /// Commits the block, after making its anchors valid if `with_anchors` is true, and checks that
    /// a rejected block leaves the state unchanged.
    fn commit(mut self, with_anchors: bool) -> Result<(), ValidateContextError> {
        if with_anchors {
            self.finalized_state
                .populate_with_anchors(&self.block.block);
        }

        let before = self.state.clone();
        let result = self
            .state
            .commit_block(self.block.clone(), &self.finalized_state);

        if result.is_err() {
            assert!(
                self.state.eq_internal_state(&before),
                "a rejected block must not change the state",
            );
        }

        result
    }
}

/// Returns true if `result` is the error of an Orchard tree update.
fn is_orchard_tree_error(result: &Result<(), ValidateContextError>) -> bool {
    matches!(
        result,
        Err(ValidateContextError::NoteCommitmentTreeError(
            NoteCommitmentTreeError::Orchard(orchard::tree::NoteCommitmentTreeError::FullTree)
        ))
    )
}

#[test]
fn unchanged_parent_commits_the_orchard_block() {
    let _init_guard = zebra_test::init();

    assert_eq!(BeforeOrchardBlock::new().commit(true), Ok(()));
}

#[test]
fn tree_error_is_returned_when_the_other_checks_pass() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.fill_orchard_tree();
    let result = test.commit(true);

    assert!(is_orchard_tree_error(&result), "{result:?}");
}

#[test]
fn invalidated_block_error_precedes_tree_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.fill_orchard_tree();
    let invalidated = test.block.clone().test_with_zero_spent_utxos();
    test.state
        .invalidated_blocks
        .insert(invalidated.height, Arc::new(vec![invalidated]));
    let result = test.commit(true);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::BlockPreviouslyInvalidated { .. })
        ),
        "{result:?}",
    );
}

#[test]
fn transparent_spend_error_precedes_tree_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.fill_orchard_tree();
    test.spend_transparent_input();
    let result = test.commit(true);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::DuplicateTransparentSpend { .. })
        ),
        "{result:?}",
    );
}

#[test]
fn anchor_error_precedes_tree_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.fill_orchard_tree();
    let result = test.commit(false);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::UnknownSaplingAnchor { .. }
                | ValidateContextError::UnknownOrchardAnchor { .. })
        ),
        "{result:?}",
    );
}

#[test]
fn value_balance_error_precedes_tree_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::with_two_earlier_spends();
    test.fill_orchard_tree();
    test.overflow_block_value_balance();
    let result = test.commit(true);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::CalculateBlockChainValueChange { .. })
        ),
        "{result:?}",
    );
}

#[test]
fn unchanged_parent_commits_the_orchard_block_with_history() {
    let _init_guard = zebra_test::init();

    assert_eq!(BeforeOrchardBlock::with_history().commit(true), Ok(()));
}

#[test]
fn tree_error_is_returned_with_history_when_the_other_checks_pass() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::with_history();
    test.fill_orchard_tree();
    let result = test.commit(true);

    assert!(is_orchard_tree_error(&result), "{result:?}");
}

#[test]
fn block_commitment_error_precedes_tree_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::with_history();
    test.fill_orchard_tree();
    test.corrupt_block_commitment();
    let result = test.commit(true);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::InvalidBlockCommitment(..))
        ),
        "{result:?}",
    );
}

/// The history tree is pushed after the checks before it pass, so a block that fails an earlier
/// check gets that check's error, and never reaches a history push that would panic.
#[test]
fn history_push_waits_for_the_earlier_checks() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::with_history();
    test.make_history_tree_stale();
    let result = test.commit(false);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::UnknownSaplingAnchor { .. }
                | ValidateContextError::UnknownOrchardAnchor { .. })
        ),
        "{result:?}",
    );
}

#[test]
fn tree_error_precedes_chain_update_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.fill_orchard_tree();
    test.reveal_orchard_nullifier();
    let result = test.commit(true);

    assert!(is_orchard_tree_error(&result), "{result:?}");
}

#[test]
fn duplicate_nullifier_gets_its_own_error() {
    let _init_guard = zebra_test::init();

    let mut test = BeforeOrchardBlock::new();
    test.reveal_orchard_nullifier();
    let result = test.commit(true);

    assert!(
        matches!(
            result,
            Err(ValidateContextError::DuplicateOrchardNullifier { .. })
        ),
        "{result:?}",
    );
}

/// Returns a fake child of `parent`, mined `seconds` after it.
fn child_after(parent: &Arc<Block>, seconds: i64) -> Arc<Block> {
    let mut child = parent.make_fake_child();
    Arc::make_mut(&mut Arc::make_mut(&mut child).header).time =
        parent.header.time + Duration::seconds(seconds);
    child
}

/// A block with the wrong difficulty, whose parent is deep in the non-finalized state, is rejected
/// by the recent chain checks before the state forks the parent's chain.
#[test]
fn wrong_difficulty_deep_fork_is_rejected_before_forking() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (finalized, mut state, genesis) = new_state_with_mainnet_genesis();
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("block 1 test vector is valid");
    state
        .commit_new_chain(block1.clone().prepare(), &finalized.db)
        .expect("block 1 is a valid child of genesis");

    // `blocks[height]` is the block at `height`, with blocks at the pre-Blossom target spacing.
    let mut blocks = vec![genesis.block.clone(), block1];
    for _ in 2..=40 {
        let child = child_after(blocks.last().expect("blocks has block 1"), 150);
        state
            .commit_block(child.clone().prepare(), &finalized.db)
            .expect("fake children of block 1 are valid");
        blocks.push(child);
    }

    // The fork parent has the full difficulty adjustment context below it, so the recent chain
    // checks run in tests, and it is ten blocks below the tip.
    let fork_height = 30;
    let fork_parent = &blocks[fork_height];

    let mut correct = child_after(fork_parent, 151);
    let context = blocks[..=fork_height]
        .iter()
        .rev()
        .map(|block| (block.header.difficulty_threshold, block.header.time));
    let expected_difficulty = AdjustedDifficulty::new_from_block(&correct, &network, context)
        .expected_difficulty_threshold();
    Arc::make_mut(&mut Arc::make_mut(&mut correct).header).difficulty_threshold =
        expected_difficulty;

    let wrong = child_after(fork_parent, 150).set_work(1);
    assert_ne!(wrong.header.difficulty_threshold, expected_difficulty);

    let before = state.clone();
    let forks_before = FORK_COUNT.with(Cell::get);
    let result = validate_and_commit_non_finalized(&finalized.db, &mut state, wrong.prepare());

    assert!(
        matches!(
            result,
            Err(ValidateContextError::InvalidDifficultyThreshold { .. })
        ),
        "{result:?}",
    );
    assert!(
        state.eq_internal_state(&before),
        "a block with the wrong difficulty must not change the state",
    );
    assert_eq!(
        FORK_COUNT.with(Cell::get),
        forks_before,
        "a block with the wrong difficulty must be rejected before its parent chain is forked",
    );

    // The same fork with the right difficulty is committed by forking the parent chain.
    validate_and_commit_non_finalized(&finalized.db, &mut state, correct.prepare())
        .expect("a deep fork with the right difficulty is valid");
    assert_eq!(FORK_COUNT.with(Cell::get), forks_before + 1);
    assert_eq!(state.chain_count(), 2);
}
