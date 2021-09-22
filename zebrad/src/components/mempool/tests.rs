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
    let (sync_status, mut recent_syncs) = SyncStatus::new();

    let (state, _latest_chain_tip, chain_tip_change) = zebra_state::init(state_config, network);
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

    // Pretend we're close to tip to enable the mempool
    SyncStatus::sync_close_to_tip(&mut recent_syncs);
    // Wait for the mempool to make it enable itself
    let _ = service.ready_and().await;

    // Insert the genesis block coinbase transaction into the mempool storage.
    service
        .storage()
        .insert(genesis_transactions.1[0].clone())?;

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
    let (count, more_transactions) = unmined_transactions_in_blocks(10, network);
    // Skip the first (used before) and the last (will be used later)
    for tx in more_transactions.iter().skip(1).take(count - 2) {
        service.storage().insert(tx.clone())?;
    }

    // Test `Request::RejectedTransactionIds`
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::RejectedTransactionIds(
            genesis_transactions_hash_set,
        ))
        .await
        .unwrap();
    let rejected_ids = match response {
        Response::RejectedTransactionIds(ids) => ids,
        _ => unreachable!("will never happen in this test"),
    };

    assert_eq!(rejected_ids, genesis_transaction_ids);

    // Test `Request::Queue`
    // Use the ID of the last transaction in the list
    let txid = more_transactions.last().unwrap().id;
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![txid.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());
    assert_eq!(service.tx_downloads().in_flight(), 1);

    Ok(())
}

#[tokio::test]
async fn mempool_service_disabled() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let peer_set = MockService::build().for_unit_tests();
    let (sync_status, mut recent_syncs) = SyncStatus::new();

    let (state, _latest_chain_tip, chain_tip_change) = zebra_state::init(state_config, network);
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

    // Test if mempool is disabled (it should start disabled)
    assert!(!service.enabled());

    // Pretend we're close to tip to enable the mempool
    SyncStatus::sync_close_to_tip(&mut recent_syncs);
    // Wait for the mempool to make it enable itself
    let _ = service.ready_and().await;

    assert!(service.enabled());

    // Insert the genesis block coinbase transaction into the mempool storage.
    service
        .storage()
        .insert(genesis_transactions.1[0].clone())?;

    // Test if the mempool answers correctly (i.e. is enabled)
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap();
    let _genesis_transaction_ids = match response {
        Response::TransactionIds(ids) => ids,
        _ => unreachable!("will never happen in this test"),
    };

    let (_count, more_transactions) = unmined_transactions_in_blocks(1, network);

    // Queue a transaction for download
    // Use the ID of the last transaction in the list
    let txid = more_transactions.last().unwrap().id;
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![txid.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());
    assert_eq!(service.tx_downloads().in_flight(), 1);

    // Pretend we're far from the tip to disable the mempool
    SyncStatus::sync_far_from_tip(&mut recent_syncs);
    // Wait for the mempool to make it disable itself
    let _ = service.ready_and().await;

    // Test if mempool is disabled again
    assert!(!service.enabled());

    // Test if the mempool returns no transactions when disabled
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap();
    match response {
        Response::TransactionIds(ids) => {
            assert_eq!(
                ids.len(),
                0,
                "mempool should return no transactions when disabled"
            )
        }
        _ => unreachable!("will never happen in this test"),
    };

    // Test if the mempool returns to Queue requests correctly when disabled
    let txid = more_transactions.last().unwrap().id;
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![txid.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert_eq!(queued_responses[0], Err(MempoolError::Disabled));

    Ok(())
}
