use std::{mem, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    transaction::Transaction,
    transparent,
};

use super::*;

/// Helper trait for constructing "valid" looking chains of blocks
pub trait FakeChainHelper {
    fn make_fake_child(&self) -> Arc<Block>;
}

impl FakeChainHelper for Block {
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
        };

        match input {
            transparent::Input::Coinbase { height, .. } => height.0 += 1,
            _ => panic!("block must have a coinbase height to create a child"),
        }

        child.transactions.push(tx);
        child.header.previous_block_hash = parent_hash;

        Arc::new(child)
    }
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

/// Check that the block locator heights are sensible.
#[test]
fn test_block_locator_heights() {
    zebra_test::init();

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
                    format!("locator for {} must not be more than the maximum reorg height {} below the tip, but {} is {} blocks below the tip",
                         height,
                         constants::MAX_BLOCK_REORG_HEIGHT,
                         final_height.0,
                         height - final_height.0));
    }
}
