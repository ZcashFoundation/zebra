//! Tests for the Zebra state service.

use std::{mem, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    transaction::Transaction,
    transparent,
    work::difficulty::ExpandedDifficulty,
    work::difficulty::{Work, U256},
};

use super::*;

pub mod setup;

/// Helper trait for constructing "valid" looking chains of blocks
pub trait FakeChainHelper {
    fn make_fake_child(&self) -> Arc<Block>;

    fn set_work(self, work: u128) -> Arc<Block>;

    fn set_block_commitment(self, commitment: [u8; 32]) -> Arc<Block>;
}

impl FakeChainHelper for Arc<Block> {
    fn make_fake_child(&self) -> Arc<Block> {
        let parent_hash = self.hash();
        let mut child = Block::clone(self);
        let mut transactions = mem::take(&mut child.transactions);
        let mut tx = transactions.remove(0);

        let input = match Arc::make_mut(&mut tx) {
            Transaction::V1 { inputs, .. } => &mut inputs[0],
            Transaction::V2 { inputs, .. } => &mut inputs[0],
            Transaction::V3 { inputs, .. } => &mut inputs[0],
            Transaction::V4 { inputs, .. } => &mut inputs[0],
            Transaction::V5 { inputs, .. } => &mut inputs[0],
        };

        match input {
            transparent::Input::Coinbase { height, .. } => height.0 += 1,
            _ => panic!("block must have a coinbase height to create a child"),
        }

        child.transactions.push(tx);
        Arc::make_mut(&mut child.header).previous_block_hash = parent_hash;

        Arc::new(child)
    }

    fn set_work(mut self, work: u128) -> Arc<Block> {
        let work: U256 = work.into();
        let expanded = work_to_expanded(work);

        let block = Arc::make_mut(&mut self);
        Arc::make_mut(&mut block.header).difficulty_threshold = expanded.into();
        self
    }

    fn set_block_commitment(mut self, block_commitment: [u8; 32]) -> Arc<Block> {
        let block = Arc::make_mut(&mut self);
        Arc::make_mut(&mut block.header).commitment_bytes = block_commitment;
        self
    }
}

fn work_to_expanded(work: U256) -> ExpandedDifficulty {
    // Work is calculated from expanded difficulty with the equation `work = 2^256 / (expanded + 1)`
    // By balancing the equation we get `expanded = (2^256 / work) - 1`
    // `2^256` is too large to represent, so we instead use the following equivalent equations
    // `expanded = (2^256 / work) - (work / work)`
    // `expanded = (2^256 - work) / work`
    // `expanded = ((2^256 - 1) - work + 1) / work`
    // `(2^256 - 1 - work)` is equivalent to `!work` when work is a U256
    let expanded = (!work + 1) / work;
    ExpandedDifficulty::from(expanded)
}

/// Block heights, and the expected minimum block locator height
static BLOCK_LOCATOR_CASES: &[(u32, u32)] = &[
    (0, 0),
    (1, 0),
    (10, 0),
    (98, 0),
    (99, 0),
    (100, 1),
    (101, 2),
    (1000, 901),
    (10000, 9901),
];

use proptest::prelude::*;

#[test]
fn round_trip_work_expanded() {
    let _init_guard = zebra_test::init();

    proptest!(|(work_before in any::<Work>())| {
        let work: U256 = work_before.as_u128().into();
        let expanded = work_to_expanded(work);
        let work_after = Work::try_from(expanded).unwrap();
        prop_assert_eq!(work_before, work_after);
    });
}

/// Check that the block locator heights are sensible.
#[test]
fn test_block_locator_heights() {
    let _init_guard = zebra_test::init();

    for (height, min_height) in BLOCK_LOCATOR_CASES.iter().cloned() {
        let locator = util::block_locator_heights(block::Height(height));

        assert!(!locator.is_empty(), "locators must not be empty");
        if (height - min_height) > 1 {
            assert!(
                locator.len() > 2,
                "non-trivial locators must have some intermediate heights"
            );
        }

        assert_eq!(
            locator[0],
            block::Height(height),
            "locators must start with the tip height"
        );

        // Check that the locator is sorted, and that it has no duplicates
        // TODO: replace with dedup() and is_sorted_by() when sorting stabilises.
        assert!(locator.windows(2).all(|v| match v {
            [a, b] => a.0 > b.0,
            _ => unreachable!("windows returns exact sized slices"),
        }));

        let final_height = locator[locator.len() - 1];
        assert_eq!(
            final_height,
            block::Height(min_height),
            "locators must end with the specified final height"
        );
        assert!(height - final_height.0 <= constants::MAX_BLOCK_REORG_HEIGHT,
                    "locator for {} must not be more than the maximum reorg height {} below the tip, but {} is {} blocks below the tip",
                    height,
                    constants::MAX_BLOCK_REORG_HEIGHT,
                    final_height.0,
                    height - final_height.0);
    }
}
