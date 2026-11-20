//! The chain info for a block template, computed from the best chain alone, must match the chain
//! info that is read from the whole state.

use std::{env, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use proptest::prelude::*;

use zebra_chain::{
    amount::NonNegative,
    block::Block,
    parameters::Network,
    serialization::{DateTime32, ZcashDeserializeInto},
    value_balance::ValueBalance,
};

use crate::{
    arbitrary::Prepare,
    service::{
        check::difficulty::pow_adjustment_block_span,
        check::RecentHeaders,
        finalized_state::FinalizedState,
        non_finalized_state::NonFinalizedState,
        read::difficulty::{chain_info_from_chain, get_block_template_chain_info, PreparedTip},
    },
    tests::FakeChainHelper,
    Config,
};

const DEFAULT_CHAIN_INFO_PROPTEST_CASES: u32 = 16;

/// The most blocks that a generated chain has above the ones that the difficulty adjustment of
/// its next block reads.
const MAX_EXTRA_BLOCKS: usize = 3;

/// The most headers that a generated chain needs: the largest block span, and the extra blocks.
const MAX_CHAIN_BLOCKS: usize = 28 + MAX_EXTRA_BLOCKS;

proptest! {
    #![proptest_config(
        ProptestConfig::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|cases| cases.parse().ok())
            .unwrap_or(DEFAULT_CHAIN_INFO_PROPTEST_CASES))
    )]

    /// On Mainnet and Testnet, a best chain that holds every header that its next block's
    /// difficulty adjustment reads gives the same chain info as a read of the whole state, and a
    /// chain with one header fewer gives none.
    ///
    /// Each block has its own difficulty threshold and time, so an answer that reads the wrong
    /// headers differs. The tip is up to half an hour old: Testnet's next block is a
    /// minimum-difficulty block when its tip is older than the minimum difficulty gap, and a
    /// standard block otherwise.
    #[test]
    fn chain_info_from_the_best_chain_matches_the_state(
        network in prop_oneof![Just(Network::Mainnet), Just(Network::new_default_testnet())],
        extra_blocks in 0..=MAX_EXTRA_BLOCKS,
        tip_age in 0i64..1_800,
        // The work of each block, and the time since its parent, from the tip down.
        headers in prop::collection::vec(((1u128 << 20)..(1u128 << 60), 1i64..600), MAX_CHAIN_BLOCKS),
    ) {
        let _init_guard = zebra_test::init();

        // These blocks are before Heartwood, so their chains need no history tree from the
        // finalized state.
        let first: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let block_span = pow_adjustment_block_span(&network, first.coinbase_height().unwrap());
        let chain_blocks = block_span + extra_blocks;
        prop_assert!(chain_blocks <= MAX_CHAIN_BLOCKS);

        // Block times count back from the tip's time, in whole seconds.
        let tip_time = DateTime::<Utc>::from(DateTime32::now()) - Duration::seconds(tip_age);
        let mut times = Vec::new();
        let mut time = tip_time;
        for (_work, spacing) in headers.iter().take(chain_blocks) {
            times.push(time);
            time -= Duration::seconds(*spacing);
        }
        let headers = headers.iter().take(chain_blocks).zip(times).rev();

        let mut state = NonFinalizedState::new(&network);
        let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
            .expect("opening an ephemeral database should succeed");
        finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());

        let mut parent: Option<Arc<Block>> = None;
        for ((work, _spacing), time) in headers {
            let block = parent
                .as_ref()
                .map_or_else(|| first.clone(), |parent| parent.make_fake_child());
            let mut block = block.set_work(*work);
            Arc::make_mut(&mut Arc::make_mut(&mut block).header).time = time;

            if parent.is_none() {
                state.commit_new_chain(block.clone().prepare(), &finalized_state)?;
            } else {
                state.commit_block(block.clone().prepare(), &finalized_state)?;
            }
            parent = Some(block);

            let chain = state.best_chain().expect("a block was committed");
            if chain.len() < block_span {
                prop_assert!(chain_info_from_chain(chain, &network).is_none());
            }
        }

        // Both answers use the local clock, so they are repeated if the clock's second changes
        // between them.
        let (from_chain, from_state) = loop {
            let second = DateTime32::now();
            let from_chain =
                chain_info_from_chain(state.best_chain().expect("blocks were committed"), &network);
            let from_state = get_block_template_chain_info(&state, &finalized_state.db, &network);
            if DateTime32::now() == second {
                break (from_chain, from_state);
            }
        };
        let from_chain = from_chain
            .expect("the chain holds every header")
            .expect("the chain's headers give a valid chain info");
        let from_state = from_state.expect("the state's headers give a valid chain info");
        prop_assert_eq!(from_chain, from_state);
    }
}

/// A block whose header commits to its final Sapling root has no prepared tip: a commit doesn't
/// check that root, and the mining candidate's own computation does. Other blocks have one.
#[test]
fn only_a_block_without_a_final_sapling_root_commitment_has_a_prepared_tip() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // Mainnet block 653,599 is after Sapling and before Heartwood, and block 1 is before Sapling.
    let blocks: [(Arc<Block>, bool); 2] = [
        (Arc::new(network.test_block(653599, 583999).unwrap()), false),
        (
            zebra_test::vectors::BLOCK_MAINNET_1_BYTES
                .zcash_deserialize_into()
                .unwrap(),
            true,
        ),
    ];
    for (block, has_prepared_tip) in blocks {
        let mut state = NonFinalizedState::new(&network);
        let finalized_state = FinalizedState::new(&Config::ephemeral(), &network)
            .expect("opening an ephemeral database should succeed");
        finalized_state
            .set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
        state
            .commit_new_chain(block.clone().prepare(), &finalized_state)
            .unwrap();
        let chain = state.best_chain().expect("the block was committed");
        let recent_headers = RecentHeaders::read(&state, &finalized_state.db, block.hash(), 1)
            .expect("the block is in the chain");

        assert_eq!(
            PreparedTip::new(chain, &block, &network, recent_headers).is_some(),
            has_prepared_tip,
        );
    }
}
