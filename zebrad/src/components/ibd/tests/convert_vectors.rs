//! Fixed test vectors for the verify-and-commit service (design doc §4.3).

use std::sync::Arc;

use futures::future;
use tower::{Service, ServiceExt};

use zebra_chain::{
    block::{self, merkle, Block},
    parameters::{Network, GENESIS_PREVIOUS_BLOCK_HASH},
    serialization::ZcashDeserializeInto,
};
use zebra_network::PeerSocketAddr;
use zebra_state as zs;
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    components::ibd::convert::{
        convert, BlockPayload, ConvertError, IbdBlock, VerifyAndCommit, VerifyAndCommitError,
    },
    BoxError,
};

/// A mocked state service for the verify-and-commit service.
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;

/// Returns the deserialized mainnet genesis block.
fn genesis() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .expect("test vectors deserialize")
}

/// Returns the deserialized mainnet block 1.
fn block1() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test vectors deserialize")
}

/// Returns the deserialized mainnet block 2.
fn block2() -> Arc<Block> {
    zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into()
        .expect("test vectors deserialize")
}

/// A real pinned block converts: the result carries the pinned hash, the
/// assigned height, the precomputed transaction hashes, and the serialized
/// size.
#[test]
fn convert_happy_path_returns_pinned_block_and_size() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let converted = convert(
        &Network::Mainnet,
        block::Height(1),
        block1.hash(),
        genesis.hash(),
        block1.clone(),
    )
    .expect("a real mainnet block converts, because the list pins the real chain");

    assert_eq!(converted.hash, block1.hash());
    assert_eq!(converted.height, block::Height(1));
    assert_eq!(
        converted.transaction_hashes.len(),
        block1.transactions.len()
    );
}

/// Genesis converts with [`GENESIS_PREVIOUS_BLOCK_HASH`] as its parent pin:
/// the engine has no `request_genesis` special case (design doc §4.4).
#[test]
fn convert_genesis_with_genesis_previous_block_hash() {
    let _init_guard = zebra_test::init();

    let genesis = genesis();

    let converted = convert(
        &Network::Mainnet,
        block::Height(0),
        genesis.hash(),
        GENESIS_PREVIOUS_BLOCK_HASH,
        genesis.clone(),
    )
    .expect("the genesis block converts at height zero");

    assert_eq!(converted.hash, genesis.hash());
    assert_eq!(converted.height, block::Height(0));
}

/// A coinbase height that disagrees with the assigned list index is a fatal
/// list diagnostic, never attributed to a peer.
#[test]
fn assigned_height_mismatch_is_fatal_wrong_height() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let error = convert(
        &Network::Mainnet,
        block::Height(2),
        block1.hash(),
        genesis.hash(),
        block1,
    )
    .expect_err("block 1's coinbase height can never match an assigned height of 2");

    assert!(
        matches!(
            error,
            ConvertError::WrongHeight {
                height: block::Height(2),
                coinbase_height: Some(block::Height(1)),
                ..
            }
        ),
        "unexpected error: {error:?}",
    );
    assert!(error.is_fatal_list_diagnostic());
    assert!(!error.is_peer_attributable());

    // Peer attribution never sticks to a list diagnostic.
    let error = error.with_source_peer(Some(PeerSocketAddr::from(([192, 168, 1, 1], 8233))));
    assert_eq!(error.source_peer(), None);
}

/// A parent hash that disagrees with the list entry below it is a fatal list
/// diagnostic naming both pins.
#[test]
fn parent_pin_mismatch_is_fatal_broken_link() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    // Wrong parent pin: the list entry below block 1 "claims" block 1 itself.
    let error = convert(
        &Network::Mainnet,
        block::Height(1),
        block1.hash(),
        block1.hash(),
        block1.clone(),
    )
    .expect_err("a parent pin that isn't the genesis hash can never match block 1's header");

    match &error {
        ConvertError::BrokenLink {
            height,
            prev_expected,
            found,
            ..
        } => {
            assert_eq!(*height, block::Height(1));
            assert_eq!(*prev_expected, block1.hash());
            assert_eq!(*found, genesis.hash());
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(error.is_fatal_list_diagnostic());
    assert!(!error.is_peer_attributable());
}

/// Fully replacing a block's transactions with another height's swaps the
/// coinbase too, so the coinbase-height check fires before the merkle check
/// (design doc §4.3 check order).
#[test]
fn fully_swapped_body_hits_wrong_height_first() {
    let _init_guard = zebra_test::init();

    let (genesis, block1, block2) = (genesis(), block1(), block2());

    let swapped = Arc::new(Block {
        header: block1.header.clone(),
        transactions: block2.transactions.clone(),
    });

    let error = convert(
        &Network::Mainnet,
        block::Height(1),
        block1.hash(),
        genesis.hash(),
        swapped,
    )
    .expect_err("block 2's coinbase commits to height 2, not the assigned height 1");

    assert!(
        matches!(
            error,
            ConvertError::WrongHeight {
                coinbase_height: Some(block::Height(2)),
                ..
            }
        ),
        "unexpected error: {error:?}",
    );
}

/// A header-valid block whose body was tampered with (a foreign transaction
/// spliced in behind block 1's own coinbase) fails the merkle pin, with the
/// delivering peer attached, and never reaches the state.
#[tokio::test]
async fn body_swapped_block_fails_bad_merkle_root_with_attribution() {
    let _init_guard = zebra_test::init();

    let (genesis, block1, block2) = (genesis(), block1(), block2());
    let peer = PeerSocketAddr::from(([192, 168, 1, 1], 8233));

    // Keep block 1's coinbase (so the coinbase-height and parent-pin checks
    // pass) and splice in block 2's transactions behind it.
    let mut transactions = block1.transactions.clone();
    transactions.extend(block2.transactions.iter().cloned());
    let tampered = Arc::new(Block {
        header: block1.header.clone(),
        transactions,
    });

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let response = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            block: tampered.into(),
            source: Some(peer),
        })
        .await;

    let error = response.expect_err("a foreign transaction body must fail the merkle pin");
    let VerifyAndCommitError::Verify(error) = error else {
        panic!("expected a verify-stage failure, got: {error:?}");
    };

    assert!(
        matches!(
            &error,
            ConvertError::BadMerkleRoot {
                source_peer: Some(source_peer),
                ..
            } if *source_peer == peer
        ),
        "unexpected error: {error:?}",
    );
    assert!(error.is_peer_attributable());
    assert_eq!(error.source_peer(), Some(peer));

    // A block that fails verification must never be sent to the state.
    state.expect_no_requests().await;
}

/// Duplicating the final transaction of an odd-length list leaves the merkle
/// root unchanged (CVE-2012-2459); the duplicate-txid check catches the
/// malleated body behind the still-matching pin.
#[test]
fn duplicate_transactions_behind_unchanged_merkle_root_are_rejected() {
    let _init_guard = zebra_test::init();

    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES
        .zcash_deserialize_into()
        .expect("test vectors deserialize");
    assert!(
        block.transactions.len() >= 3 && block.transactions.len() % 2 == 1,
        "this vector needs an odd transaction count of at least 3, \
         so that duplicating the final transaction preserves the merkle root",
    );

    let mut transactions = block.transactions.clone();
    transactions.push(
        transactions
            .last()
            .expect("the vector block has transactions")
            .clone(),
    );
    let malleated = Arc::new(Block {
        header: block.header.clone(),
        transactions,
    });

    // The malleated body still matches the pinned header's merkle root, so
    // only the duplicate-txid check can reject it.
    let malleated_root: merkle::Root = malleated.transactions.iter().map(|tx| tx.hash()).collect();
    assert_eq!(
        malleated_root, block.header.merkle_root,
        "the merkle malleation must preserve the pinned root",
    );

    let error = convert(
        &Network::Mainnet,
        block::Height(1_046_401),
        block.hash(),
        block.header.previous_block_hash,
        malleated,
    )
    .expect_err("a block with duplicate transactions must be rejected");

    assert!(
        matches!(error, ConvertError::DuplicateTransaction { .. }),
        "unexpected error: {error:?}",
    );
    assert!(error.is_peer_attributable());
}

/// The service verifies, commits through the state, and resolves with the
/// committed hash only after the state answers.
#[tokio::test]
async fn verify_and_commit_resolves_with_committed_hash() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let call = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            block: block1.clone().into(),
            source: None,
        });
    let call = tokio::spawn(call);

    let responder = state
        .expect_request_that(|req| matches!(req, zs::Request::CommitCheckpointVerifiedBlock(_)))
        .await;
    let zs::Request::CommitCheckpointVerifiedBlock(committed) = responder.request() else {
        unreachable!("expect_request_that only matches the commit variant");
    };
    assert_eq!(committed.hash, block1.hash());
    assert_eq!(committed.height, block::Height(1));
    assert_eq!(
        committed.transaction_hashes.len(),
        block1.transactions.len()
    );

    let hash = committed.hash;
    responder.respond(zs::Response::Committed(hash));

    let committed_hash = call
        .await
        .expect("the call task does not panic")
        .expect("a pinned block with a healthy state commits");
    assert_eq!(committed_hash, block1.hash());
}

/// A state failure on commit surfaces as the distinct commit-stage error,
/// carrying the block's pins, so the engine can map it per §4.6 instead of
/// blaming the delivering peer.
#[tokio::test]
async fn state_failure_surfaces_as_commit_error() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let call = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            block: block1.clone().into(),
            source: None,
        });
    let call = tokio::spawn(call);

    state
        .expect_request_that(|req| matches!(req, zs::Request::CommitCheckpointVerifiedBlock(_)))
        .await
        .respond(Err(BoxError::from("injected state commit failure")));

    let error = call
        .await
        .expect("the call task does not panic")
        .expect_err("the injected state failure must surface");

    match error {
        VerifyAndCommitError::Commit { height, hash, .. } => {
            assert_eq!(height, block::Height(1));
            assert_eq!(hash, block1.hash());
        }
        other => panic!("expected a commit-stage failure, got: {other:?}"),
    }
}

/// Eight calls overlap: every conversion runs on rayon and every commit
/// request reaches the state while none has been answered, so no call
/// serializes behind another call's unresolved commit.
#[tokio::test]
async fn eight_conversions_and_commits_overlap() {
    let _init_guard = zebra_test::init();

    const COUNT: usize = 8;

    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .values()
        .take(COUNT + 1)
        .map(|bytes| {
            bytes
                .zcash_deserialize_into()
                .expect("test vectors deserialize")
        })
        .collect();
    assert_eq!(
        blocks.len(),
        COUNT + 1,
        "not enough continuous mainnet test vectors",
    );

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let mut calls = Vec::new();
    for height in 1..=COUNT {
        let request = IbdBlock {
            // the tiny test range fits u32
            height: block::Height(height as u32),
            expected: blocks[height].hash(),
            prev_expected: blocks[height - 1].hash(),
            block: blocks[height].clone().into(),
            source: None,
        };

        calls.push(
            service
                .ready()
                .await
                .expect("the mocked state service is always ready")
                .call(request),
        );
    }

    let results = tokio::spawn(future::join_all(calls));

    // All eight commit requests arrive before any is answered: every call is
    // in flight simultaneously.
    let mut responders = Vec::new();
    for _ in 0..COUNT {
        responders.push(
            state
                .expect_request_that(|req| {
                    matches!(req, zs::Request::CommitCheckpointVerifiedBlock(_))
                })
                .await,
        );
    }

    let mut pending_heights: Vec<u32> = responders
        .iter()
        .map(|responder| {
            let zs::Request::CommitCheckpointVerifiedBlock(committed) = responder.request() else {
                unreachable!("expect_request_that only matches the commit variant");
            };
            committed.height.0
        })
        .collect();
    pending_heights.sort_unstable();
    assert_eq!(
        pending_heights,
        // the tiny test range fits u32
        (1..=COUNT as u32).collect::<Vec<_>>(),
        "every height must be in flight simultaneously",
    );

    for responder in responders {
        let zs::Request::CommitCheckpointVerifiedBlock(committed) = responder.request() else {
            unreachable!("expect_request_that only matches the commit variant");
        };
        let hash = committed.hash;
        responder.respond(zs::Response::Committed(hash));
    }

    let results = results.await.expect("the join task does not panic");
    assert_eq!(results.len(), COUNT);
    for (idx, result) in results.into_iter().enumerate() {
        let hash = result.expect("every pinned block commits");
        assert_eq!(hash, blocks[idx + 1].hash());
    }
}

/// Raw cached bytes (a disk-tier promotion, design doc §4.5) deserialize
/// inside the verify stage and commit exactly like a network block.
#[tokio::test]
async fn raw_cached_bytes_verify_and_commit() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let call = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            block: BlockPayload::Raw {
                bytes: zebra_test::vectors::BLOCK_MAINNET_1_BYTES.to_vec(),
                body_offset: 0,
            },
            source: None,
        });
    let call = tokio::spawn(call);

    let responder = state
        .expect_request_that(|req| matches!(req, zs::Request::CommitCheckpointVerifiedBlock(_)))
        .await;
    let zs::Request::CommitCheckpointVerifiedBlock(committed) = responder.request() else {
        unreachable!("expect_request_that only matches the commit variant");
    };
    let hash = committed.hash;
    responder.respond(zs::Response::Committed(hash));

    let committed_hash = call
        .await
        .expect("the call task does not panic")
        .expect("pinned raw bytes with a healthy state commit");
    assert_eq!(committed_hash, block1.hash());
}

/// Truncated cached bytes fail as a corrupt cache entry — not attributable
/// to any peer — and never reach the state.
#[tokio::test]
async fn truncated_cached_bytes_fail_as_corrupt() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    // Half a block: a torn write, the §4.5 exception (a) shape.
    let bytes = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.to_vec();
    let truncated = bytes[..bytes.len() / 2].to_vec();

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let error = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            block: BlockPayload::Raw {
                bytes: truncated,
                body_offset: 0,
            },
            source: Some(PeerSocketAddr::from(([192, 168, 1, 1], 8233))),
        })
        .await
        .expect_err("truncated bytes must fail verification");

    let VerifyAndCommitError::Verify(error) = error else {
        panic!("expected a verify-stage failure, got: {error:?}");
    };
    assert!(
        matches!(error, ConvertError::CorruptCachedBytes { .. }),
        "unexpected error: {error:?}",
    );
    assert!(
        !error.is_peer_attributable(),
        "disk corruption must not be blamed on the delivering peer",
    );
    assert_eq!(error.source_peer(), None);
    assert!(!error.is_fatal_list_diagnostic());

    state.expect_no_requests().await;
}

/// Valid block bytes promoted under the wrong pinned hash fail the
/// receipt-equivalent hash re-check that network blocks already passed in
/// the connection handler.
#[tokio::test]
async fn hash_mismatched_cached_bytes_fail_as_corrupt() {
    let _init_guard = zebra_test::init();

    let (genesis, block1) = (genesis(), block1());

    let mut state: MockState = MockService::build().for_unit_tests();
    let mut service = VerifyAndCommit::new(Network::Mainnet, state.clone());

    let error = service
        .ready()
        .await
        .expect("the mocked state service is always ready")
        .call(IbdBlock {
            height: block::Height(1),
            expected: block1.hash(),
            prev_expected: genesis.hash(),
            // Genesis bytes cached under block 1's pinned hash.
            block: BlockPayload::Raw {
                bytes: zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.to_vec(),
                body_offset: 0,
            },
            source: None,
        })
        .await
        .expect_err("bytes hashing to a different block must fail the hash re-check");

    let VerifyAndCommitError::Verify(error) = error else {
        panic!("expected a verify-stage failure, got: {error:?}");
    };
    assert!(
        matches!(error, ConvertError::CorruptCachedBytes { .. }),
        "unexpected error: {error:?}",
    );

    state.expect_no_requests().await;
}
