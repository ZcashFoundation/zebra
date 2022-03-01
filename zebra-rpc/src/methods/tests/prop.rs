use proptest::prelude::*;
use tower::buffer::Buffer;

use zebra_chain::{
    serialization::ZcashSerialize,
    transaction::{Transaction, UnminedTx},
};
use zebra_node_services::mempool;
use zebra_test::mock_service::MockService;

use super::super::{Rpc, RpcImpl, SentTransactionHash};

proptest! {
    /// Test that when sending a raw transaction, it is received by the mempool service.
    #[test]
    fn mempool_receives_raw_transaction(transaction in any::<Transaction>()) {
        let runtime = zebra_test::init_async();

        runtime.block_on(async move {
            let mut mempool = MockService::build().for_prop_tests();
            let rpc = RpcImpl::new("RPC test".to_owned(), Buffer::new(mempool.clone(), 1));
            let hash = SentTransactionHash(transaction.hash().to_string());

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

            let result = send_task
                .await
                .expect("Sending raw transactions should not panic");

            prop_assert_eq!(result, Ok(hash));

            Ok::<_, TestCaseError>(())
        })?;
    }
}
