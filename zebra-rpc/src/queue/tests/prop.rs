//! Randomised property tests for the RPC Queue.

use std::{collections::HashSet, ops::Mul};

use proptest::prelude::*;

use tower::ServiceExt;

use zebra_chain::transaction::{Transaction, UnminedTx, UnminedTxId};
use zebra_node_services::mempool::{Gossip, Request, Response};
use zebra_state::{BoxError, ReadRequest, ReadResponse};
use zebra_test::mock_service::MockService;

use crate::queue::{Queue, Runner, CHANNEL_AND_QUEUE_CAPACITY};

proptest! {
    /// Test insert to the queue and remove from it.
    #[test]
    fn insert_remove_to_from_queue(transaction in any::<UnminedTx>()) {
        // create a queue
        let mut runner = Queue::start();

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
        let mut runner = Queue::start();

        // insert all transactions we have
        transactions.iter().for_each(|t| runner.queue.insert(t.clone()));

        // transaction queue is never above limit
        let queue_transactions = runner.queue.transactions();
        prop_assert_eq!(CHANNEL_AND_QUEUE_CAPACITY, queue_transactions.len());
    }

    /// Test queue order.
    #[test]
    fn queue_order(transactions in any::<[UnminedTx; CHANNEL_AND_QUEUE_CAPACITY * 2]>()) {
        // create a queue
        let mut runner = Queue::start();
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
            let transaction = transactions[CHANNEL_AND_QUEUE_CAPACITY + i].clone();
            prop_assert_eq!(UnminedTx::from(queue_transactions[i].0.clone()), transaction);
        }
    }

    /// Test transactions are removed from the queue after time elapses.
    #[test]
    fn remove_expired_transactions_from_queue(transaction in any::<UnminedTx>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            // pause the clock
            tokio::time::pause();

            // create a queue
            let mut runner = Queue::start();

            // insert a transaction to the queue
            runner.queue.insert(transaction);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // have a block interval value
            let spacing = chrono::Duration::seconds(150);

            // apply expiration inmediatly, transaction will not be removed from queue
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 1 block elapsed, transaction will not be removed from queue
            tokio::time::advance(spacing.to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 2 blocks elapsed, transaction will not be removed from queue
            tokio::time::advance(spacing.mul(2).to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // apply expiration after 3 block elapsed, transaction will be removed from queue
            tokio::time::advance(spacing.mul(3).to_std().unwrap()).await;
            runner.remove_expired(spacing);
            prop_assert_eq!(runner.queue.transactions().len(), 0);

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test transactions are removed from queue after they get in the mempool
    #[test]
    fn queue_runner_mempool(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();

            // create a queue
            let mut runner = Queue::start();

            // insert a transaction to the queue
            let unmined_transaction = UnminedTx::from(transaction);
            runner.queue.insert(unmined_transaction.clone());

            let transactions = runner.queue.transactions();
            prop_assert_eq!(transactions.len(), 1);

            // convert to hashset
            let transactions_hash_set: HashSet<UnminedTxId> = transactions.iter().map(|t| *t.0).collect();

            // run the mempool checker
            let send_task = tokio::spawn(Runner::check_mempool(mempool.clone(), transactions_hash_set.clone()));

            let expected_request = Request::TransactionsById(transactions_hash_set.clone());
            let response = Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transactions should not panic");

            // empty results, transaction is not in the mempool
            prop_assert_eq!(result, HashSet::new());

            // now lets insert it to the mempool
            let request = Request::Queue(vec![Gossip::Tx(unmined_transaction.clone())]);
            let expected_request = Request::Queue(vec![Gossip::Tx(unmined_transaction.clone())]);

            let send_task = tokio::spawn(mempool.clone().oneshot(request));

            let response = Response::Queued(vec![Ok(())]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let _ = send_task.await.expect("Inserting to mempool should not panic");

            // check the mempool again
            let send_task = tokio::spawn(Runner::check_mempool(mempool.clone(), transactions_hash_set.clone()));

            let expected_request = Request::TransactionsById(transactions_hash_set);
            let response = Response::Transactions(vec![unmined_transaction]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transactions should not panic");

            prop_assert_eq!(result.len(), 1);
            // not deleted yet
            prop_assert_eq!(runner.queue.transactions().len(), 1);
            // delete
            runner.remove_committed(result);
            prop_assert_eq!(runner.queue.transactions().len(), 0);

            // no more
            mempool.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test transactions are removed from queue after they get in the state
    #[test]
    fn queue_runner_state(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        let transaction_hash = transaction.hash();

        runtime.block_on(async move {

            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            // create a queue
            let mut runner = Queue::start();

            // insert a transaction to the queue
            let unmined_transaction = UnminedTx::from(&transaction);
            runner.queue.insert(unmined_transaction.clone());
            prop_assert_eq!(runner.queue.transactions().len(), 1);

            // run the runner
            let mut hs = HashSet::new();
            hs.insert(unmined_transaction.id);

            let send_task = tokio::spawn(Runner::check_state(state.clone(), hs));

            let expected_request = ReadRequest::Transaction(transaction_hash);
            let response = ReadResponse::Transaction(None);

            state
                .expect_request(expected_request)
                .await?
                .respond(response);

            let result = send_task.await.expect("Requesting transaction should not panic");

            prop_assert_eq!(HashSet::new(), result);

            // TODO: finish this test

            state.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    // TODO: add a Runner::retry test
}
