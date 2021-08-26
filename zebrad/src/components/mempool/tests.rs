use super::*;
use color_eyre::Report;
use std::collections::HashSet;
use storage::tests::unmined_transactions_in_blocks;
use tower::ServiceExt;

#[tokio::test]
async fn mempool_service_basic() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    // get the transactions in block one from the Zcash blockchain.
    let block_one_transactions = unmined_transactions_in_blocks(1, network);

    // Start the mempool service
    let mut service = Mempool::new(network);

    // Insert the first transaction from block one into the mempool storage.
    service
        .storage
        .insert(block_one_transactions.1[0].clone())?;

    // Test `Request::TransactionIds`
    let response = service
        .clone()
        .oneshot(Request::TransactionIds)
        .await
        .unwrap();
    let transaction_ids = match response {
        Response::TransactionIds(ids) => ids,
        _ => unreachable!("will never happen in this test"),
    };

    // Test `Request::TransactionsById`
    let hash_set = transaction_ids.iter().copied().collect::<HashSet<_>>();
    let response = service
        .oneshot(Request::TransactionsById(hash_set))
        .await
        .unwrap();
    let transactions = match response {
        Response::Transactions(transactions) => transactions,
        _ => unreachable!("will never happen in this test"),
    };

    // Make sure the transaction from the blockchain test vector is the same as the
    // response of `Request::TransactionsById`
    assert_eq!(block_one_transactions.1[0], transactions[0]);

    Ok(())
}
