use std::ops::RangeBounds;

use zebra_chain::{
    block::Block, parameters::Network, serialization::ZcashDeserializeInto, transaction::UnminedTx,
};

mod prop;
mod vectors;

pub fn unmined_transactions_in_blocks(
    block_height_range: impl RangeBounds<u32>,
    network: Network,
) -> impl DoubleEndedIterator<Item = UnminedTx> {
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

    // Extract the transactions from the blocks and warp each one as an unmined transaction.
    selected_blocks
        .flat_map(|block| block.transactions)
        .map(UnminedTx::from)
}
