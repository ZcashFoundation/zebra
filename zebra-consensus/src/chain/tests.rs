//! Tests for chain verification

use std::{sync::Arc, time::Duration};

use color_eyre::eyre::Report;
use once_cell::sync::Lazy;
use tower::{layer::Layer, timeout::TimeoutLayer, Service, ServiceBuilder};

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::ZcashDeserialize,
};
use zebra_state as zs;
use zebra_test::transcript::{TransError, Transcript};

use crate::Config;

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
        header: block::Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap(),
        transactions: Vec::new(),
    }
}

/// Return a new `(chain_verifier, state_service)` using the hard-coded
/// checkpoint list for `network`.
async fn verifiers_from_network(
    network: Network,
) -> (
    impl Service<
            Arc<Block>,
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
    let state_service = ServiceBuilder::new()
        .buffer(1)
        .service(zs::init(zs::Config::ephemeral(), network));
    let chain_verifier =
        crate::chain::init(Config::default(), network, state_service.clone()).await;

    (chain_verifier, state_service)
}

static BLOCK_VERIFY_TRANSCRIPT_GENESIS: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let hash = Ok(block.hash());

        vec![(block, hash)]
    });

static BLOCK_VERIFY_TRANSCRIPT_GENESIS_FAIL: Lazy<
    Vec<(Arc<Block>, Result<block::Hash, TransError>)>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();

    vec![(block, Err(TransError::Any))]
});

static NO_COINBASE_TRANSCRIPT: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let block = block_no_transactions();

        vec![(Arc::new(block), Err(TransError::Any))]
    });

static NO_COINBASE_STATE_TRANSCRIPT: Lazy<Vec<(zs::Request, Result<zs::Response, TransError>)>> =
    Lazy::new(|| {
        let block = block_no_transactions();
        let hash = block.hash();

        vec![(
            zs::Request::Block(hash.into()),
            Ok(zs::Response::Block(None)),
        )]
    });

static STATE_VERIFY_TRANSCRIPT_GENESIS: Lazy<Vec<(zs::Request, Result<zs::Response, TransError>)>> =
    Lazy::new(|| {
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

#[tokio::test]
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
    zebra_test::init();

    let network = Network::Mainnet;

    // Test that the chain::init function works. Most of the other tests use
    // init_from_verifiers.
    let chain_verifier = super::init(
        config.clone(),
        network,
        ServiceBuilder::new()
            .buffer(1)
            .service(zs::init(zs::Config::ephemeral(), network)),
    )
    .await;

    // Add a timeout layer
    let chain_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(chain_verifier);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(chain_verifier).await.unwrap();

    Ok(())
}

#[tokio::test]
async fn verify_fail_no_coinbase_test() -> Result<(), Report> {
    verify_fail_no_coinbase().await
}

/// Test that blocks with no coinbase height are rejected by the ChainVerifier
///
/// ChainVerifier uses the block height to decide between the CheckpointVerifier
/// and BlockVerifier. This is the error case, where there is no height.
#[spandoc::spandoc]
async fn verify_fail_no_coinbase() -> Result<(), Report> {
    zebra_test::init();

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let chain_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(chain_verifier);

    let transcript = Transcript::from(NO_COINBASE_TRANSCRIPT.iter().cloned());
    transcript.check(chain_verifier).await.unwrap();

    let transcript = Transcript::from(NO_COINBASE_STATE_TRANSCRIPT.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}

#[tokio::test]
async fn round_trip_checkpoint_test() -> Result<(), Report> {
    round_trip_checkpoint().await
}

/// Test that state updates work
#[spandoc::spandoc]
async fn round_trip_checkpoint() -> Result<(), Report> {
    zebra_test::init();

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let chain_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(chain_verifier);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(chain_verifier).await.unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service).await.unwrap();

    Ok(())
}

#[tokio::test]
async fn verify_fail_add_block_checkpoint_test() -> Result<(), Report> {
    verify_fail_add_block_checkpoint().await
}

/// Test that the state rejects duplicate block adds
#[spandoc::spandoc]
async fn verify_fail_add_block_checkpoint() -> Result<(), Report> {
    zebra_test::init();

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet).await;

    // Add a timeout layer
    let chain_verifier =
        TimeoutLayer::new(Duration::from_secs(VERIFY_TIMEOUT_SECONDS)).layer(chain_verifier);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(chain_verifier.clone()).await.unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service.clone()).await.unwrap();

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS_FAIL.iter().cloned());
    transcript.check(chain_verifier.clone()).await.unwrap();

    let transcript = Transcript::from(STATE_VERIFY_TRANSCRIPT_GENESIS.iter().cloned());
    transcript.check(state_service.clone()).await.unwrap();

    Ok(())
}

/*
// This test is disabled because it doesn't test the right thing:
// the BlockVerifier and CheckpointVerifier make different requests
// and produce different transcripts.


#[tokio::test]
// Temporarily ignore this test, until the state can handle out-of-order blocks
#[ignore]
async fn continuous_blockchain_test() -> Result<(), Report> {
    continuous_blockchain(None).await?;
    for height in 0..=10 {
        continuous_blockchain(Some(block::Height(height))).await?;
    }
    Ok(())
}

/// Test a continuous blockchain in the BlockVerifier and CheckpointVerifier,
/// restarting verification at `restart_height`.
#[spandoc::spandoc]
async fn continuous_blockchain(restart_height: Option<block::Height>) -> Result<(), Report> {
    zebra_test::init();
    let network = Network::Mainnet;

    // A continuous blockchain
    let mut blockchain = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_3_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_5_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_6_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_7_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_8_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_9_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_10_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        blockchain.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // Parse only some blocks as checkpoints
    let mut checkpoints = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoints.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // The checkpoint list will contain only blocks 0 and 4
    let checkpoint_list: BTreeMap<block::Height, block::Hash> = checkpoints
        .iter()
        .map(|(_block, height, hash)| (*height, *hash))
        .collect();
    let checkpoint_list = CheckpointList::from_list(checkpoint_list).map_err(|e| eyre!(e))?;

    let mut state_service = zs::init(zs::Config::ephemeral(), network);
    /// SPANDOC: Add blocks to the state from 0..=restart_height {?restart_height}
    if restart_height.is_some() {
        for block in blockchain
            .iter()
            .take((restart_height.unwrap().0 + 1) as usize)
            .map(|(block, ..)| block)
        {
            state_service
                .ready_and()
                .map_err(|e| eyre!(e))
                .await?
                .call(zs::Request::AddBlock {
                    block: block.clone(),
                })
                .map_err(|e| eyre!(e))
                .await?;
        }
    }
    let initial_tip = restart_height
        .map(|block::Height(height)| &blockchain[height as usize].0)
        .cloned();

    let block_verifier = crate::block::init(state_service.clone());
    let mut chain_verifier = super::init_from_verifiers(
        network,
        block_verifier,
        Some(checkpoint_list),
        state_service.clone(),
        initial_tip,
    );

    let mut handles = FuturesUnordered::new();

    /// SPANDOC: Verify blocks, restarting at restart_height {?restart_height}
    for (block, height, _hash) in blockchain
        .iter()
        .filter(|(_, height, _)| restart_height.map_or(true, |rh| *height > rh))
    {
        /// SPANDOC: Make sure the verifier service is ready for the block at height {?height}
        let ready_verifier_service = chain_verifier.ready_and().map_err(|e| eyre!(e)).await?;

        /// SPANDOC: Set up the future for block {?height}
        let verify_future = timeout(
            std::time::Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block.clone()),
        );

        /// SPANDOC: spawn verification future in the background for block {?height}
        let handle = tokio::spawn(verify_future.in_current_span());
        handles.push(handle);
    }

    while let Some(result) = handles.next().await {
        result??.map_err(|e| eyre!(e))?;
    }

    Ok(())
}
*/
