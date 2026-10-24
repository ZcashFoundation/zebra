//! Tests for chain verification

#![allow(clippy::unwrap_in_result)]

use std::{sync::Arc, time::Duration};

use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use tower::{layer::Layer, timeout::TimeoutLayer};

use zebra_chain::{
    block::Block,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
};
use zebra_state as zs;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use super::*;

/// The timeout we apply to each verify future during testing.
///
/// The checkpoint verifier uses `tokio::sync::oneshot` channels as futures.
/// If the verifier doesn't send a message on the channel, any tests that
/// await the channel future will hang.
///
/// The block verifier waits for the previous block to reach the state service.
/// If that never happens, the test can hang.
///
/// This value is set to a large value, to avoid spurious failures due to
/// high system load.
const VERIFY_TIMEOUT_SECONDS: u64 = 10;

/// Initialization returns while preload is queued, and its handle covers all three key builds.
#[test]
fn init_preloads_halo2_keys_without_waiting() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(120), async {
            let (release, blocked) = std::sync::mpsc::channel();
            let (started, ready) = oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(120)).unwrap();
            });
            ready.await.unwrap();

            let state = tower::service_fn(|request| async move {
                Ok::<_, BoxError>(match request {
                    zs::Request::Tip => zs::Response::Tip(None),
                    zs::Request::BestChainBlockHash(_) => zs::Response::BlockHash(None),
                    _ => panic!("unexpected state request during initialization"),
                })
            });
            let read_state = tower::service_fn(|_: zs::ReadRequest| async {
                Err::<zs::ReadResponse, BoxError>("unexpected read during initialization".into())
            });
            let (_test_router, _test_transaction, test_handles, _, _) =
                init_test(Config::default(), &Network::Mainnet, state, read_state).await;
            tokio::time::timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                test_handles.halo2_key_preload_handle,
            )
            .await
            .expect("test initialization must leave keys lazy while the blocking worker is busy")
            .unwrap();
            test_handles.state_checkpoint_verify_handle.await.unwrap();

            let (_router, _transaction, handles, _, _) = tokio::time::timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                init(
                    Config::default(),
                    &Network::Mainnet,
                    state,
                    read_state,
                    oneshot::channel::<
                        Buffer<
                            BoxService<mempool::Request, mempool::Response, BoxError>,
                            mempool::Request,
                        >,
                    >()
                    .1,
                    None,
                ),
            )
            .await
            .expect("router initialization must not await queued key construction");

            assert!(
                !handles.halo2_key_preload_handle.is_finished(),
                "initialization must return before the blocking worker is available"
            );
            assert!(
                !halo2::tests::KEY_BUILDS
                    .lock()
                    .unwrap()
                    .contains(&std::thread::current().id()),
                "key construction must not run on the async caller"
            );
            release.send(()).unwrap();
            blocker.await.unwrap();
            handles.halo2_key_preload_handle.await.unwrap();
            handles.state_checkpoint_verify_handle.await.unwrap();
            assert_eq!(
                halo2::tests::KEY_BUILDS.lock().unwrap().len(),
                3,
                "preload must construct every era key without a verification request"
            );
        })
        .await
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    result.expect("initialization must not wait for key preload");
}

/// Generate a block with no transactions (not even a coinbase transaction).
///
/// The generated block should fail validation.
pub fn block_no_transactions() -> Block {
    Block {
        header: zebra_test::vectors::DUMMY_HEADER[..]
            .zcash_deserialize_into()
            .unwrap(),
        transactions: Vec::new(),
    }
}

/// Return a new chain verifier and state service,
/// using the hard-coded checkpoint list for `network`.
async fn verifiers_from_network(
    network: Network,
) -> (
    Buffer<BoxService<Request, block::Hash, RouterError>, Request>,
    Buffer<BoxService<zs::Request, zs::Response, BoxError>, zs::Request>,
) {
    let (state_service, read_state, _, _) = zs::init_test_services(&network).await;
    let (
        block_verifier_router,
        _transaction_verifier,
        _groth16_download_handle,
        _max_checkpoint_height,
        _prepared_blocks,
    ) = crate::router::init_test(
        Config::default(),
        &network,
        state_service.clone(),
        read_state,
    )
    .await;

    // We can drop the download task handle here, because:
    // - if the download task fails, the tests will panic, and
    // - if the download task hangs, the tests will hang.

    (block_verifier_router, state_service)
}

static BLOCK_VERIFY_TRANSCRIPT_GENESIS: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let hash = Ok(block.hash());

    vec![(Request::Commit(block), hash)]
});

static BLOCK_VERIFY_TRANSCRIPT_GENESIS_FAIL: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();

    vec![(Request::Commit(block), Err(ExpectedTranscriptError::Any))]
});

static NO_COINBASE_TRANSCRIPT: Lazy<Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>> =
    Lazy::new(|| {
        let block = block_no_transactions();

        vec![(
            Request::Commit(Arc::new(block)),
            Err(ExpectedTranscriptError::Any),
        )]
    });

static NO_COINBASE_STATE_TRANSCRIPT: Lazy<
    Vec<(zs::Request, Result<zs::Response, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block = block_no_transactions();
    let hash = block.hash();

    vec![(
        zs::Request::Block(hash.into()),
        Ok(zs::Response::Block(None)),
    )]
});

static STATE_VERIFY_TRANSCRIPT_GENESIS: Lazy<
    Vec<(zs::Request, Result<zs::Response, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let hash = block.hash();

    vec![(
        zs::Request::Block(hash.into()),
        Ok(zs::Response::Block(Some(block))),
    )]
});

#[tokio::test(flavor = "multi_thread")]
async fn verify_checkpoint_test() -> Result<(), Report> {
    verify_checkpoint(Config {
        checkpoint_sync: true,
    })
    .await?;
    verify_checkpoint(Config {
        checkpoint_sync: false,
    })
    .await?;

    Ok(())
}

/// Test that checkpoint verifies work.
///
/// Also tests the `chain::init` function.
#[spandoc::spandoc]
async fn verify_checkpoint(config: Config) -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let (state, read_state, _, _) = zs::init_test_services(&network).await;

    // Test that the chain::init function works. Most of the other tests use
    // init_from_verifiers.
    //
    // Download task panics and timeouts are propagated to the tests that use Groth16 verifiers.
    let (
        block_verifier_router,
        _transaction_verifier,
        _groth16_download_handle,
        _max_checkpoint_height,
        _prepared_blocks,
    ) = super::init_test(config.clone(), &network, state, read_state).await;

    // Add a timeout layer
    let block_verifier_router =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(block_verifier_router);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(block_verifier_router).await.unwrap();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_fail_no_coinbase_test() -> Result<(), Report> {
    verify_fail_no_coinbase().await
}

/// Test that blocks with no coinbase height are rejected by the BlockVerifierRouter
///
/// BlockVerifierRouter uses the block height to decide between the CheckpointVerifier
/// and SemanticBlockVerifier. This is the error case, where there is no height.
#[spandoc::spandoc]
async fn verify_fail_no_coinbase() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (router, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let block_verifier_router =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(router);

    let transcript = Transcript::from(NO_COINBASE_TRANSCRIPT.iter().cloned());
    transcript.check(block_verifier_router).await.unwrap();

    let transcript = Transcript::from(NO_COINBASE_STATE_TRANSCRIPT.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn round_trip_checkpoint_test() -> Result<(), Report> {
    round_trip_checkpoint().await
}

/// Test that state updates work
#[spandoc::spandoc]
async fn round_trip_checkpoint() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (block_verifier_router, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let block_verifier_router =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(block_verifier_router);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(block_verifier_router).await.unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_fail_add_block_checkpoint_test() -> Result<(), Report> {
    verify_fail_add_block_checkpoint().await
}

/// Test that the state rejects duplicate block adds
#[spandoc::spandoc]
async fn verify_fail_add_block_checkpoint() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (block_verifier_router, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let block_verifier_router =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(block_verifier_router);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript
        .check(block_verifier_router.clone())
        .await
        .unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service.clone()).await.unwrap();

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS_FAIL.iter().cloned());
    transcript
        .check(block_verifier_router.clone())
        .await
        .unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service.clone()).await.unwrap();

    Ok(())
}

/// Local submissions retain checkpoint validation and notify only after the real state commit.
#[tokio::test(flavor = "multi_thread")]
async fn mined_checkpoint_notifications_follow_commit() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let (router, state) = verifiers_from_network(Network::Mainnet).await;
        let genesis: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
            .zcash_deserialize_into()
            .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, validation) = tokio::sync::watch::channel(None);
        let notifications = zs::MinedBlockNotifications::new(committed.clone(), validated);

        let mut invalid = (*genesis).clone();
        invalid.transactions.push(invalid.transactions[0].clone());
        assert!(router
            .clone()
            .oneshot(Request::CommitDecoded(
                Arc::new(invalid).into(),
                Some(notifications.clone()),
            ))
            .await
            .is_err());
        assert!(commits.try_recv().is_err());
        assert!(validation.borrow().is_none());

        // Capacity is checked by the real writer before mutation; the checkpoint can be retried.
        let occupied = (block::Hash([0; 32]), Height(0));
        committed.try_send(occupied).unwrap();
        let error = router
            .clone()
            .oneshot(Request::CommitMined(genesis.clone(), notifications.clone()))
            .await
            .unwrap_err();
        let error = error
            .downcast_ref::<RouterError>()
            .expect("the router returns its production error type");
        assert!(!error.is_duplicate_request());
        assert_eq!(error.misbehavior_score(), 0);
        assert_eq!(
            state
                .clone()
                .oneshot(zs::Request::Block(genesis.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(None),
        );
        assert_eq!(commits.try_recv().unwrap(), occupied);
        assert!(validation.borrow().is_none());

        assert_eq!(
            router
                .clone()
                .oneshot(Request::CommitDecoded(
                    genesis.clone().into(),
                    Some(notifications.clone()),
                ))
                .await
                .unwrap(),
            genesis.hash(),
        );
        assert_eq!(
            commits
                .try_recv()
                .expect("the writer notifies before replying"),
            (genesis.hash(), Height(0)),
        );
        assert!(validation.borrow().is_none());
        assert_eq!(
            state
                .oneshot(zs::Request::Block(genesis.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(Some(genesis.clone())),
        );

        assert!(router
            .oneshot(Request::CommitMined(genesis, notifications))
            .await
            .unwrap_err()
            .downcast_ref::<RouterError>()
            .expect("the router returns its production error type")
            .is_duplicate_request());
        assert!(commits.try_recv().is_err());
        assert!(validation.borrow().is_none());
    })
    .await
    .expect("checkpointed local submissions must not stall");
}
