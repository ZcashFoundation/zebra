//! Fixed test vectors for the mempool.

use std::{collections::HashSet, sync::Arc};

use color_eyre::Report;
use tokio::time;
use tower::{ServiceBuilder, ServiceExt};

use zebra_chain::{block::Block, parameters::Network, serialization::ZcashDeserializeInto};
use zebra_consensus::transaction as tx;
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::UnboxMempoolError;
use crate::components::{
    mempool::{self, storage::tests::unmined_transactions_in_blocks, *},
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
    let mut unmined_transactions = unmined_transactions_in_blocks(..=10, network);
    let genesis_transaction = unmined_transactions
        .next()
        .expect("Missing genesis transaction");
    let last_transaction = unmined_transactions.next_back().unwrap();
    let more_transactions = unmined_transactions.collect::<Vec<_>>();

    // Use as cost limit the costs of all transactions that will be
    // inserted except one (the genesis block transaction).
    let cost_limit = more_transactions.iter().map(|tx| tx.cost()).sum();

    let (mut service, _peer_set, _state_service, _tx_verifier, mut recent_syncs) =
        setup(network, cost_limit).await;

    // Enable the mempool
    service.enable(&mut recent_syncs).await;

    // Insert the genesis block coinbase transaction into the mempool storage.
    let mut inserted_ids = HashSet::new();
    service.storage().insert(genesis_transaction.clone())?;
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
        let _ = service.storage().insert(tx.clone());
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
    let unmined_transactions = unmined_transactions_in_blocks(..=10, network);
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

    let (mut service, _peer_set, _state_service, _tx_verifier, mut recent_syncs) =
        setup(network, cost_limit).await;

    // Enable the mempool
    service.enable(&mut recent_syncs).await;

    // Insert [transactions...] into the mempool storage.
    // This will cause the at least one transaction to be rejected, since
    // the cost limit is the sum of all costs except of the last transaction.
    for tx in transactions.iter() {
        // Error must be ignored because a insert can trigger an eviction and
        // an error is returned if the transaction being inserted in chosen.
        let _ = service.storage().insert(tx.clone());
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
            error => panic!("transaction should not be rejected with reason {:?}", error),
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

    let (mut service, _peer_set, _state_service, _tx_verifier, mut recent_syncs) =
        setup(network, u64::MAX).await;

    // get the genesis block transactions from the Zcash blockchain.
    let mut unmined_transactions = unmined_transactions_in_blocks(..=10, network);
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
    service.storage().insert(genesis_transaction.clone())?;

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

    let (mut mempool, _peer_set, mut state_service, _tx_verifier, mut recent_syncs) =
        setup(network, u64::MAX).await;

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Push the genesis block to the state
    let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitFinalizedBlock(
            genesis_block.clone().into(),
        ))
        .await
        .unwrap();

    // Query the mempool to make it poll chain_tip_change
    mempool.dummy_call().await;

    // Push block 1 to the state
    state_service
        .ready()
        .await
        .unwrap()
        .call(zebra_state::Request::CommitFinalizedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

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
    let queued_responses = match response {
        Response::Queued(queue_responses) => queue_responses,
        _ => unreachable!("will never happen in this test"),
    };
    assert_eq!(queued_responses.len(), 1);
    assert!(queued_responses[0].is_ok());
    assert_eq!(mempool.tx_downloads().in_flight(), 1);

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
        mempool.dummy_call().await;
        // Sleep to avoid starvation and make sure the cancellation is picked up.
        time::sleep(time::Duration::from_millis(100)).await;
    }

    // Check if download was cancelled.
    assert_eq!(mempool.tx_downloads().in_flight(), 0);

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

    let (mut mempool, _peer_set, mut state_service, _tx_verifier, mut recent_syncs) =
        setup(network, u64::MAX).await;

    // Enable the mempool
    mempool.enable(&mut recent_syncs).await;
    assert!(mempool.is_enabled());

    // Push the genesis block to the state
    let genesis_block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    state_service
        .ready()
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
        .call(zebra_state::Request::CommitFinalizedBlock(
            block1.clone().into(),
        ))
        .await
        .unwrap();

    // Query the mempool to make it poll chain_tip_change
    mempool.dummy_call().await;

    // Check if download was cancelled.
    assert_eq!(mempool.tx_downloads().in_flight(), 0);

    Ok(())
}

/// Check if a transaction that fails verification is rejected by the mempool.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_failed_verification_is_rejected() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    let (mut mempool, _peer_set, _state_service, mut tx_verifier, mut recent_syncs) =
        setup(network, u64::MAX).await;

    // Get transactions to use in the test
    let mut unmined_transactions = unmined_transactions_in_blocks(1..=2, network);
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

    Ok(())
}

/// Check if a transaction that fails download is _not_ rejected.
#[tokio::test(flavor = "multi_thread")]
async fn mempool_failed_download_is_not_rejected() -> Result<(), Report> {
    // Using the mainnet for now
    let network = Network::Mainnet;

    let (mut mempool, mut peer_set, _state_service, _tx_verifier, mut recent_syncs) =
        setup(network, u64::MAX).await;

    // Get transactions to use in the test
    let mut unmined_transactions = unmined_transactions_in_blocks(1..=2, network);
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

    Ok(())
}

/// Create a new [`Mempool`] instance using mocked services.
async fn setup(
    network: Network,
    tx_cost_limit: u64,
) -> (
    Mempool,
    MockPeerSet,
    StateService,
    MockTxVerifier,
    RecentSyncLengths,
) {
    let peer_set = MockService::build().for_unit_tests();

    let state_config = StateConfig::ephemeral();
    let (state, _read_only_state_service, latest_chain_tip, chain_tip_change) =
        zebra_state::init(state_config, network);
    let state_service = ServiceBuilder::new().buffer(1).service(state);

    let tx_verifier = MockService::build().for_unit_tests();

    let (sync_status, recent_syncs) = SyncStatus::new();

    let (mempool, _mempool_transaction_receiver) = Mempool::new(
        &mempool::Config {
            tx_cost_limit,
            ..Default::default()
        },
        Buffer::new(BoxService::new(peer_set.clone()), 1),
        state_service.clone(),
        Buffer::new(BoxService::new(tx_verifier.clone()), 1),
        sync_status,
        latest_chain_tip,
        chain_tip_change,
    );

    (mempool, peer_set, state_service, tx_verifier, recent_syncs)
}
