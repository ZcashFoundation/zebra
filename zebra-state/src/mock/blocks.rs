//! Shared block construction and trusted commits for mock databases.

use std::sync::Arc;

use zebra_chain::{
    block::{self, Block, ChainHistoryBlockTxAuthCommitmentHash},
    parameters::{Network, NetworkUpgrade},
    transaction::{LockTime, Transaction},
    transparent,
};

use crate::{service::finalized_state::FinalizedState, BoxError, CheckpointVerifiedBlock};

/// Commits `block` to `state`, trusting it like a snapshot-imported block.
pub(super) fn commit(state: &mut FinalizedState, block: Arc<Block>) -> Result<(), BoxError> {
    state.commit_finalized_direct(
        CheckpointVerifiedBlock::from(block).into(),
        None,
        "mock state builder",
    )?;
    Ok(())
}

/// Builds a child whose commitments are derived from the supplied finalized state.
pub(super) fn child(
    network: &Network,
    state: &FinalizedState,
    parent: &Block,
    coinbase: Vec<transparent::Output>,
    transactions: Vec<Transaction>,
) -> Arc<Block> {
    let height = parent
        .coinbase_height()
        .expect("mock parent blocks have a coinbase height")
        .next()
        .expect("the mock chain is below the maximum height");

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

    let history_root = state
        .db
        .history_tree()
        .hash()
        .unwrap_or_else(|| block::CHAIN_HISTORY_ACTIVATION_RESERVED.into());
    let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
        &history_root,
        &block.auth_data_root(),
    );
    let commitment: [u8; 32] = commitment.into();

    Arc::make_mut(&mut block.header).commitment_bytes = commitment.into();
    Arc::new(block)
}
