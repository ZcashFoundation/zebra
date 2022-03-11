use jsonrpc_core::{Error, ErrorCode};
use proptest::prelude::*;
use thiserror::Error;
use tower::buffer::Buffer;

use zebra_chain::{
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::{Transaction, UnminedTx},
};
use zebra_node_services::mempool;
use zebra_state::BoxError;

use zebra_test::mock_service::MockService;

use super::super::{Rpc, RpcImpl, SentTransactionHash};

proptest! {
    /// Test that when sending a raw transaction, it is received by the mempool service.
    #[test]
    fn mempool_receives_raw_transaction(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let mut state: MockService<_, _, _, BoxError> = MockService::build().for_prop_tests();
            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1), Buffer::new(state.clone(), 1));
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

            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1), Buffer::new(state.clone(), 1));

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

            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1), Buffer::new(state.clone(), 1));

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

            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1), Buffer::new(state.clone(), 1));

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

            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1), Buffer::new(state.clone(), 1));

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

            Ok::<_, TestCaseError>(())
        })?;
    }
}

#[derive(Clone, Copy, Debug, Error)]
#[error("a dummy error type")]
pub struct DummyError;
