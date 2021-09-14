use super::*;

use zebra_chain::{
    block::Block, parameters::Network, serialization::ZcashDeserializeInto, transaction::UnminedTx,
};

use color_eyre::eyre::Result;

#[test]
fn mempool_storage_crud_mainnet() {
    zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Default::default();

    // Get transactions from the first 10 blocks of the Zcash blockchain
    let (_, unmined_transactions) = unmined_transactions_in_blocks(10, network);

    // Get one (1) unmined transaction
    let unmined_tx = &unmined_transactions[0];

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone());

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains(&unmined_tx.id));

    // Remove tx
    let _ = storage.remove(&unmined_tx.id);

    // Check that it is /not/ in the mempool.
    assert!(!storage.contains(&unmined_tx.id));
}

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
    for tx in unmined_transactions.iter().rev().take(MEMPOOL_SIZE) {
        assert!(storage.contains(&tx.id));
    }

    // Anything greater should not be in the verified
    for tx in unmined_transactions
        .iter()
        .take(unmined_transactions.len() - MEMPOOL_SIZE)
    {
        assert!(!storage.contains(&tx.id));
    }

    // Query all the ids we have for rejected, get back `total - MEMPOOL_SIZE`
    let all_ids: HashSet<UnminedTxId> = unmined_transactions.iter().map(|tx| tx.id).collect();
    let rejected_ids: HashSet<UnminedTxId> = unmined_transactions
        .iter()
        .take(total_transactions - MEMPOOL_SIZE)
        .map(|tx| tx.id)
        .collect();
    // Convert response to a `HashSet` as we need a fixed order to compare.
    let rejected_response: HashSet<UnminedTxId> =
        storage.rejected_transactions(all_ids).into_iter().collect();

    assert_eq!(rejected_response, rejected_ids);

    // Use `contains_rejected` to make sure the first id stored is now rejected
    assert!(storage.contains_rejected(&unmined_transactions[0].id));
    // Use `contains_rejected` to make sure the last id stored is not rejected
    assert!(!storage.contains_rejected(&unmined_transactions[unmined_transactions.len() - 1].id));

    Ok(())
}

pub fn unmined_transactions_in_blocks(
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
