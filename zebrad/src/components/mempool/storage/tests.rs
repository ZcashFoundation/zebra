use super::*;

use zebra_chain::parameters::Network;

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto, transaction::UnminedTx};

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

    // Get transactions from test vectors
    let (total_transactions, unmined_transactions) = unmined_transactions_in_blocks(10, network);

    // Insert them all to the storage
    for unmined_transaction in unmined_transactions.clone() {
        storage.insert(unmined_transaction)?;
    }

    // Only MEMPOOL_SIZE should land in verified
    assert_eq!(storage.verified.len(), MEMPOOL_SIZE);

    // The rest of the transactions will be in rejected
    assert_eq!(storage.rejected.len(), total_transactions - MEMPOOL_SIZE);

    // TODO: Resolve the following checks with a loop for when `MEMPOOL_SIZE` changes:

    // Make sure the last 2 transactions we sent are in the verified
    assert_eq!(
        storage
            .clone()
            .contains(&unmined_transactions[total_transactions - 1].id),
        true
    );
    assert_eq!(
        storage
            .clone()
            .contains(&unmined_transactions[total_transactions - 2].id),
        true
    );

    // But the third one is not
    assert_eq!(
        storage
            .clone()
            .contains(&unmined_transactions[total_transactions - 3].id),
        false
    );

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
