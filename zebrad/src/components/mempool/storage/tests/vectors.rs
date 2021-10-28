use std::iter;

use color_eyre::eyre::Result;

use zebra_chain::{
    amount::Amount,
    block::{Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    transaction::{UnminedTxId, VerifiedUnminedTx},
};

use crate::components::mempool::{
    config, storage::tests::unmined_transactions_in_blocks, storage::*, Mempool,
};

/// Eviction memory time used for tests. Most tests won't care about this
/// so we use a large enough value that will never be reached in the tests.
const EVICTION_MEMORY_TIME: Duration = Duration::from_secs(60 * 60);

/// Transaction count used in some tests to derive the mempool test size.
const MEMPOOL_TX_COUNT: usize = 4;

#[test]
fn mempool_storage_crud_exact_mainnet() {
    zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: u64::MAX,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    // Get one (1) unmined transaction
    let unmined_tx = unmined_transactions_in_blocks(.., network)
        .next()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone());

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains_transaction_exact(&unmined_tx.transaction.id));

    // Remove tx
    let removal_count = storage.remove_exact(&iter::once(unmined_tx.transaction.id).collect());

    // Check that it is /not/ in the mempool.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains_transaction_exact(&unmined_tx.transaction.id));
}

#[test]
fn mempool_storage_basic() -> Result<()> {
    zebra_test::init();

    // Test multiple times to catch intermittent bugs since eviction is randomized
    for _ in 0..10 {
        mempool_storage_basic_for_network(Network::Mainnet)?;
        mempool_storage_basic_for_network(Network::Testnet)?;
    }

    Ok(())
}

fn mempool_storage_basic_for_network(network: Network) -> Result<()> {
    // Get transactions from the first 10 blocks of the Zcash blockchain
    let unmined_transactions: Vec<_> = unmined_transactions_in_blocks(..=10, network).collect();

    assert!(
        MEMPOOL_TX_COUNT < unmined_transactions.len(),
        "inconsistent MEMPOOL_TX_COUNT value for this test; decrease it"
    );

    // Use the sum of the costs of the first `MEMPOOL_TX_COUNT` transactions
    // as the cost limit
    let tx_cost_limit = unmined_transactions
        .iter()
        .take(MEMPOOL_TX_COUNT)
        .map(|tx| tx.cost())
        .sum();

    // Create an empty storage
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit,
        ..Default::default()
    });

    // Insert them all to the storage
    let mut maybe_inserted_transactions = Vec::new();
    let mut some_rejected_transactions = Vec::new();
    for unmined_transaction in unmined_transactions.clone() {
        let result = storage.insert(unmined_transaction.clone());
        match result {
            Ok(_) => {
                // While the transaction was inserted here, it can be rejected later.
                maybe_inserted_transactions.push(unmined_transaction);
            }
            Err(_) => {
                // Other transactions can be rejected on a successful insert,
                // so not all rejected transactions will be added.
                // Note that `some_rejected_transactions` can be empty since `insert` only
                // returns a rejection error if the transaction being inserted is the one
                // that was randomly evicted.
                some_rejected_transactions.push(unmined_transaction);
            }
        }
    }
    // Since transactions are rejected randomly we can't test exact numbers.
    // We know the first MEMPOOL_TX_COUNT must have been inserted successfully.
    assert!(maybe_inserted_transactions.len() >= MEMPOOL_TX_COUNT);
    assert_eq!(
        some_rejected_transactions.len() + maybe_inserted_transactions.len(),
        unmined_transactions.len()
    );

    // Test if the actual number of inserted/rejected transactions is consistent.
    assert!(storage.verified.transaction_count() <= maybe_inserted_transactions.len());
    assert!(storage.rejected_transaction_count() >= some_rejected_transactions.len());

    // Test if rejected transactions were actually rejected.
    for tx in some_rejected_transactions.iter() {
        assert!(!storage.contains_transaction_exact(&tx.transaction.id));
    }

    // Query all the ids we have for rejected, get back `total - MEMPOOL_SIZE`
    let all_ids: HashSet<UnminedTxId> = unmined_transactions
        .iter()
        .map(|tx| tx.transaction.id)
        .collect();

    // Convert response to a `HashSet`, because the order of the response doesn't matter.
    let all_rejected_ids: HashSet<UnminedTxId> =
        storage.rejected_transactions(all_ids).into_iter().collect();

    let some_rejected_ids = some_rejected_transactions
        .iter()
        .map(|tx| tx.transaction.id)
        .collect::<HashSet<_>>();

    // Test if the rejected transactions we have are a subset of the actually
    // rejected transactions.
    assert!(some_rejected_ids.is_subset(&all_rejected_ids));

    Ok(())
}

#[test]
fn mempool_storage_crud_same_effects_mainnet() {
    zebra_test::init();

    let network = Network::Mainnet;

    // Create an empty storage instance
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: 160_000_000,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    // Get one (1) unmined transaction
    let unmined_tx = unmined_transactions_in_blocks(.., network)
        .next()
        .expect("at least one unmined transaction");

    // Insert unmined tx into the mempool.
    let _ = storage.insert(unmined_tx.clone());

    // Check that it is in the mempool, and not rejected.
    assert!(storage.contains_transaction_exact(&unmined_tx.transaction.id));

    // Remove tx
    let removal_count =
        storage.remove_same_effects(&iter::once(unmined_tx.transaction.id.mined_id()).collect());

    // Check that it is /not/ in the mempool.
    assert_eq!(removal_count, 1);
    assert!(!storage.contains_transaction_exact(&unmined_tx.transaction.id));
}

#[test]
fn mempool_expired_basic() -> Result<()> {
    zebra_test::init();

    mempool_expired_basic_for_network(Network::Mainnet)?;
    mempool_expired_basic_for_network(Network::Testnet)?;

    Ok(())
}

fn mempool_expired_basic_for_network(network: Network) -> Result<()> {
    // Create an empty storage
    let mut storage: Storage = Storage::new(&config::Config {
        tx_cost_limit: 160_000_000,
        eviction_memory_time: EVICTION_MEMORY_TIME,
        ..Default::default()
    });

    let block: Block = match network {
        Network::Mainnet => {
            zebra_test::vectors::BLOCK_MAINNET_982681_BYTES.zcash_deserialize_into()?
        }
        Network::Testnet => {
            zebra_test::vectors::BLOCK_TESTNET_925483_BYTES.zcash_deserialize_into()?
        }
    };

    // Get a test transaction
    let tx = &*(block.transactions[1]).clone();

    // Change the expiration height of the test transaction
    let mut tx = tx.clone();
    *tx.expiry_height_mut() = zebra_chain::block::Height(1);

    let tx_id = tx.unmined_id();

    // Insert the transaction into the mempool, with a fake zero miner fee
    storage.insert(VerifiedUnminedTx::new(tx.into(), Amount::zero()))?;

    assert_eq!(storage.transaction_count(), 1);

    // Get everything available in mempool now
    let everything_in_mempool: HashSet<UnminedTxId> = storage.tx_ids().collect();
    assert_eq!(everything_in_mempool.len(), 1);
    assert!(everything_in_mempool.contains(&tx_id));

    // remove_expired_transactions() will return what was removed
    let expired = storage.remove_expired_transactions(Height(1));
    assert!(expired.contains(&tx_id));
    let everything_in_mempool: HashSet<UnminedTxId> = storage.tx_ids().collect();
    assert_eq!(everything_in_mempool.len(), 0);

    // No transaction will be sent to peers
    let send_to_peers = Mempool::remove_expired_from_peer_list(&everything_in_mempool, &expired);
    assert_eq!(send_to_peers.len(), 0);

    Ok(())
}
