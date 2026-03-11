//! Tests for the Zebra state service.

#![allow(clippy::unwrap_in_result)]

use std::{mem, sync::Arc};

use zebra_chain::{
    block::Block,
    transaction::Transaction,
    transparent,
    work::difficulty::ExpandedDifficulty,
    work::difficulty::{Work, U256},
};

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
            #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
            Transaction::V6 { inputs, .. } => &mut inputs[0],
        };

        match input {
            transparent::Input::Coinbase { height, .. } => height.0 += 1,
            _ => panic!("block must have a coinbase height to create a child"),
        }

        child.transactions.insert(0, tx);
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
        Arc::make_mut(&mut block.header).commitment_bytes = block_commitment.into();
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

#[test]
fn round_trip_work_expanded() {
    use proptest::prelude::*;

    let _init_guard = zebra_test::init();

    proptest!(|(work_before in any::<Work>())| {
        let work: U256 = work_before.as_u128().into();
        let expanded = work_to_expanded(work);
        let work_after = Work::try_from(expanded).unwrap();
        prop_assert_eq!(work_before, work_after);
    });
}
