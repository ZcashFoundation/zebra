use super::*;

use zebra_chain::{
    block::Block, parameters::Network, serialization::ZcashDeserializeInto, transaction::UnminedTx,
};

use color_eyre::eyre::Result;

#[test]
fn mempool_storage_basic() -> Result<()> {
    zebra_test::init();

    mempool_storage_basic_for_network(Network::Mainnet)?;
    mempool_storage_basic_for_network(Network::Testnet)?;

    Ok(())
}

fn mempool_storage_basic_for_network(network: Network) -> Result<()> {
    // Create an empty storage
    let mut storage: Storage = Default::default();

    // Get transactions from the first 10 blocks of the Zcash blockchain
    let (total_transactions, unmined_transactions) = unmined_transactions_in_blocks(10, network);

    // Insert them all to the storage
    for unmined_transaction in unmined_transactions.clone() {
        storage.insert(unmined_transaction)?;
    }

    // Only MEMPOOL_SIZE should land in verified
    assert_eq!(storage.verified.len(), MEMPOOL_SIZE);

    // The rest of the transactions will be in rejected
    assert_eq!(storage.rejected.len(), total_transactions - MEMPOOL_SIZE);

    // Make sure the last MEMPOOL_SIZE transactions we sent are in the verified
    for count in 1..MEMPOOL_SIZE {
        assert!(storage
            .clone()
            .contains(&unmined_transactions[total_transactions - count].id));
    }

    // Anything greater should not be in the verified
    for count in MEMPOOL_SIZE + 1..total_transactions {
        assert!(!storage
            .clone()
            .contains(&unmined_transactions[total_transactions - count].id));
    }

    Ok(())
}

fn unmined_transactions_in_blocks(
    last_block_height: u32,
    network: Network,
) -> (usize, Vec<UnminedTx>) {
    let mut transactions = vec![];
    let mut total = 0;

    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (&height, block) in block_iter {
        if height <= last_block_height {
            let block = block
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid");

            for transaction in block.transactions.iter() {
                transactions.push(UnminedTx::from(transaction));
                total += 1;
            }
        }
    }

    (total, transactions)
}
