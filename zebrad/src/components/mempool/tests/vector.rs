//! Fixed test vectors for the mempool.

use std::{sync::Arc, time::Duration};

use color_eyre::Report;
use tokio::time::{self, timeout};
use tower::{ServiceBuilder, ServiceExt};

use zebra_chain::{
    amount::Amount, block::Block, fmt::humantime_seconds, parameters::Network,
    serialization::ZcashDeserializeInto, transaction::VerifiedUnminedTx, transparent::OutPoint,
};
use zebra_consensus::transaction as tx;
use zebra_state::{Config as StateConfig, CHAIN_TIP_UPDATE_WAIT_LIMIT};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::components::{
    mempool::{self, *},
    sync::RecentSyncLengths,
};

/// A [`MockService`] representing the network service.
type MockPeerSet = MockService<zn::Request, zn::Response, PanicAssertion>;

/// The unmocked Zebra state service's type.
type StateService = Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>;

/// A [`MockService`] representing the Zebra transaction verifier service.
type MockTxVerifier = MockService<tx::Request, tx::Response, PanicAssertion, TransactionError>;

#[tokio::test]
async fn mempool_service_basic() -> Result<(), Report> {
    // Test multiple times to catch intermittent bugs since eviction is randomized
    for _ in 0..10 {
        mempool_service_basic_single().await?;
    }
    Ok(())
}

async fn mempool_service_basic_single() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    // get the genesis block transactions from the Zcash blockchain.
    let mut unmined_transactions = network.unmined_transactions_in_blocks(1..=10);
    let genesis_transaction = unmined_transactions
        .next()
        .expect("Missing genesis transaction");
    let last_transaction = unmined_transactions.next_back().unwrap();
    let more_transactions = unmined_transactions.collect::<Vec<_>>();

    // Use as cost limit the costs of all transactions that will be
    // inserted except one (the genesis block transaction).
    let cost_limit = more_transactions.iter().map(|tx| tx.cost()).sum();

    let (
        mut service,
        _peer_set,
        _state_service,
        _chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        _mempool_transaction_receiver,
    ) = setup(&network, cost_limit, true).await;

    // Enable the mempool
    service.enable(&mut recent_syncs).await;

    // Insert the genesis block coinbase transaction into the mempool storage.
    let mut inserted_ids = HashSet::new();
    service
        .storage()
        .insert(genesis_transaction.clone(), Vec::new(), None)?;
    inserted_ids.insert(genesis_transaction.transaction.id);

    // Test `Request::TransactionIds`
    let response = service
        .ready()
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
        .ready()
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
    assert_eq!(genesis_transaction.transaction, transactions[0]);

    // Test `Request::TransactionsByMinedId`
    // TODO: use a V5 tx to test if it's really matched by mined ID
    let genesis_transactions_mined_hash_set = genesis_transaction_ids
        .iter()
        .map(|txid| txid.mined_id())
        .collect::<HashSet<_>>();
    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::TransactionsByMinedId(
            genesis_transactions_mined_hash_set,
        ))
        .await
        .unwrap();
    let transactions = match response {
        Response::Transactions(transactions) => transactions,
        _ => unreachable!("will never happen in this test"),
    };

    // Make sure the transaction from the blockchain test vector is the same as the
    // response of `Request::TransactionsByMinedId`
    assert_eq!(genesis_transaction.transaction, transactions[0]);

    // Insert more transactions into the mempool storage.
    // This will cause the genesis transaction to be moved into rejected.
    // Skip the last (will be used later)
    for tx in more_transactions {
        inserted_ids.insert(tx.transaction.id);
        // Error must be ignored because a insert can trigger an eviction and
        // an error is returned if the transaction being inserted in chosen.
        let _ = service.storage().insert(tx.clone(), Vec::new(), None);
    }

    // Test `Request::RejectedTransactionIds`
    let response = service
        .ready()
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

    assert!(rejected_ids.is_subset(&inserted_ids));

    // Test `Request::Queue`
    // Use the ID of the last transaction in the list
    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![last_transaction.transaction.id.into()]))
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
    // Test multiple times to catch intermittent bugs since eviction is randomized
    for _ in 0..10 {
        mempool_queue_single().await?;
    }
    Ok(())
}

async fn mempool_queue_single() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    // Get transactions to use in the test
    let unmined_transactions = network.unmined_transactions_in_blocks(1..=10);
    let mut transactions = unmined_transactions.collect::<Vec<_>>();
    // Split unmined_transactions into:
    // [transactions..., new_tx]
    // A transaction not in the mempool that will be Queued
    let new_tx = transactions.pop().unwrap();

    // Use as cost limit the costs of all transactions that will be
    // inserted except the last.
    let cost_limit = transactions
        .iter()
        .take(transactions.len() - 1)
        .map(|tx| tx.cost())
        .sum();

    let (
        mut service,
        _peer_set,
        _state_service,
        _chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        _mempool_transaction_receiver,
    ) = setup(&network, cost_limit, true).await;

    // Enable the mempool
    service.enable(&mut recent_syncs).await;

    // Insert [transactions...] into the mempool storage.
    // This will cause the at least one transaction to be rejected, since
    // the cost limit is the sum of all costs except of the last transaction.
    for tx in transactions.iter() {
        // Error must be ignored because a insert can trigger an eviction and
        // an error is returned if the transaction being inserted in chosen.
        let _ = service.storage().insert(tx.clone(), Vec::new(), None);
    }

    // Test `Request::Queue` for a new transaction
    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![new_tx.transaction.id.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());

    // Test `Request::Queue` with all previously inserted transactions.
    // They should all be rejected; either because they are already in the mempool,
    // or because they are in the recently evicted list.
    let response = service
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(
            transactions
                .iter()
                .map(|tx| tx.transaction.id.into())
                .collect(),
        ))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), transactions.len());

    // Check if the responses are consistent
    let mut in_mempool_count = 0;
    let mut evicted_count = 0;
    for response in queued_responses {
        match response.unbox_mempool_error() {
            MempoolError::StorageEffectsChain(SameEffectsChainRejectionError::RandomlyEvicted) => {
                evicted_count += 1
            }
            MempoolError::InMempool => in_mempool_count += 1,
            error => panic!("transaction should not be rejected with reason {error:?}"),
        }
    }
    assert_eq!(in_mempool_count, transactions.len() - 1);
    assert_eq!(evicted_count, 1);

    Ok(())
}

#[tokio::test]
async fn mempool_service_disabled() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    let (
        mut service,
        _peer_set,
        _state_service,
        _chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        _mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // get the genesis block transactions from the Zcash blockchain.
    let mut unmined_transactions = network.unmined_transactions_in_blocks(1..=10);
    let genesis_transaction = unmined_transactions
        .next()
        .expect("Missing genesis transaction");
    let more_transactions = unmined_transactions;

    // Test if mempool is disabled (it should start disabled)
    assert!(!service.is_enabled());

    // Enable the mempool
    service.enable(&mut recent_syncs).await;

    assert!(service.is_enabled());

    // Insert the genesis block coinbase transaction into the mempool storage.
    service
        .storage()
        .insert(genesis_transaction.clone(), Vec::new(), None)?;

    // Test if the mempool answers correctly (i.e. is enabled)
    let response = service
        .ready()
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
    let txid = more_transactions.last().unwrap().transaction.id;
    let response = service
        .ready()
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
    service.disable(&mut recent_syncs).await;

    // Test if mempool is disabled again
    assert!(!service.is_enabled());

    // Test if the mempool returns no transactions when disabled
    let response = service
        .ready()
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
        .ready()
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
    assert_eq!(
        queued_responses
            .into_iter()
            .next()
            .unwrap()
            .unbox_mempool_error(),
        MempoolError::Disabled
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mempool_cancel_mined() -> Result<(), Report> {
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();

    // Using the mainnet for now
    let network = Network::Mainnet;

    let (
        mut mempool,
        _peer_set,
        mut state_service,
        mut chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        mut mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Query the mempool to make it poll chain_tip_change
    mempool.dummy_call().await;

    // Push block 1 to the state
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

    // Wait for the chain tip update
    if let Err(timeout_error) = timeout(
        CHAIN_TIP_UPDATE_WAIT_LIMIT,
        chain_tip_change.wait_for_tip_change(),
    )
    .await
    .map(|change_result| change_result.expect("unexpected chain tip update failure"))
    {
        info!(
            timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
            ?timeout_error,
            "timeout waiting for chain tip change after committing block"
        );
    }

    // Query the mempool to make it poll chain_tip_change
    mempool.dummy_call().await;

    // Queue transaction from block 2 for download.
    // It can't be queued before because block 1 triggers a network upgrade,
    // which cancels all downloads.
    let txid = block2.transactions[0].unmined_id();
    let response = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![txid.into()]))
        .await
        .unwrap();
    let mut queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);

    let queued_response = queued_responses
        .pop()
        .expect("already checked that there is exactly 1 item in Vec")
        .expect("initial queue checks result should be Ok");

    assert_eq!(mempool.tx_downloads().in_flight(), 1);

    // Push block 2 to the state
    state_service
        .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block2.clone().into(),
        ))
        .await
        .unwrap();

    // Wait for the chain tip update
    if let Err(timeout_error) = timeout(
        CHAIN_TIP_UPDATE_WAIT_LIMIT,
        chain_tip_change.wait_for_tip_change(),
    )
    .await
    .map(|change_result| change_result.expect("unexpected chain tip update failure"))
    {
        info!(
            timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
            ?timeout_error,
            "timeout waiting for chain tip change after committing block"
        );
    }

    // This is done twice because after the first query the cancellation
    // is picked up by select!, and after the second the mempool gets the
    // result and the download future is removed.
    for _ in 0..2 {
        // Query the mempool just to poll it and make it cancel the download.
        mempool.dummy_call().await;
        // Sleep to avoid starvation and make sure the cancellation is picked up.
        time::sleep(time::Duration::from_millis(100)).await;
    }

    // Check if download was cancelled.
    assert_eq!(mempool.tx_downloads().in_flight(), 0);

    assert!(
        queued_response
            .await
            .expect("channel should not be closed")
            .is_err(),
        "queued tx should fail to download and verify due to chain tip change"
    );

    let mempool_change = timeout(Duration::from_secs(3), mempool_transaction_receiver.recv())
        .await
        .expect("should not timeout")
        .expect("recv should return Ok");

    assert_eq!(
        mempool_change,
        MempoolChange::invalidated([txid].into_iter().collect())
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn mempool_cancel_downloads_after_network_upgrade() -> Result<(), Report> {
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();

    // Using the mainnet for now
    let network = Network::Mainnet;

    let (
        mut mempool,
        mut peer_set,
        mut state_service,
        mut chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        _mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Queue transaction from block 2 for download
    let txid = block2.transactions[0].unmined_id();
    let response = mempool
        .ready()
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
    mempool.dummy_call().await;

    // Push block 1 to the state. This is considered a network upgrade,
    // and thus must cancel all pending transaction downloads.
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

    // Wait for the chain tip update
    if let Err(timeout_error) = timeout(
        CHAIN_TIP_UPDATE_WAIT_LIMIT,
        chain_tip_change.wait_for_tip_change(),
    )
    .await
    .map(|change_result| change_result.expect("unexpected chain tip update failure"))
    {
        info!(
            timeout = ?humantime_seconds(CHAIN_TIP_UPDATE_WAIT_LIMIT),
            ?timeout_error,
            "timeout waiting for chain tip change after committing block"
        );
    }

    // Ignore all the previous network requests.
    while let Some(_request) = peer_set.try_next_request().await {}

    // Query the mempool to make it poll chain_tip_change
    mempool.dummy_call().await;

    // Check if download was cancelled and transaction was retried.
    let request = peer_set
        .try_next_request()
        .await
        .expect("unexpected missing mempool retry");

    assert_eq!(
        request.request(),
        &zebra_network::Request::TransactionsById(iter::once(txid).collect()),
    );
    assert_eq!(mempool.tx_downloads().in_flight(), 1);

    Ok(())
}

/// Check if a transaction that fails verification is rejected by the mempool.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_failed_verification_is_rejected() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    let (
        mut mempool,
        _peer_set,
        _state_service,
        _chain_tip_change,
        mut tx_verifier,
        mut recent_syncs,
        mut mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // Get transactions to use in the test
    let mut unmined_transactions = network.unmined_transactions_in_blocks(1..=2);
    let rejected_tx = unmined_transactions.next().unwrap().clone();

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;

    // Queue first transaction for verification
    // (queue the transaction itself to avoid a download).
    let request = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![rejected_tx.transaction.clone().into()]));
    // Make the mock verifier return that the transaction is invalid.
    let verification = tx_verifier.expect_request_that(|_| true).map(|responder| {
        responder.respond(Err(TransactionError::BadBalance));
    });
    let (response, _) = futures::join!(request, verification);
    let queued_responses = match response.unwrap() {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    // Check that the request was enqueued successfully.
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());

    for _ in 0..2 {
        // Query the mempool just to poll it and make get the downloader/verifier result.
        mempool.dummy_call().await;
        // Sleep to avoid starvation and make sure the verification failure is picked up.
        time::sleep(time::Duration::from_millis(100)).await;
    }

    // Try to queue the same transaction by its ID and check if it's correctly
    // rejected.
    let response = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![rejected_tx.transaction.id.into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(matches!(
        queued_responses
            .into_iter()
            .next()
            .unwrap()
            .unbox_mempool_error(),
        MempoolError::StorageExactTip(ExactTipRejectionError::FailedVerification(_))
    ));

    let mempool_change = timeout(Duration::from_secs(3), mempool_transaction_receiver.recv())
        .await
        .expect("should not timeout")
        .expect("recv should return Ok");

    assert_eq!(
        mempool_change,
        MempoolChange::invalidated([rejected_tx.transaction.id].into_iter().collect())
    );

    Ok(())
}

/// Check if a transaction that fails download is _not_ rejected.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_failed_download_is_not_rejected() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    let (
        mut mempool,
        mut peer_set,
        _state_service,
        _chain_tip_change,
        _tx_verifier,
        mut recent_syncs,
        mut mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // Get transactions to use in the test
    let mut unmined_transactions = network.unmined_transactions_in_blocks(1..=2);
    let rejected_valid_tx = unmined_transactions.next().unwrap().clone();

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;

    // Queue second transaction for download and verification.
    let request = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![rejected_valid_tx
            .transaction
            .id
            .into()]));
    // Make the mock peer set return that the download failed.
    let verification = peer_set
        .expect_request_that(|r| matches!(r, zn::Request::TransactionsById(_)))
        .map(|responder| {
            responder.respond(zn::Response::Transactions(vec![]));
        });
    let (response, _) = futures::join!(request, verification);
    let queued_responses = match response.unwrap() {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    // Check that the request was enqueued successfully.
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());

    for _ in 0..2 {
        // Query the mempool just to poll it and make get the downloader/verifier result.
        mempool.dummy_call().await;
        // Sleep to avoid starvation and make sure the download failure is picked up.
        time::sleep(time::Duration::from_millis(100)).await;
    }

    // Try to queue the same transaction by its ID and check if it's not being
    // rejected.
    let response = mempool
        .ready()
        .await
        .unwrap()
        .call(Request::Queue(vec![rejected_valid_tx
            .transaction
            .id
            .into()]))
        .await
        .unwrap();
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());

    let mempool_change = timeout(Duration::from_secs(3), mempool_transaction_receiver.recv())
        .await
        .expect("should not timeout")
        .expect("recv should return Ok");

    assert_eq!(
        mempool_change,
        MempoolChange::invalidated([rejected_valid_tx.transaction.id].into_iter().collect())
    );

    Ok(())
}

/// Check that transactions are re-verified if the tip changes
/// during verification.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_reverifies_after_tip_change() -> Result<(), Report> {
    let network = Network::Mainnet;

    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let block3: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into()
        .unwrap();

    let (
        mut mempool,
        mut peer_set,
        mut state_service,
        mut chain_tip_change,
        mut tx_verifier,
        mut recent_syncs,
        _mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Queue transaction from block 3 for download
    let tx = block3.transactions[0].clone();
    let txid = block3.transactions[0].unmined_id();
    let response = mempool
        .ready()
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

    // Verify the transaction

    peer_set
        .expect_request_that(|req| matches!(req, zn::Request::TransactionsById(_)))
        .map(|responder| {
            responder.respond(zn::Response::Transactions(vec![
                zn::InventoryResponse::Available((tx.clone().into(), None)),
            ]));
        })
        .await;

    tx_verifier
        .expect_request_that(|_| true)
        .map(|responder| {
            let transaction = responder
                .request()
                .clone()
                .mempool_transaction()
                .expect("unexpected non-mempool request");

            // Set a dummy fee and sigops.
            responder.respond(transaction::Response::from(
                VerifiedUnminedTx::new(
                    transaction,
                    Amount::try_from(1_000_000).expect("invalid value"),
                    0,
                )
                .expect("verification should pass"),
            ));
        })
        .await;

    // Push block 1 to the state. This is considered a network upgrade,
    // and must cancel all pending transaction downloads with a `TipAction::Reset`.
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

    // Wait for the chain tip update without a timeout
    // (skipping the chain tip change here will fail the test)
    chain_tip_change
        .wait_for_tip_change()
        .await
        .expect("unexpected chain tip update failure");

    // Query the mempool to make it poll chain_tip_change and try reverifying its state for the `TipAction::Reset`
    mempool.dummy_call().await;

    // Check that there is still an in-flight tx_download and that
    // no transactions were inserted in the mempool.
    assert_eq!(mempool.tx_downloads().in_flight(), 1);
    assert_eq!(mempool.storage().transaction_count(), 0);

    // Verify the transaction again

    peer_set
        .expect_request_that(|req| matches!(req, zn::Request::TransactionsById(_)))
        .map(|responder| {
            responder.respond(zn::Response::Transactions(vec![
                zn::InventoryResponse::Available((tx.into(), None)),
            ]));
        })
        .await;

    // Verify the transaction now that the mempool has already checked chain_tip_change
    tx_verifier
        .expect_request_that(|_| true)
        .map(|responder| {
            let transaction = responder
                .request()
                .clone()
                .mempool_transaction()
                .expect("unexpected non-mempool request");

            // Set a dummy fee and sigops.
            responder.respond(transaction::Response::from(
                VerifiedUnminedTx::new(
                    transaction,
                    Amount::try_from(1_000_000).expect("invalid value"),
                    0,
                )
                .expect("verification should pass"),
            ));
        })
        .await;

    // Push block 2 to the state. This will increase the tip height past the expected
    // tip height that the tx was verified at.
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
            block2.clone().into(),
        ))
        .await
        .unwrap();

    // Wait for the chain tip update without a timeout
    // (skipping the chain tip change here will fail the test)
    chain_tip_change
        .wait_for_tip_change()
        .await
        .expect("unexpected chain tip update failure");

    // Query the mempool to make it poll tx_downloads.pending and try reverifying transactions
    // because the tip height has changed.
    mempool.dummy_call().await;

    // Check that there is still an in-flight tx_download and that
    // no transactions were inserted in the mempool.
    assert_eq!(mempool.tx_downloads().in_flight(), 1);
    assert_eq!(mempool.storage().transaction_count(), 0);

    Ok(())
}

/// Checks that the mempool service responds to AwaitOutput requests after verifying transactions
/// that create those outputs, or immediately if the outputs had been created by transaction that
/// are already in the mempool.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_responds_to_await_output() -> Result<(), Report> {
    let network = Network::Mainnet;

    let (
        mut mempool,
        _peer_set,
        _state_service,
        _chain_tip_change,
        mut tx_verifier,
        mut recent_syncs,
        mut mempool_transaction_receiver,
    ) = setup(&network, u64::MAX, true).await;
    mempool.enable(&mut recent_syncs).await;

    let verified_unmined_tx = network
        .unmined_transactions_in_blocks(1..=10)
        .find(|tx| !tx.transaction.transaction.outputs().is_empty())
        .expect("should have at least 1 tx with transparent outputs");

    let unmined_tx = verified_unmined_tx.transaction.clone();
    let unmined_tx_id = unmined_tx.id;
    let output_index = 0;
    let outpoint = OutPoint::from_usize(unmined_tx.id.mined_id(), output_index);
    let expected_output = unmined_tx
        .transaction
        .outputs()
        .get(output_index)
        .expect("already checked that tx has outputs")
        .clone();

    // Call mempool with an AwaitOutput request

    let request = Request::AwaitOutput(outpoint);
    let await_output_response_fut = mempool.ready().await.unwrap().call(request);

    // Queue the transaction with the pending output to be added to the mempool

    let request = Request::Queue(vec![Gossip::Tx(unmined_tx)]);
    let queue_response_fut = mempool.ready().await.unwrap().call(request);
    let mock_verify_tx_fut = tx_verifier.expect_request_that(|_| true).map(|responder| {
        responder.respond(transaction::Response::Mempool {
            transaction: verified_unmined_tx,
            spent_mempool_outpoints: Vec::new(),
        });
    });

    let (response, _) = futures::join!(queue_response_fut, mock_verify_tx_fut);
    let Response::Queued(mut results) = response.expect("response should be Ok") else {
        panic!("wrong response from mempool to Queued request");
    };

    let result_rx = results.remove(0).expect("should pass initial checks");
    assert!(results.is_empty(), "should have 1 result for 1 queued tx");

    // Wait for post-verification steps in mempool's Downloads
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Note: Buffered services shouldn't be polled without being called.
    //       See `mempool::Request::CheckForVerifiedTransactions` for more details.
    mempool
        .ready()
        .await
        .expect("polling mempool should succeed");

    tokio::time::timeout(Duration::from_secs(10), result_rx)
        .await
        .expect("should not time out")
        .expect("mempool tx verification result channel should not be closed")
        .expect("mocked verification should be successful");

    assert_eq!(
        mempool.storage().transaction_count(),
        1,
        "should have 1 transaction in mempool's verified set"
    );

    assert_eq!(
        mempool.storage().created_output(&outpoint),
        Some(expected_output.clone()),
        "created output should match expected output"
    );

    // Check that the AwaitOutput request has been responded to after the relevant tx was added to the verified set

    let response_fut = tokio::time::timeout(Duration::from_secs(30), await_output_response_fut);
    let response = response_fut
        .await
        .expect("should not time out")
        .expect("should not return RecvError");

    let Response::UnspentOutput(response) = response else {
        panic!("wrong response from mempool to AwaitOutput request");
    };

    assert_eq!(
        response, expected_output,
        "AwaitOutput response should match expected output"
    );

    // Check that the mempool responds to AwaitOutput requests correctly when the outpoint is already in its `created_outputs` collection too.

    let request = Request::AwaitOutput(outpoint);
    let await_output_response_fut = mempool.ready().await.unwrap().call(request);
    let response_fut = tokio::time::timeout(Duration::from_secs(30), await_output_response_fut);
    let response = response_fut
        .await
        .expect("should not time out")
        .expect("should not return RecvError");

    let Response::UnspentOutput(response) = response else {
        panic!("wrong response from mempool to AwaitOutput request");
    };

    assert_eq!(
        response, expected_output,
        "AwaitOutput response should match expected output"
    );

    let mempool_change = timeout(Duration::from_secs(3), mempool_transaction_receiver.recv())
        .await
        .expect("should not timeout")
        .expect("recv should return Ok");

    assert_eq!(
        mempool_change,
        MempoolChange::added([unmined_tx_id].into_iter().collect())
    );

    Ok(())
}

/// Create a new [`Mempool`] instance using mocked services.
async fn setup(
    network: &Network,
    tx_cost_limit: u64,
    should_commit_genesis_block: bool,
) -> (
    Mempool,
    MockPeerSet,
    StateService,
    ChainTipChange,
    MockTxVerifier,
    RecentSyncLengths,
    tokio::sync::broadcast::Receiver<MempoolChange>,
) {
    let peer_set = MockService::build().for_unit_tests();

    // UTXO verification doesn't matter here.
    let state_config = StateConfig::ephemeral();
    let (state, _read_only_state_service, latest_chain_tip, mut chain_tip_change) =
        zebra_state::init(state_config, network, Height::MAX, 0).await;
    let mut state_service = ServiceBuilder::new().buffer(10).service(state);

    let tx_verifier = MockService::build().for_unit_tests();

    let (sync_status, recent_syncs) = SyncStatus::new();
    let (misbehavior_tx, _misbehavior_rx) = tokio::sync::mpsc::channel(1);
    let (mempool, mempool_transaction_subscriber) = Mempool::new(
        &mempool::Config {
            tx_cost_limit,
            ..Default::default()
        },
        Buffer::new(BoxService::new(peer_set.clone()), 1),
        state_service.clone(),
        Buffer::new(BoxService::new(tx_verifier.clone()), 1),
        sync_status,
        latest_chain_tip,
        chain_tip_change.clone(),
        misbehavior_tx,
    );

    let mut mempool_transaction_receiver = mempool_transaction_subscriber.subscribe();
    tokio::spawn(async move { while mempool_transaction_receiver.recv().await.is_ok() {} });

    if should_commit_genesis_block {
        let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into()
            .unwrap();

        // Push the genesis block to the state
        state_service
            .ready()
            .await
            .unwrap()
            .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
                genesis_block.clone().into(),
            ))
            .await
            .unwrap();

        // Wait for the chain tip update without a timeout
        chain_tip_change
            .wait_for_tip_change()
            .await
            .expect("unexpected chain tip update failure");
    }

    (
        mempool,
        peer_set,
        state_service,
        chain_tip_change,
        tx_verifier,
        recent_syncs,
        mempool_transaction_subscriber.subscribe(),
    )
}
