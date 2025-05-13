//! Randomised property tests for the RPC Queue.

use std::{collections::HashSet, env, sync::Arc};

use proptest::prelude::*;

use chrono::Duration;
use tokio::{sync::oneshot, time};
use tower::ServiceExt;

use zebra_chain::{
    block::{Block, Height},
    serialization::ZcashDeserializeInto,
    transaction::{Transaction, UnminedTx},
};
use zebra_node_services::mempool::{Gossip, Request, Response};
use zebra_state::{BoxError, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::queue::{Queue, Runner, CHANNEL_AND_QUEUE_CAPACITY};

/// The default number of proptest cases for these tests.
const DEFAULT_BLOCK_VEC_PROPTEST_CASES: u32 = 2;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BLOCK_VEC_PROPTEST_CASES))
    )]

    /// Test insert to the queue and remove from it.
    #[test]
    fn insert_remove_to_from_queue(transaction in any::<UnminedTx>()) {
        // create a queue
        let (mut runner, _sender) = Queue::start();

        // insert transaction
        runner.queue.insert(transaction.clone());

        // transaction was inserted to queue
        let queue_transactions = runner.queue.transactions();
        prop_assert_eq!(1, queue_transactions.len());

        // remove transaction from the queue
        runner.queue.remove(transaction.id);

        // transaction was removed from queue
        prop_assert_eq!(runner.queue.transactions().len(), 0);
    }

    /// Test queue never grows above limit.
    #[test]
    fn queue_size_limit(transactions in any::<[UnminedTx; CHANNEL_AND_QUEUE_CAPACITY + 1]>()) {
        // create a queue
        let (mut runner, _sender) = Queue::start();

        // insert all transactions we have
        transactions.iter().for_each(|t| runner.queue.insert(t.clone()));

        // transaction queue is never above limit
        let queue_transactions = runner.queue.transactions();
        prop_assert_eq!(CHANNEL_AND_QUEUE_CAPACITY, queue_transactions.len());
    }

    /// Test queue order.
    #[test]
    fn queue_order(transactions in any::<[UnminedTx; 32]>()) {
        // create a queue
        let (mut runner, _sender) = Queue::start();
        // fill the queue and check insertion order
        for i in 0..CHANNEL_AND_QUEUE_CAPACITY {
            let transaction = transactions[i].clone();
            runner.queue.insert(transaction.clone());
            let queue_transactions = runner.queue.transactions();
            prop_assert_eq!(i + 1, queue_transactions.len());
            prop_assert_eq!(UnminedTx::from(queue_transactions[i].0.clone()), transaction);
        }

        // queue is full
        let queue_transactions = runner.queue.transactions();
        prop_assert_eq!(CHANNEL_AND_QUEUE_CAPACITY, queue_transactions.len());

        // keep adding transaction, new transactions will always be on top of the queue
        for transaction in transactions.iter().skip(CHANNEL_AND_QUEUE_CAPACITY) {
            runner.queue.insert(transaction.clone());
            let queue_transactions = runner.queue.transactions();
            prop_assert_eq!(CHANNEL_AND_QUEUE_CAPACITY, queue_transactions.len());
            prop_assert_eq!(UnminedTx::from(queue_transactions.last().unwrap().1.0.clone()), transaction.clone());
        }

        // check the order of the final queue
        let queue_transactions = runner.queue.transactions();
        for i in 0..CHANNEL_AND_QUEUE_CAPACITY {
            let transaction = transactions[(CHANNEL_AND_QUEUE_CAPACITY - 8) + i].clone();
            prop_assert_eq!(UnminedTx::from(queue_transactions[i].0.clone()), transaction);
        }
    }

    /// Test transactions are removed from the queue after time elapses.
    #[test]
    fn remove_expired_transactions_from_queue(transaction in any::<UnminedTx>()) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            // pause the clock
            time::pause();

            // create a queue
            let (mut runner, _sender) = Queue::start();

            // insert a transaction to the queue
            runner.queue.insert(transaction);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // have a block interval value equal to the one at Height(1)
            let spacing = Duration::seconds(150);

            // apply expiration immediately, transaction will not be removed from queue
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 1 block elapsed, transaction will not be removed from queue
            time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 2 blocks elapsed, transaction will not be removed from queue
            time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 3 blocks elapsed, transaction will not be removed from queue
            time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 4 blocks elapsed, transaction will not be removed from queue
            time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 5 block elapsed, transaction will not be removed from queue
            // as it needs the extra time of 5 seconds
            time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply 6 seconds more, transaction will be removed from the queue
            time::advance(chrono::Duration::seconds(6).to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 0);

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test transactions are removed from queue after they get in the mempool
    #[test]
    fn queue_runner_mempool(transaction in any::<Transaction>()) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();

            // create a queue
            let (mut runner, _sender) = Queue::start();

            // insert a transaction to the queue
            let unmined_transaction = UnminedTx::from(transaction);
            runner.queue.insert(unmined_transaction.clone());
            let transactions = runner.queue.transactions();
            prop_assert_eq!(transactions.len(), 1);

            // get a `HashSet` of transactions to call mempool with
            let transactions_hash_set = runner.transactions_as_hash_set();

            // run the mempool checker
            let send_task = tokio::spawn(Runner::check_mempool(mempool.clone(), transactions_hash_set.clone()));

            // mempool checker will call the mempool looking for the transaction
            let expected_request = Request::TransactionsById(transactions_hash_set.clone());
            let response = Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);
            let result = send_task.await.expect("Requesting transactions should not panic");

            // empty results, transaction is not in the mempool
            prop_assert_eq!(result, HashSet::new());

            // insert transaction to the mempool
            let request = Request::Queue(vec![Gossip::Tx(unmined_transaction.clone())]);
            let expected_request = Request::Queue(vec![Gossip::Tx(unmined_transaction.clone())]);
            let send_task = tokio::spawn(mempool.clone().oneshot(request));
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Ok(()));
            let response = Response::Queued(vec![Ok(rsp_rx)]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let _ = send_task.await.expect("Inserting to mempool should not panic");

            // check the mempool again
            let send_task = tokio::spawn(Runner::check_mempool(mempool.clone(), transactions_hash_set.clone()));

            // mempool checker will call the mempool looking for the transaction
            let expected_request = Request::TransactionsById(transactions_hash_set);
            let response = Response::Transactions(vec![unmined_transaction]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transactions should not panic");

            // transaction is in the mempool
            prop_assert_eq!(result.len(), 1);

            // but it is not deleted from the queue yet
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // delete by calling remove_committed
            runner.remove_committed(result);
            prop_assert_eq!(runner.queue.transactions().len(), 0);

            // no more requests expected
            mempool.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test transactions are removed from queue after they get in the state
    #[test]
    fn queue_runner_state(transaction in any::<Transaction>()) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut read_state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();
            let mut write_state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            // create a queue
            let (mut runner, _sender) = Queue::start();

            // insert a transaction to the queue
            let unmined_transaction = UnminedTx::from(&transaction);
            runner.queue.insert(unmined_transaction.clone());
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // get a `HashSet` of transactions to call state with
            let transactions_hash_set = runner.transactions_as_hash_set();

            let send_task = tokio::spawn(Runner::check_state(read_state.clone(), transactions_hash_set.clone()));

            let expected_request = ReadRequest::Transaction(transaction.hash());
            let response = ReadResponse::Transaction(None);

            read_state
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transaction should not panic");
            // transaction is not in the state
            prop_assert_eq!(HashSet::new(), result);

            // get a block and push our transaction to it
            let block =
                zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into::<Arc<Block>>()?;
            let mut block = Arc::try_unwrap(block).expect("block should unwrap");
            block.transactions.push(Arc::new(transaction.clone()));

            // commit the created block
            let request = zebra_state::Request::CommitCheckpointVerifiedBlock(zebra_state::CheckpointVerifiedBlock::from(Arc::new(block.clone())));
            let send_task = tokio::spawn(write_state.clone().oneshot(request.clone()));
            let response = zebra_state::Response::Committed(block.hash());

            write_state
                .expect_request(request)
                .await?
                .respond(response);

            let _ = send_task.await.expect("Inserting block to state should not panic");

            // check the state again
            let send_task = tokio::spawn(Runner::check_state(read_state.clone(), transactions_hash_set));

            let expected_request = ReadRequest::Transaction(transaction.hash());
            let response = ReadResponse::Transaction(Some(zebra_state::MinedTx::new(Arc::new(transaction), Height(1), 1, block.header.time)));

            read_state
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transaction should not panic");

            // transaction was found in the state
            prop_assert_eq!(result.len(), 1);

            read_state.expect_no_requests().await?;
            write_state.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    // Test any given transaction can be mempool retried.
    #[test]
    fn queue_mempool_retry(transaction in any::<Transaction>()) {
        let (runtime, _init_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();

            // create a queue
            let (mut runner, _sender) = Queue::start();

            // insert a transaction to the queue
            let unmined_transaction = UnminedTx::from(transaction.clone());
            runner.queue.insert(unmined_transaction.clone());
            let transactions = runner.queue.transactions();
            prop_assert_eq!(transactions.len(), 1);

            // get a `Vec` of transactions to do retries
            let transactions_vec = runner.transactions_as_vec();

            // run retry
            let send_task = tokio::spawn(Runner::retry(mempool.clone(), transactions_vec.clone()));

            // retry will queue the transaction to mempool
            let gossip = Gossip::Tx(UnminedTx::from(transaction.clone()));
            let expected_request = Request::Queue(vec![gossip]);
            let (rsp_tx, rsp_rx) = oneshot::channel();
            let _ = rsp_tx.send(Ok(()));
            let response = Response::Queued(vec![Ok(rsp_rx)]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transactions should not panic");

            // retry was done
            prop_assert_eq!(result.len(), 1);

            Ok::<_, TestCaseError>(())
        })?;
    }
}
