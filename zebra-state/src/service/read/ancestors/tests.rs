//! Fixed test vectors for reading a block's ancestor chain.

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use tower::ServiceExt;
use zebra_chain::{
    block::{self, Block},
    parameters::Network::Mainnet,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
};
use zebra_test::prelude::Result;

use crate::{
    arbitrary::Prepare,
    populated_state,
    service::{finalized_state::FinalizedState, non_finalized_state::NonFinalizedState},
    tests::FakeChainHelper,
    CheckpointVerifiedBlock, Config, ReadRequest, ReadResponse,
};

use super::ancestor_blocks;

/// The heights committed to the finalized state; the rest are non-finalized.
const FINALIZED_HEIGHTS: u32 = 5;

/// A state whose continuous Mainnet blocks span the finalized and
/// non-finalized states, with a two-block side chain forked in memory.
struct Fixture {
    finalized_state: FinalizedState,
    non_finalized_state: NonFinalizedState,
    /// The best chain, indexed by height.
    best: Vec<Arc<Block>>,
    /// The side chain blocks, lowest first.
    side: Vec<Arc<Block>>,
}

impl Fixture {
    /// Returns the walk from `hash`, through every chain and the database.
    fn walk(&self, hash: block::Hash, count: u32, max_bytes: u64) -> Vec<block::Hash> {
        ancestor_blocks(
            self.non_finalized_state.chain_iter(),
            &self.finalized_state.db,
            hash,
            count,
            max_bytes,
        )
        .iter()
        .map(|block| block.hash())
        .collect()
    }
}

/// Builds the [`Fixture`].
fn fixture() -> Result<Fixture> {
    let best: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into().unwrap())
        .collect();
    assert!(
        best.len() > FINALIZED_HEIGHTS as usize + 2,
        "the fixture needs at least two non-finalized best chain blocks"
    );

    let mut finalized_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    let mut non_finalized_state = NonFinalizedState::new(&Mainnet);

    // `FINALIZED_HEIGHTS` is a small constant.
    let (finalized, non_finalized) = best.split_at(FINALIZED_HEIGHTS as usize);
    for block in finalized {
        finalized_state.commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "ancestor test",
        )?;
    }

    non_finalized_state.commit_new_chain(non_finalized[0].clone().prepare(), &finalized_state)?;
    for block in &non_finalized[1..] {
        non_finalized_state.commit_block(block.clone().prepare(), &finalized_state)?;
    }

    // Fork two lower-work blocks from the second non-finalized block.
    let fork_parent = &non_finalized[1];
    let side_1 = fork_parent.make_fake_child().set_work(1);
    let side_2 = side_1.make_fake_child().set_work(1);
    for block in [&side_1, &side_2] {
        non_finalized_state.commit_block(block.clone().prepare(), &finalized_state)?;
    }
    assert_eq!(non_finalized_state.chain_count(), 2);
    assert_eq!(
        non_finalized_state.best_tip().map(|(_, hash)| hash),
        best.last().map(|block| block.hash()),
        "the real chain stays the best chain"
    );

    Ok(Fixture {
        finalized_state,
        non_finalized_state,
        best,
        side: vec![side_1, side_2],
    })
}

/// Returns the hashes of `blocks`, highest first.
fn descending(blocks: &[Arc<Block>]) -> Vec<block::Hash> {
    blocks.iter().rev().map(|block| block.hash()).collect()
}

/// Returns the serialized size of `block`.
fn size(block: &Block) -> u64 {
    block.zcash_serialized_size() as u64
}

#[test]
fn walks_the_best_chain_across_the_finalized_boundary_to_genesis() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;
    let tip = fixture.best.last().unwrap().hash();

    // Asking for more blocks than exist stops at genesis.
    assert_eq!(
        fixture.walk(tip, u32::MAX, u64::MAX),
        descending(&fixture.best)
    );

    // Each prefix length ends exactly at `count`, on either side of the boundary.
    for count in 1..=fixture.best.len() {
        let expected: Vec<_> = descending(&fixture.best).into_iter().take(count).collect();
        assert_eq!(
            fixture.walk(tip, count.try_into().unwrap(), u64::MAX),
            expected
        );
    }

    Ok(())
}

#[test]
fn starts_from_a_finalized_block() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;

    let start = FINALIZED_HEIGHTS as usize - 2;
    let hash = fixture.best[start].hash();
    assert_eq!(
        fixture.walk(hash, u32::MAX, u64::MAX),
        descending(&fixture.best[..=start])
    );

    // The finalized tip, just below the in-memory root.
    let tip = FINALIZED_HEIGHTS as usize - 1;
    assert_eq!(
        fixture.walk(fixture.best[tip].hash(), 2, u64::MAX),
        descending(&fixture.best[tip - 1..=tip])
    );

    Ok(())
}

#[test]
fn walks_a_side_chain_into_the_shared_prefix() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;

    // The side chain forks from the second non-finalized block.
    let fork_height = FINALIZED_HEIGHTS as usize + 1;
    let mut expected = descending(&fixture.side);
    expected.extend(descending(&fixture.best[..=fork_height]));

    let side_tip = fixture.side.last().unwrap().hash();
    assert_eq!(fixture.walk(side_tip, u32::MAX, u64::MAX), expected);

    // The best-chain block at the side chain's height is never served.
    assert!(!fixture
        .walk(side_tip, u32::MAX, u64::MAX)
        .contains(&fixture.best[fork_height + 1].hash()));

    Ok(())
}

#[test]
fn max_bytes_exempts_the_first_block() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;
    let tip_index = fixture.best.len() - 1;
    let tip = fixture.best[tip_index].hash();

    // A bound below the first block still returns it.
    assert_eq!(fixture.walk(tip, u32::MAX, 0), vec![tip]);
    assert_eq!(fixture.walk(tip, u32::MAX, 1), vec![tip]);

    // A bound of exactly the running size of the first n blocks returns n
    // blocks; one byte less returns n - 1 (but never fewer than one).
    let expected = descending(&fixture.best);
    let mut running = 0;
    for (n, block) in fixture.best.iter().rev().enumerate() {
        running += size(block);
        assert_eq!(
            fixture.walk(tip, u32::MAX, running),
            expected[..=n],
            "a bound of exactly {running} bytes returns {} blocks",
            n + 1,
        );
        assert_eq!(
            fixture.walk(tip, u32::MAX, running - 1),
            expected[..n.max(1)],
            "a bound one byte short of {running} returns {} blocks",
            n.max(1),
        );
    }

    Ok(())
}

#[test]
fn unknown_hash_and_zero_count_return_nothing() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;

    assert!(fixture
        .walk(block::Hash([0xff; 32]), u32::MAX, u64::MAX)
        .is_empty());

    for block in fixture.best.iter().chain(&fixture.side) {
        assert!(fixture.walk(block.hash(), 0, u64::MAX).is_empty());
    }

    Ok(())
}

#[test]
fn each_block_is_the_parent_of_the_one_before() -> Result<()> {
    let _init_guard = zebra_test::init();
    let fixture = fixture()?;

    for start in fixture.best.iter().chain(&fixture.side) {
        let blocks = ancestor_blocks(
            fixture.non_finalized_state.chain_iter(),
            &fixture.finalized_state.db,
            start.hash(),
            u32::MAX,
            u64::MAX,
        );
        assert_eq!(blocks[0].hash(), start.hash());
        for pair in blocks.windows(2) {
            assert_eq!(pair[1].hash(), pair[0].header.previous_block_hash);
        }
        assert_eq!(
            blocks.last().unwrap().coinbase_height(),
            Some(block::Height(0))
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn read_state_serves_ancestor_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .map(|bytes| bytes.zcash_deserialize_into().unwrap())
        .collect();
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), &Mainnet).await;

    let request = |hash, count| ReadRequest::AncestorBlocks {
        hash,
        count,
        max_bytes: u64::MAX,
    };
    let hashes = |response| match response {
        ReadResponse::AncestorBlocks(blocks) => {
            blocks.iter().map(|block| block.hash()).collect::<Vec<_>>()
        }
        response => panic!("unexpected response {response:?}"),
    };

    let tip = blocks.last().unwrap().hash();
    let response = read_state
        .clone()
        .oneshot(request(tip, u32::MAX))
        .await
        .unwrap();
    assert_eq!(hashes(response), descending(&blocks));

    let response = read_state.clone().oneshot(request(tip, 3)).await.unwrap();
    assert_eq!(hashes(response), descending(&blocks[blocks.len() - 3..]));

    let response = read_state
        .oneshot(request(block::Hash([0xff; 32]), u32::MAX))
        .await
        .unwrap();
    assert!(hashes(response).is_empty());

    Ok(())
}

/// The number of finalized blocks walked by [`walks_30k_small_finalized_blocks`].
const TIMED_WALK_BLOCKS: u32 = 30_000;

/// Walks 30,000 small finalized blocks, and logs how long the walk took, for
/// the design's acceptance target of under a second in release builds.
///
/// The time is logged rather than asserted: CI runs ignored tests on loaded
/// runners, where a wall-clock bound would be flaky.
///
/// Ignored because committing the blocks takes a while. Run it with:
/// `cargo nextest run --release -p zebra-state --run-ignored=only walks_30k --no-capture`
#[test]
#[ignore]
fn walks_30k_small_finalized_blocks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut finalized_state = FinalizedState::new_with_debug(
        &Config::ephemeral(),
        &Mainnet,
        true,
        #[cfg(feature = "elasticsearch")]
        false,
        false,
    )?;
    let non_finalized_state = NonFinalizedState::new(&Mainnet);

    // Genesis, then fake children of block 1, which are all small
    // pre-Sapling blocks.
    let genesis: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    for parent in [genesis, block.clone()] {
        finalized_state.commit_finalized_direct(
            CheckpointVerifiedBlock::from(parent).into(),
            None,
            "ancestor timing test",
        )?;
    }
    let mut total_bytes = size(&block);
    for _ in 2..TIMED_WALK_BLOCKS {
        block = block.make_fake_child();
        total_bytes += size(&block);
        finalized_state.commit_finalized_direct(
            CheckpointVerifiedBlock::from(block.clone()).into(),
            None,
            "ancestor timing test",
        )?;
    }

    let start = std::time::Instant::now();
    let blocks = ancestor_blocks(
        non_finalized_state.chain_iter(),
        &finalized_state.db,
        block.hash(),
        u32::MAX,
        u64::MAX,
    );
    let elapsed = start.elapsed();

    // Every committed block is walked, down to genesis.
    assert_eq!(blocks.len(), TIMED_WALK_BLOCKS as usize);
    assert_eq!(blocks[0].hash(), block.hash());
    assert_eq!(
        blocks.last().unwrap().coinbase_height(),
        Some(block::Height(0))
    );

    tracing::info!(
        ?elapsed,
        blocks = blocks.len(),
        total_bytes,
        "walked the finalized ancestor chain"
    );

    Ok(())
}
