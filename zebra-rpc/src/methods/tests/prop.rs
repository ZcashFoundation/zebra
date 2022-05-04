//! Randomised property tests for RPC methods.

use std::collections::HashSet;

use futures::{join, FutureExt, TryFutureExt};
use hex::ToHex;
use jsonrpc_core::{Error, ErrorCode};
use proptest::{collection::vec, prelude::*};
use thiserror::Error;
use tower::buffer::Buffer;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{Block, Height},
    chain_tip::{mock::MockChainTip, NoChainTip},
    parameters::{
        Network::{self, *},
        NetworkUpgrade,
    },
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{self, Transaction, UnminedTx, UnminedTxId},
    transparent,
};
use zebra_node_services::mempool;
use zebra_state::BoxError;

use zebra_test::mock_service::MockService;

use super::super::{
    AddressBalance, AddressStrings, NetworkUpgradeStatus, Rpc, RpcImpl, SentTransactionHash,
};

proptest! {
    /// Test that when sending a raw transaction, it is received by the mempool service.
    #[test]
    fn mempool_receives_raw_transaction(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();
            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );
            let hash = SentTransactionHash(transaction.hash());

            let transaction_bytes = transaction
                .zcash_serialize_to_vec()
                .expect("Transaction serializes successfully");
            let transaction_hex = hex::encode(&transaction_bytes);

            let send_task = tokio::spawn(rpc.send_raw_transaction(transaction_hex));

            let unmined_transaction = UnminedTx::from(transaction);
            let expected_request = mempool::Request::Queue(vec![unmined_transaction.into()]);
            let response = mempool::Response::Queued(vec![Ok(())]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert_eq!(result, Ok(hash));

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that mempool errors are forwarded to the caller.
    ///
    /// Mempool service errors should become server errors.
    #[test]
    fn mempool_errors_are_forwarded(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let transaction_bytes = transaction
                .zcash_serialize_to_vec()
                .expect("Transaction serializes successfully");
            let transaction_hex = hex::encode(&transaction_bytes);

            let send_task = tokio::spawn(rpc.send_raw_transaction(transaction_hex));

            let unmined_transaction = UnminedTx::from(transaction);
            let expected_request = mempool::Request::Queue(vec![unmined_transaction.into()]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(Err(DummyError));

            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::ServerError(_),
                        ..
                    })
                ),
                "Result is not a server error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that when the mempool rejects a transaction the caller receives an error.
    #[test]
    fn rejected_transactions_are_reported(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let transaction_bytes = transaction
                .zcash_serialize_to_vec()
                .expect("Transaction serializes successfully");
            let transaction_hex = hex::encode(&transaction_bytes);

            let send_task = tokio::spawn(rpc.send_raw_transaction(transaction_hex));

            let unmined_transaction = UnminedTx::from(transaction);
            let expected_request = mempool::Request::Queue(vec![unmined_transaction.into()]);
            let response = mempool::Response::Queued(vec![Err(DummyError.into())]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::ServerError(_),
                        ..
                    })
                ),
                "Result is not a server error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that the method rejects non-hexadecimal characters.
    ///
    /// Try to call `send_raw_transaction` using a string parameter that has at least one
    /// non-hexadecimal character, and check that it fails with an expected error.
    #[test]
    fn non_hexadecimal_string_results_in_an_error(non_hex_string in ".*[^0-9A-Fa-f].*") {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let send_task = tokio::spawn(rpc.send_raw_transaction(non_hex_string));

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::InvalidParams,
                        ..
                    })
                ),
                "Result is not an invalid parameters error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that the method rejects an input that's not a transaction.
    ///
    /// Try to call `send_raw_transaction` using random bytes that fail to deserialize as a
    /// transaction, and check that it fails with an expected error.
    #[test]
    fn invalid_transaction_results_in_an_error(random_bytes in any::<Vec<u8>>()) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        prop_assume!(Transaction::zcash_deserialize(&*random_bytes).is_err());

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let send_task = tokio::spawn(rpc.send_raw_transaction(hex::encode(random_bytes)));

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::InvalidParams,
                        ..
                    })
                ),
                "Result is not an invalid parameters error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that the `getrawmempool` method forwards the transactions in the mempool.
    ///
    /// Make the mock mempool service return a list of transaction IDs, and check that the RPC call
    /// returns those IDs as hexadecimal strings.
    #[test]
    fn mempool_transactions_are_sent_to_caller(transaction_ids in any::<HashSet<UnminedTxId>>()) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let call_task = tokio::spawn(rpc.get_raw_mempool());
            let mut expected_response: Vec<String> = transaction_ids
                .iter()
                .map(|id| id.mined_id().encode_hex())
                .collect();
            expected_response.sort();

            mempool
                .expect_request(mempool::Request::TransactionIds)
                .await?
                .respond(mempool::Response::TransactionIds(transaction_ids));

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            let result = call_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert_eq!(result, Ok(expected_response));

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that the method rejects non-hexadecimal characters.
    ///
    /// Try to call `get_raw_transaction` using a string parameter that has at least one
    /// non-hexadecimal character, and check that it fails with an expected error.
    #[test]
    fn get_raw_transaction_non_hexadecimal_string_results_in_an_error(
        non_hex_string in ".*[^0-9A-Fa-f].*",
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let send_task = tokio::spawn(rpc.get_raw_transaction(non_hex_string, 0));

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::InvalidParams,
                        ..
                    })
                ),
                "Result is not an invalid parameters error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test that the method rejects an input that's not a transaction.
    ///
    /// Try to call `get_raw_transaction` using random bytes that fail to deserialize as a
    /// transaction, and check that it fails with an expected error.
    #[test]
    fn get_raw_transaction_invalid_transaction_results_in_an_error(
        random_bytes in any::<Vec<u8>>(),
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        // CORRECTNESS: Nothing in this test depends on real time, so we can speed it up.
        tokio::time::pause();

        prop_assume!(transaction::Hash::zcash_deserialize(&*random_bytes).is_err());

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let send_task = tokio::spawn(rpc.get_raw_transaction(hex::encode(random_bytes), 0));

            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::InvalidParams,
                        ..
                    })
                ),
                "Result is not an invalid parameters error: {result:?}"
            );

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the `get_blockchain_info` response when Zebra's state is empty.
    #[test]
    fn get_blockchain_info_response_without_a_chain_tip(network in any::<Network>()) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();
        let mut mempool = MockService::build().for_prop_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

        // look for an error with a `NoChainTip`
        let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
            "RPC test",
            Buffer::new(mempool.clone(), 1),
            Buffer::new(state.clone(), 1),
            NoChainTip,
            network,
        );

        let response = rpc.get_blockchain_info();
        prop_assert_eq!(
            &response.err().unwrap().message,
            "No Chain tip available yet"
        );

        // The queue task should continue without errors or panics
        let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
        prop_assert!(matches!(rpc_tx_queue_task_result, None));

        runtime.block_on(async move {
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;
            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the `get_blockchain_info` response using an arbitrary block as the `ChainTip`.
    #[test]
    fn get_blockchain_info_response_with_an_arbitrary_chain_tip(
        network in any::<Network>(),
        block in any::<Block>(),
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();
        let mut mempool = MockService::build().for_prop_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

        // get block data
        let block_height = block.coinbase_height().unwrap();
        let block_hash = block.hash();
        let block_time = block.header.time;

        // create a mocked `ChainTip`
        let (chain_tip, mock_chain_tip_sender) = MockChainTip::new();
        mock_chain_tip_sender.send_best_tip_height(block_height);
        mock_chain_tip_sender.send_best_tip_hash(block_hash);
        mock_chain_tip_sender.send_best_tip_block_time(block_time);

        // Start RPC with the mocked `ChainTip`
        let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
            "RPC test",
            Buffer::new(mempool.clone(), 1),
            Buffer::new(state.clone(), 1),
            chain_tip,
            network,
        );
        let response = rpc.get_blockchain_info();

        // Check response
        match response {
            Ok(info) => {
                prop_assert_eq!(info.chain, network.bip70_network_name());
                prop_assert_eq!(info.blocks, block_height);
                prop_assert_eq!(info.best_block_hash, block_hash);
                prop_assert!(info.estimated_height < Height::MAX);

                prop_assert_eq!(
                    info.consensus.chain_tip.0,
                    NetworkUpgrade::current(network, block_height)
                        .branch_id()
                        .unwrap()
                );
                prop_assert_eq!(
                    info.consensus.next_block.0,
                    NetworkUpgrade::current(network, (block_height + 1).unwrap())
                        .branch_id()
                        .unwrap()
                );

                for u in info.upgrades {
                    let mut status = NetworkUpgradeStatus::Active;
                    if block_height < u.1.activation_height {
                        status = NetworkUpgradeStatus::Pending;
                    }
                    prop_assert_eq!(u.1.status, status);
                }
            }
            Err(_) => {
                unreachable!("Test should never error with the data we are feeding it")
            }
        };

        // The queue task should continue without errors or panics
        let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
        prop_assert!(matches!(rpc_tx_queue_task_result, None));

        // check no requests were made during this test
        runtime.block_on(async move {
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the `get_address_balance` RPC using an arbitrary set of addresses.
    #[test]
    fn queries_balance_for_valid_addresses(
        network in any::<Network>(),
        addresses in any::<HashSet<transparent::Address>>(),
        balance in any::<Amount<NonNegative>>(),
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        let mut mempool = MockService::build().for_prop_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

        // Create a mocked `ChainTip`
        let (chain_tip, _mock_chain_tip_sender) = MockChainTip::new();

        // Prepare the list of addresses.
        let address_strings = AddressStrings {
            addresses: addresses
                .iter()
                .map(|address| address.to_string())
                .collect(),
        };

        tokio::time::pause();

        // Start RPC with the mocked `ChainTip`
        runtime.block_on(async move {
            let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                chain_tip,
                network,
            );

            // Build the future to call the RPC
            let call = rpc.get_address_balance(address_strings);

            // The RPC should perform a state query
            let state_query = state
                .expect_request(zebra_state::ReadRequest::AddressBalance(addresses))
                .map_ok(|responder| {
                    responder.respond(zebra_state::ReadResponse::AddressBalance(balance))
                });

            // Await the RPC call and the state query
            let (response, state_query_result) = join!(call, state_query);

            state_query_result?;

            // Check that response contains the expected balance
            let received_balance = response?;

            prop_assert_eq!(received_balance, AddressBalance { balance: balance.into() });

            // Check no further requests were made during this test
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the `get_address_balance` RPC using an invalid list of addresses.
    ///
    /// An error should be returned.
    #[test]
    fn does_not_query_balance_for_invalid_addresses(
        network in any::<Network>(),
        at_least_one_invalid_address in vec(".*", 1..10),
    ) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        prop_assume!(at_least_one_invalid_address
            .iter()
            .any(|string| string.parse::<transparent::Address>().is_err()));

        let mut mempool = MockService::build().for_prop_tests();
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

        // Create a mocked `ChainTip`
        let (chain_tip, _mock_chain_tip_sender) = MockChainTip::new();

        tokio::time::pause();

        // Start RPC with the mocked `ChainTip`
        runtime.block_on(async move {
            let (rpc, _rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                chain_tip,
                network,
            );

            let address_strings = AddressStrings {
                addresses: at_least_one_invalid_address,
            };

            // Build the future to call the RPC
            let result = rpc.get_address_balance(address_strings).await;

            // Check that the invalid addresses lead to an error
            prop_assert!(
                matches!(
                    result,
                    Err(Error {
                        code: ErrorCode::InvalidParams,
                        ..
                    })
                ),
                "Result is not a server error: {result:?}"
            );

            // Check no requests were made during this test
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test the queue functionality using `send_raw_transaction`
    #[test]
    fn rpc_queue_main_loop(tx in any::<Transaction>()) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        let transaction_hash = tx.hash();

        runtime.block_on(async move {
            tokio::time::pause();

            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            // send a transaction
            let tx_bytes = tx
                .zcash_serialize_to_vec()
                .expect("Transaction serializes successfully");
            let tx_hex = hex::encode(&tx_bytes);
            let send_task = tokio::spawn(rpc.send_raw_transaction(tx_hex));

            let tx_unmined = UnminedTx::from(tx);
            let expected_request = mempool::Request::Queue(vec![tx_unmined.clone().into()]);

            // fail the mempool insertion
            mempool
                .expect_request(expected_request)
                .await
                .unwrap()
                .respond(Err(DummyError));

            let _ = send_task
                .await
                .expect("Sending raw transactions should not panic");

            // advance enough time to have a new runner iteration
            let spacing = chrono::Duration::seconds(150);
            tokio::time::advance(spacing.to_std().unwrap()).await;

            // the runner will made a new call to TransactionsById
            let mut transactions_hash_set = HashSet::new();
            transactions_hash_set.insert(tx_unmined.id);
            let expected_request = mempool::Request::TransactionsById(transactions_hash_set);
            let response = mempool::Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // the runner will also query the state again for the transaction
            let expected_request = zebra_state::ReadRequest::Transaction(transaction_hash);
            let response = zebra_state::ReadResponse::Transaction(None);

            state
                .expect_request(expected_request)
                .await?
                .respond(response);

            // now a retry will be sent to the mempool
            let expected_request =
                mempool::Request::Queue(vec![mempool::Gossip::Tx(tx_unmined.clone())]);
            let response = mempool::Response::Queued(vec![Ok(())]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // no more requests are done
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }

    /// Test we receive all transactions that are sent in a channel
    #[test]
    fn rpc_queue_receives_all_transactions_from_channel(txs in any::<[Transaction; 2]>()) {
        let runtime = zebra_test::init_async();
        let _guard = runtime.enter();

        runtime.block_on(async move {
            tokio::time::pause();

            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();

            let (rpc, rpc_tx_queue_task_handle) = RpcImpl::new(
                "RPC test",
                Buffer::new(mempool.clone(), 1),
                Buffer::new(state.clone(), 1),
                NoChainTip,
                Mainnet,
            );

            let mut transactions_hash_set = HashSet::new();
            for tx in txs.clone() {
                // send a transaction
                let tx_bytes = tx
                    .zcash_serialize_to_vec()
                    .expect("Transaction serializes successfully");
                let tx_hex = hex::encode(&tx_bytes);
                let send_task = tokio::spawn(rpc.send_raw_transaction(tx_hex));

                let tx_unmined = UnminedTx::from(tx.clone());
                let expected_request = mempool::Request::Queue(vec![tx_unmined.clone().into()]);

                // insert to hs we will use later
                transactions_hash_set.insert(tx_unmined.id);

                // fail the mempool insertion
                mempool
                    .clone()
                    .expect_request(expected_request)
                    .await
                    .unwrap()
                    .respond(Err(DummyError));

                let _ = send_task
                    .await
                    .expect("Sending raw transactions should not panic");
            }

            // advance enough time to have a new runner iteration
            let spacing = chrono::Duration::seconds(150);
            tokio::time::advance(spacing.to_std().unwrap()).await;

            // the runner will made a new call to TransactionsById querying with both transactions
            let expected_request = mempool::Request::TransactionsById(transactions_hash_set);
            let response = mempool::Response::Transactions(vec![]);

            mempool
                .expect_request(expected_request)
                .await?
                .respond(response);

            // the runner will also query the state again for each transaction
            for _tx in txs.clone() {
                let response = zebra_state::ReadResponse::Transaction(None);

                // we use `expect_request_that` because we can't guarantee the state request order
                state
                    .expect_request_that(|request| {
                        matches!(request, zebra_state::ReadRequest::Transaction(_))
                    })
                    .await?
                    .respond(response);
            }

            // each transaction will be retried
            for tx in txs.clone() {
                let expected_request =
                    mempool::Request::Queue(vec![mempool::Gossip::Tx(UnminedTx::from(tx))]);
                let response = mempool::Response::Queued(vec![Ok(())]);

                mempool
                    .expect_request(expected_request)
                    .await?
                    .respond(response);
            }

            // no more requests are done
            mempool.expect_no_requests().await?;
            state.expect_no_requests().await?;

            // The queue task should continue without errors or panics
            let rpc_tx_queue_task_result = rpc_tx_queue_task_handle.now_or_never();
            prop_assert!(matches!(rpc_tx_queue_task_result, None));

            Ok::<_, TestCaseError>(())
        })?;
    }
}

#[derive(Clone, Copy, Debug, Error)]
#[error("a dummy error type")]
pub struct DummyError;
