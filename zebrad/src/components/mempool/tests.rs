use super::*;
use color_eyre::Report;
use std::{collections::HashSet, sync::Arc};
use storage::tests::unmined_transactions_in_blocks;
use tokio::time;
use tower::{ServiceBuilder, ServiceExt};

use zebra_chain::block::Block;
use zebra_chain::serialization::ZcashDeserializeInto;
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
    let (state, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let state_service = ServiceBuilder::new().buffer(1).service(state);
    let (_chain_verifier, tx_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    // get the genesis block transactions from the Zcash blockchain.
    let mut unmined_transactions = unmined_transactions_in_blocks(..=10, network);
    let genesis_transaction = unmined_transactions
        .next()
        .expect("Missing genesis transaction");
    let txid = unmined_transactions.next_back().unwrap().id;
    let more_transactions = unmined_transactions;

    // Start the mempool service
    let mut service = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set), 1),
        state_service.clone(),
        tx_verifier,
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    // Enable the mempool
    let _ = service.enable(&mut recent_syncs).await;

    // Insert the genesis block coinbase transaction into the mempool storage.
    service.storage().insert(genesis_transaction.clone())?;

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
        .call(Request::TransactionsById(
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
    assert_eq!(genesis_transaction, transactions[0]);

    // Insert more transactions into the mempool storage.
    // This will cause the genesis transaction to be moved into rejected.
    // Skip the last (will be used later)
    for tx in more_transactions {
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
async fn mempool_queue() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let peer_set = MockService::build().for_unit_tests();
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (state, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let state_service = ServiceBuilder::new().buffer(1).service(state);
    let (_chain_verifier, tx_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    // Get transactions to use in the test
    let unmined_transactions = unmined_transactions_in_blocks(..=10, network);
    let mut transactions = unmined_transactions;
    // Split unmined_transactions into:
    // [rejected_tx, transactions..., stored_tx, new_tx]
    //
    // The first transaction to be added in the mempool which will be eventually
    // put in the rejected list
    let rejected_tx = transactions.next().unwrap().clone();
    // A transaction not in the mempool that will be Queued
    let new_tx = transactions.next_back().unwrap();
    // The last transaction that will be added in the mempool (and thus not rejected)
    let stored_tx = transactions.next_back().unwrap().clone();

    // Start the mempool service
    let mut service = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set), 1),
        state_service.clone(),
        tx_verifier,
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    // Enable the mempool
    let _ = service.enable(&mut recent_syncs).await;

    // Insert [rejected_tx, transactions..., stored_tx] into the mempool storage.
    // Insert the genesis block coinbase transaction into the mempool storage.
    service.storage().insert(rejected_tx.clone())?;
    // Insert more transactions into the mempool storage.
    // This will cause the `rejected_tx` to be moved into rejected.
    for tx in transactions {
        service.storage().insert(tx.clone())?;
    }
    service.storage().insert(stored_tx.clone())?;

    // Test `Request::Queue` for a new transaction
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![new_tx.id.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());

    // Test `Request::Queue` for a transaction already in the mempool
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![stored_tx.id.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert_eq!(queued_responses[0], Err(MempoolError::InMempool));

    // Test `Request::Queue` for a transaction rejected by the mempool
    let response = service
        .ready_and()
        .await
        .unwrap()
        .call(Request::Queue(vec![rejected_tx.id.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert_eq!(queued_responses[0], Err(MempoolError::Rejected));

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

    let (state, latest_chain_tip, chain_tip_change) = zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(1).service(state);
    let (_chain_verifier, tx_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    // get the genesis block transactions from the Zcash blockchain.
    let mut unmined_transactions = unmined_transactions_in_blocks(..=10, network);
    let genesis_transaction = unmined_transactions
        .next()
        .expect("Missing genesis transaction");
    let more_transactions = unmined_transactions;

    // Start the mempool service
    let mut service = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set), 1),
        state_service.clone(),
        tx_verifier,
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    // Test if mempool is disabled (it should start disabled)
    assert!(!service.is_enabled());

    // Enable the mempool
    let _ = service.enable(&mut recent_syncs).await;

    assert!(service.is_enabled());

    // Insert the genesis block coinbase transaction into the mempool storage.
    service.storage().insert(genesis_transaction.clone())?;

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

    // Disable the mempool
    let _ = service.disable(&mut recent_syncs).await;

    // Test if mempool is disabled again
    assert!(!service.is_enabled());

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

#[tokio::test]
async fn mempool_cancel_mined() -> Result<(), Report> {
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();

    // Using the mainnet for now
    let network = Network::Mainnet;
    let consensus_config = ConsensusConfig::default();
    let state_config = StateConfig::ephemeral();
    let peer_set = MockService::build().for_unit_tests();
    let (sync_status, mut recent_syncs) = SyncStatus::new();
    let (state, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config.clone(), network);

    let mut state_service = ServiceBuilder::new().buffer(1).service(state);
    let (_chain_verifier, tx_verifier) =
        zebra_consensus::chain::init(consensus_config.clone(), network, state_service.clone())
            .await;

    time::pause();

    // Start the mempool service
    let mut mempool = Mempool::new(
        network,
        Buffer::new(BoxService::new(peer_set), 1),
        state_service.clone(),
        tx_verifier,
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    // Enable the mempool
    let _ = mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Push the genesis block to the state
    let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .ready_and()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitFinalizedBlock(
            genesis_block.clone().into(),
        ))
        .await
        .unwrap();

    // Queue transaction from block 2 for download
    let txid = block2.transactions[0].unmined_id();
    let response = mempool
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
    assert_eq!(mempool.tx_downloads().in_flight(), 1);

    // Query the mempool to make it poll chain_tip_change
    let _response = mempool
        .ready_and()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap();

    // Push block 1 to the state
    state_service
        .ready_and()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitFinalizedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

    // Query the mempool to make it poll chain_tip_change
    let _response = mempool
        .ready_and()
        .await
        .unwrap()
        .call(Request::TransactionIds)
        .await
        .unwrap();

    // Push block 2 to the state
    state_service
        .oneshot(zebra_state::Request::CommitFinalizedBlock(
            block2.clone().into(),
        ))
        .await
        .unwrap();

    // This is done twice because after the first query the cancellation
    // is picked up by select!, and after the second the mempool gets the
    // result and the download future is removed.
    for _ in 0..2 {
        // Query the mempool just to poll it and make it cancel the download.
        let _response = mempool
            .ready_and()
            .await
            .unwrap()
            .call(Request::TransactionIds)
            .await
            .unwrap();
        // Sleep to avoid starvation and make sure the cancellation is picked up.
        time::sleep(time::Duration::from_millis(100)).await;
    }

    // Check if download was cancelled.
    assert_eq!(mempool.tx_downloads().in_flight(), 0);

    Ok(())
}
