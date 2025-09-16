//! Tests for chain verification

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
    impl Service<
            Request,
            Response = block::Hash,
            Error = BoxError,
            Future = impl Future<Output = Result<block::Hash, BoxError>>,
        > + Send
        + Clone
        + 'static,
    impl Service<
            zs::Request,
            Response = zs::Response,
            Error = BoxError,
            Future = impl Future<Output = Result<zs::Response, BoxError>>,
        > + Send
        + Clone
        + 'static,
) {
    let state_service = zs::init_test(&network).await;
    let (
        block_verifier_router,
        _transaction_verifier,
        _groth16_download_handle,
        _max_checkpoint_height,
    ) = crate::router::init_test(Config::default(), &network, state_service.clone()).await;

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

    // Test that the chain::init function works. Most of the other tests use
    // init_from_verifiers.
    //
    // Download task panics and timeouts are propagated to the tests that use Groth16 verifiers.
    let (
        block_verifier_router,
        _transaction_verifier,
        _groth16_download_handle,
        _max_checkpoint_height,
    ) = super::init_test(config.clone(), &network, zs::init_test(&network).await).await;

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
