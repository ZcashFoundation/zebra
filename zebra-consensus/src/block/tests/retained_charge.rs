//! The semantic verifier computes each block's memory charge once and carries it to the state.

use zebra_chain::transaction::UnminedTxId;
use zebra_test::mock_service::MockService;

use super::*;

/// Committed and proposed blocks reach the state with the charge of their block, so the state's
/// block writer never serializes them to charge them.
#[tokio::test]
async fn verified_blocks_reach_the_state_with_their_charge() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()?
        .into();
    let hash = block.hash();

    for is_proposal in [false, true] {
        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut transaction_verifier: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();
        let verifier = SemanticBlockVerifier::new(
            &Network::Mainnet,
            state.clone(),
            transaction_verifier.clone(),
        );
        let request = if is_proposal {
            Request::CheckProposal(block.clone())
        } else {
            Request::Commit(block.clone())
        };
        let verify = tokio::spawn(verifier.oneshot(request));

        state
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
        if !is_proposal {
            state
                .expect_request(zs::Request::MiningCandidate(block.clone()))
                .await
                .respond_error("early mining is disabled in this test".into());
        }
        transaction_verifier
            .expect_request_that(|_| true)
            .await
            .respond(transaction::BlockResponse {
                tx_id: UnminedTxId::from(block.transactions[0].as_ref()),
                miner_fee: None,
                sigops: 0,
            });

        let state_request = state
            .expect_request_that(|request| {
                matches!(
                    request,
                    zs::Request::CommitSemanticallyVerifiedBlock(_)
                        | zs::Request::CheckBlockProposalValidity(_)
                )
            })
            .await;
        let (zs::Request::CommitSemanticallyVerifiedBlock(prepared)
        | zs::Request::CheckBlockProposalValidity(prepared)) = state_request.request()
        else {
            unreachable!("the request matched one of these variants");
        };
        assert_eq!(
            prepared.retained_charge,
            Some(zs::block_retained_charge(
                &prepared.block,
                &prepared.transaction_hashes
            )),
            "proposal: {is_proposal}",
        );

        if is_proposal {
            state_request.respond_error("the test stops after the state request".into());
            verify.await?.expect_err("the state rejected the proposal");
        } else {
            state_request.respond(zs::Response::Committed(hash));
            assert_eq!(verify.await?.expect("the state committed the block"), hash);
        }
    }

    Ok(())
}
