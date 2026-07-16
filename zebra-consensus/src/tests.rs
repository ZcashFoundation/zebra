//! Tests for the block verification stack built by [`init`](crate::init).

#![allow(clippy::unwrap_in_result)]

use std::{future::Future, sync::Arc, time::Duration};

use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use tower::{layer::Layer, timeout::TimeoutLayer, Service};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
};
use zebra_state as zs;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{BoxError, Config, Request};

/// The timeout we apply to each verify future during testing.
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

/// Return a new block verifier stack and state service for `network`.
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
    let (block_verifier, _transaction_verifier, _background_handles, _max_checkpoint_height) =
        crate::init_test(Config::default(), &network, state_service.clone()).await;

    // We can drop the background task handles here, because:
    // - if a background task fails, the tests will panic, and
    // - if a background task hangs, the tests will hang.

    (block_verifier, state_service)
}

static BLOCK_VERIFY_TRANSCRIPT_GENESIS_BELOW_CHECKPOINT: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();

    // Genesis is below the mandatory checkpoint height, so the checkpoint
    // gate rejects it: blocks in that range are only committed by
    // checkpoint-verified sync (the known-hash IBD engine).
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

static STATE_VERIFY_TRANSCRIPT_GENESIS_MISSING: Lazy<
    Vec<(zs::Request, Result<zs::Response, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let block: Arc<Block> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let hash = block.hash();

    vec![(
        zs::Request::Block(hash.into()),
        Ok(zs::Response::Block(None)),
    )]
});

#[tokio::test(flavor = "multi_thread")]
async fn verify_below_mandatory_checkpoint_test() -> Result<(), Report> {
    verify_below_mandatory_checkpoint(Config {
        checkpoint_sync: true,
    })
    .await?;
    verify_below_mandatory_checkpoint(Config {
        checkpoint_sync: false,
    })
    .await?;

    Ok(())
}

/// Test that the checkpoint gate rejects commits at or below the mandatory
/// checkpoint height, regardless of the `checkpoint_sync` config, and that
/// the rejected block never reaches the state.
///
/// Also tests the `init` function.
#[spandoc::spandoc]
async fn verify_below_mandatory_checkpoint(config: Config) -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let state_service = zs::init_test(&network).await;

    let (block_verifier, _transaction_verifier, _background_handles, _max_checkpoint_height) =
        crate::init_test(config.clone(), &network, state_service.clone()).await;

    // Add a timeout layer
    let block_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(block_verifier);

    let transcript = Transcript::from(
        BLOCK_VERIFY_TRANSCRIPT_GENESIS_BELOW_CHECKPOINT
            .iter()
            .cloned(),
    );
    transcript.check(block_verifier).await.unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS_MISSING.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_fail_no_coinbase_test() -> Result<(), Report> {
    verify_fail_no_coinbase().await
}

/// Test that blocks with no coinbase height are rejected by the verifier
/// stack.
///
/// The checkpoint gate passes blocks without a parseable coinbase height
/// through, so the semantic verifier reports them with full context. This is
/// that error case.
#[spandoc::spandoc]
async fn verify_fail_no_coinbase() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (verifier, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let block_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(verifier);

    let transcript = Transcript::from(NO_COINBASE_TRANSCRIPT.iter().cloned());
    transcript.check(block_verifier).await.unwrap();

    let transcript = Transcript::from(NO_COINBASE_STATE_TRANSCRIPT.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}
