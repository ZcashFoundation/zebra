use std::iter;

use super::{super::*, unmined_transactions_in_blocks};

use zebra_chain::parameters::Network;

use color_eyre::eyre::Result;

#[test]
fn mempool_storage_crud_exact_mainnet() {
    zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Default::default();

    // Get one (1) unmined transaction
    let unmined_tx = unmined_transactions_in_blocks(.., network)
        .next()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone());

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains(&unmined_tx.id));

    // Remove tx
    let removal_count = storage.remove_exact(&iter::once(unmined_tx.id).collect());

    // Check that it is /not/ in the mempool.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains(&unmined_tx.id));
}

#[test]
fn mempool_storage_crud_same_effects_mainnet() {
    zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Default::default();

    // Get one (1) unmined transaction
    let unmined_tx = unmined_transactions_in_blocks(.., network)
        .next()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone());

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains(&unmined_tx.id));

    // Remove tx
    let removal_count =
        storage.remove_same_effects(&iter::once(unmined_tx.id.mined_id()).collect());

    // Check that it is /not/ in the mempool.
    assert_eq!(removal_count, 1);
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
    let unmined_transactions: Vec<_> = unmined_transactions_in_blocks(..=10, network).collect();
    let total_transactions = unmined_transactions.len();

    // Insert them all to the storage
    for unmined_transaction in unmined_transactions.clone() {
        storage.insert(unmined_transaction)?;
    }

    // Separate transactions into the ones expected to be in the mempool and those expected to be
    // rejected.
    let rejected_transaction_count = total_transactions - MEMPOOL_SIZE;
    let expected_to_be_rejected = &unmined_transactions[..rejected_transaction_count];
    let expected_in_mempool = &unmined_transactions[rejected_transaction_count..];

    // Only MEMPOOL_SIZE should land in verified
    assert_eq!(storage.verified.len(), MEMPOOL_SIZE);

    // The rest of the transactions will be in rejected
    assert_eq!(storage.rejected.len(), rejected_transaction_count);

    // Make sure the last MEMPOOL_SIZE transactions we sent are in the verified
    for tx in expected_in_mempool {
        assert!(storage.contains(&tx.id));
    }

    // Anything greater should not be in the verified
    for tx in expected_to_be_rejected {
        assert!(!storage.contains(&tx.id));
    }

    // Query all the ids we have for rejected, get back `total - MEMPOOL_SIZE`
    let all_ids: HashSet<UnminedTxId> = unmined_transactions.iter().map(|tx| tx.id).collect();

    // Convert response to a `HashSet` as we need a fixed order to compare.
    let rejected_response: HashSet<UnminedTxId> =
        storage.rejected_transactions(all_ids).into_iter().collect();

    let rejected_ids = expected_to_be_rejected.iter().map(|tx| tx.id).collect();

    assert_eq!(rejected_response, rejected_ids);

    // Use `contains_rejected` to make sure the first id stored is now rejected
    assert!(storage.contains_rejected(&expected_to_be_rejected[0].id));
    // Use `contains_rejected` to make sure the last id stored is not rejected
    assert!(!storage.contains_rejected(&expected_in_mempool[0].id));

    Ok(())
}
