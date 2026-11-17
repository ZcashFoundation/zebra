//! Regression tests for transaction request lifetimes during semantic block verification.

use std::{sync::Arc, time::Duration};

use tower::{buffer::Buffer, ServiceExt};
use zebra_chain::{
    block::Block, parameters::Network, serialization::ZcashDeserialize, transaction::UnminedTxId,
};
use zebra_test::mock_service::MockService;

use crate::{
    block::{Request, SemanticBlockVerifier},
    transaction::{BlockRequest, BlockResponse},
    BoxError,
};

/// Successful transaction responses do not require services to release their requests.
#[tokio::test]
async fn semantic_verification_retains_transaction_request() {
    let _init_guard = zebra_test::init();

    tokio::time::timeout(Duration::from_secs(10), async {
        let network = Network::Mainnet;
        let block = Arc::new(
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .expect("canonical mainnet block must deserialize"),
        );
        assert_eq!(block.transactions.len(), 1);
        assert!(block.transactions[0].is_coinbase());
        let hash = block.hash();

        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut transactions: MockService<BlockRequest, BlockResponse, _, BoxError> =
            MockService::build().for_unit_tests();
        let verifier = SemanticBlockVerifier::new(
            &network,
            state.clone(),
            Buffer::new(transactions.clone(), 1),
        );
        let verify = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

        state
            .expect_request(zebra_state::Request::KnownBlock(hash))
            .await
            .respond(zebra_state::Response::KnownBlock(None));

        let response = transactions.expect_request_that(|_| true).await;
        // Keep the full mock request alive beyond both its response and block verification.
        let retained_request = response.request().clone();
        response.respond(BlockResponse {
            tx_id: UnminedTxId::from(retained_request.transaction.as_ref()),
            miner_fee: None,
            sigops: 0,
        });

        let commit = state
            .expect_request_that(|request| {
                matches!(
                    request,
                    zebra_state::Request::CommitSemanticallyVerifiedBlock(_)
                )
            })
            .await;
        let zebra_state::Request::CommitSemanticallyVerifiedBlock(committed) = commit.request()
        else {
            unreachable!("the expected request commits a semantically verified block")
        };
        assert_eq!(committed.hash, hash);
        assert_eq!(committed.new_outputs, *retained_request.known_utxos);
        commit.respond(zebra_state::Response::Committed(hash));

        assert_eq!(
            verify
                .await
                .expect("semantic verification must not panic with a retained request")
                .expect("a canonical block must reach a successful state commit"),
            hash,
        );
        assert_eq!(retained_request.transaction, block.transactions[0]);
        assert!(!retained_request.known_utxos.is_empty());
    })
    .await
    .expect("semantic verification and mock responses must finish within the timeout");
}
