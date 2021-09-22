use super::*;
use color_eyre::Report;
use std::collections::HashSet;
use storage::tests::unmined_transactions_in_blocks;
use tower::{ServiceBuilder, ServiceExt};

use zebra_consensus::Config as ConsensusConfig;
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::MockService;

#[tokio::test]
async fn mempool_service_basic() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let peer_set = MockService::build().for_unit_tests();
    let (sync_status, _recent_syncs) = SyncStatus::new();
    let (_state_service, _latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let (state, _, _) = zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(1).service(state);
    let (_chain_verifier, tx_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    // get the genesis block transactions from the Zcash blockchain.
    let genesis_transactions = unmined_transactions_in_blocks(0, network);
    // Start the mempool service
    let mut service = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set), 1),
        state_service.clone(),
        tx_verifier,
        sync_status,
        chain_tip_change,
    );
    // Insert the genesis block coinbase transaction into the mempool storage.
    service.storage.insert(genesis_transactions.1[0].clone())?;

    // Test `Request::TransactionIds`
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap();
    let genesis_transaction_ids = match response {
        Response::TransactionIds(ids) => ids,
        _ => unreachable!("will never happen in this test"),
    };

    // Test `Request::TransactionsById`
    let genesis_transactions_hash_set = genesis_transaction_ids
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let response = service
        .ready_and()
        .await
        .unwrap()
        .oneshot(Request::TransactionsById(
            genesis_transactions_hash_set.clone(),
        ))
        .await
        .unwrap();
    let transactions = match response {
        Response::Transactions(transactions) => transactions,
        _ => unreachable!("will never happen in this test"),
    };

    // Make sure the transaction from the blockchain test vector is the same as the
    // response of `Request::TransactionsById`
    assert_eq!(genesis_transactions.1[0], transactions[0]);

    // Insert more transactions into the mempool storage.
    // This will cause the genesis transaction to be moved into rejected.
    let more_transactions = unmined_transactions_in_blocks(10, network);
    for tx in more_transactions.1.iter().skip(1) {
        service.storage.insert(tx.clone())?;
    }

    // Test `Request::RejectedTransactionIds`
    let response = service
        .oneshot(Request::RejectedTransactionIds(
            genesis_transactions_hash_set,
        ))
        .await
        .unwrap();
    let rejected_ids = match response {
        Response::RejectedTransactionIds(ids) => ids,
        _ => unreachable!("will never happen in this test"),
    };

    assert_eq!(rejected_ids, genesis_transaction_ids);

    Ok(())
}
