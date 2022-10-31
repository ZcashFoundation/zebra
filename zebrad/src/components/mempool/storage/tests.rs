//! Tests and test utility functions for mempool storage.

use std::ops::RangeBounds;

use zebra_chain::{
    amount::Amount,
    block::Block,
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTx, VerifiedUnminedTx},
};

mod prop;
mod vectors;

pub fn unmined_transactions_in_blocks(
    block_height_range: impl RangeBounds<u32>,
    network: Network,
) -> impl DoubleEndedIterator<Item = VerifiedUnminedTx> {
    let blocks = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    // Deserialize the blocks that are selected based on the specified `block_height_range`.
    let selected_blocks = blocks
        .filter(move |(&height, _)| block_height_range.contains(&height))
        .map(|(_, block)| {
            block
                .zcash_deserialize_into::<Block>()
                .expect("block test vector is structurally valid")
        });

    // Extract the transactions from the blocks and wrap each one as an unmined transaction.
    // Use a fake zero miner fee and sigops, because we don't have the UTXOs to calculate
    // the correct fee.
    selected_blocks
        .flat_map(|block| block.transactions)
        .map(UnminedTx::from)
        .map(|transaction| VerifiedUnminedTx::new(transaction, Amount::zero(), 0))
}
