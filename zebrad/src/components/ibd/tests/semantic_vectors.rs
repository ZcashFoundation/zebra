//! Fixed test vectors for the full-validation stage-2 service
//! ([`SemanticCommit`]), the second [`CommitStage`] implementation.
//!
//! These mirror the known-hash `convert_vectors` tests, but mock the consensus
//! block verifier instead of the state: the engine drives both stage-2
//! implementations through the same seam, so both must resolve with the
//! committed hash, surface a verifier failure as a commit-stage error, and
//! reject corrupt cached bytes before reaching the verifier.
//!
//! [`SemanticCommit`]: crate::components::ibd::semantic::SemanticCommit
//! [`CommitStage`]: crate::components::ibd::convert::CommitStage

use std::sync::Arc;

use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::GENESIS_PREVIOUS_BLOCK_HASH,
    serialization::ZcashDeserializeInto,
};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::ibd::{
        convert::{BlockPayload, ConvertError, IbdBlock, VerifyAndCommitError},
        semantic::SemanticCommit,
    },
    BoxError,
};

/// A mocked consensus block verifier for the full-validation stage-2 service.
type MockVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;

/// Returns the first continuous mainnet block above genesis (height 1).
fn block1() -> Arc<Block> {
    zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .nth(1)
        .expect("a continuous mainnet block at height 1")
        .zcash_deserialize_into()
        .expect("test vectors deserialize")
}

/// Builds an [`IbdBlock`] for `block` at `height` from a network delivery (an
/// already-deserialized payload).
fn network_block(height: u32, block: Arc<Block>) -> IbdBlock {
    IbdBlock {
        height: block::Height(height),
        expected: block.hash(),
        prev_expected: GENESIS_PREVIOUS_BLOCK_HASH,
        block: block.into(),
        source: None,
    }
}

/// The service hands the block to the verifier as a `Commit` request and
/// resolves with the committed hash only after the verifier answers.
#[tokio::test]
async fn semantic_commit_resolves_with_committed_hash() {
    let _init_guard = zebra_test::init();

    let block1 = block1();

    let mut verifier: MockVerifier = MockService::build().for_unit_tests();
    let mut service = SemanticCommit::new(verifier.clone());

    let call = service
        .ready()
        .await
        .expect("the mocked verifier is always ready")
        .call(network_block(1, block1.clone()));
    let call = tokio::spawn(call);

    let responder = verifier
        .expect_request_that(|req| matches!(req, zebra_consensus::Request::Commit(_)))
        .await;
    assert_eq!(
        responder.request().block().hash(),
        block1.hash(),
        "the verifier must receive the fetched block",
    );

    responder.respond(block1.hash());

    let committed_hash = call
        .await
        .expect("the call task does not panic")
        .expect("a healthy verifier commits the block");
    assert_eq!(committed_hash, block1.hash());
}

/// A verifier failure (an invalid block, or a state error during commit)
/// surfaces as the commit-stage error carrying the block's height and hash, so
/// the engine maps it per §4.6 rather than swallowing it.
#[tokio::test]
async fn verifier_failure_surfaces_as_commit_error() {
    let _init_guard = zebra_test::init();

    let block1 = block1();

    let mut verifier: MockVerifier = MockService::build().for_unit_tests();
    let mut service = SemanticCommit::new(verifier.clone());

    let call = service
        .ready()
        .await
        .expect("the mocked verifier is always ready")
        .call(network_block(1, block1.clone()));
    let call = tokio::spawn(call);

    verifier
        .expect_request_that(|req| matches!(req, zebra_consensus::Request::Commit(_)))
        .await
        .respond(Err(BoxError::from("injected verifier failure")));

    let error = call
        .await
        .expect("the call task does not panic")
        .expect_err("the injected verifier failure must surface");

    match error {
        VerifyAndCommitError::Commit { height, hash, .. } => {
            assert_eq!(height, block::Height(1));
            assert_eq!(hash, block1.hash());
        }
        other => panic!("expected a commit-stage failure, got: {other:?}"),
    }
}

/// Corrupt bytes promoted from the disk overflow tier fail as a verify-stage
/// error before the verifier is ever called: the engine discards the cache
/// entry and refetches, exactly as on the known-hash path.
#[tokio::test]
async fn corrupt_cached_bytes_fail_before_the_verifier() {
    let _init_guard = zebra_test::init();

    let expected = block1().hash();

    // A `MockService` with no expected request panics if it is ever called.
    let verifier: MockVerifier = MockService::build().for_unit_tests();
    let mut service = SemanticCommit::new(verifier);

    let request = IbdBlock {
        height: block::Height(1),
        expected,
        prev_expected: GENESIS_PREVIOUS_BLOCK_HASH,
        block: BlockPayload::Raw {
            bytes: vec![0xff; 16],
            body_offset: 0,
        },
        source: None,
    };

    let error = service
        .ready()
        .await
        .expect("the mocked verifier is always ready")
        .call(request)
        .await
        .expect_err("undeserializable cached bytes must fail");

    match error {
        VerifyAndCommitError::Verify(ConvertError::CorruptCachedBytes { height, .. }) => {
            assert_eq!(height, block::Height(1));
        }
        other => panic!("expected a corrupt-cache verify failure, got: {other:?}"),
    }
}
