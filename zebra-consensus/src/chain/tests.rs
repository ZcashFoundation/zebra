//! Tests for chain verification

use std::{collections::BTreeMap, mem::drop, sync::Arc, time::Duration};

use color_eyre::eyre::eyre;
use color_eyre::eyre::Report;
use futures::{future::TryFutureExt, stream::FuturesUnordered};
use once_cell::sync::Lazy;
use tokio::{stream::StreamExt, time::timeout};
use tower::{layer::Layer, timeout::TimeoutLayer, Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::{
    block::{self, Block},
    parameters::Network,
    serialization::ZcashDeserialize,
};
use zebra_test::transcript::{TransError, Transcript};

use crate::checkpoint::CheckpointList;
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

/// Return a new `(chain_verifier, state_service)` using `checkpoint_list`.
///
/// Also creates a new block verfier and checkpoint verifier, so it can
/// initialise the chain verifier.
fn verifiers_from_checkpoint_list(
    network: Network,
    checkpoint_list: CheckpointList,
) -> (
    impl Service<
            Arc<Block>,
            Response = block::Hash,
            Error = Error,
            Future = impl Future<Output = Result<block::Hash, Error>>,
        > + Send
        + Clone
        + 'static,
    impl Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = Error,
            Future = impl Future<Output = Result<zebra_state::Response, Error>>,
        > + Send
        + Clone
        + 'static,
) {
    let state_service = zebra_state::in_memory::init();
    let block_verifier = crate::block::init(state_service.clone());
    let chain_verifier = super::init_from_verifiers(
        network,
        block_verifier,
        Some(checkpoint_list),
        state_service.clone(),
        None,
    );

    (chain_verifier, state_service)
}

/// Return a new `(chain_verifier, state_service)` using the hard-coded
/// checkpoint list for `network`.
fn verifiers_from_network(
    network: Network,
) -> (
    impl Service<
            Arc<Block>,
            Response = block::Hash,
            Error = Error,
            Future = impl Future<Output = Result<block::Hash, Error>>,
        > + Send
        + Clone
        + 'static,
    impl Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = Error,
            Future = impl Future<Output = Result<zebra_state::Response, Error>>,
        > + Send
        + Clone
        + 'static,
) {
    verifiers_from_checkpoint_list(network, CheckpointList::new(network))
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

static BLOCK_VERIFY_TRANSCRIPT_GENESIS_TO_BLOCK_1: Lazy<
    Vec<(Arc<Block>, Result<block::Hash, TransError>)>,
> = Lazy::new(|| {
    let block0: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let hash0 = Ok(block0.hash());

    let block1: Arc<_> = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
        .unwrap()
        .into();
    let hash1 = Ok(block1.hash());

    vec![(block0, hash0), (block1, hash1)]
});

static NO_COINBASE_TRANSCRIPT: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let block = block_no_transactions();

        vec![(Arc::new(block), Err(TransError::Any))]
    });

static NO_COINBASE_STATE_TRANSCRIPT: Lazy<
    Vec<(
        zebra_state::Request,
        Result<zebra_state::Response, TransError>,
    )>,
> = Lazy::new(|| {
    let block = block_no_transactions();
    let hash = block.hash();

    vec![(
        zebra_state::Request::GetBlock { hash },
        Err(TransError::Any),
    )]
});

static STATE_VERIFY_TRANSCRIPT_GENESIS: Lazy<
    Vec<(
        zebra_state::Request,
        Result<zebra_state::Response, TransError>,
    )>,
> = Lazy::new(|| {
    let block: Arc<_> =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
            .unwrap()
            .into();
    let hash = block.hash();

    vec![(
        zebra_state::Request::GetBlock { hash },
        Ok(zebra_state::Response::Block { block }),
    )]
});

#[tokio::test]
async fn verify_block_test() -> Result<(), Report> {
    verify_block().await
}

/// Test that block verifies work
///
/// Uses a custom checkpoint list, containing only the genesis block. Since the
/// maximum checkpoint height is 0, non-genesis blocks are verified using the
/// BlockVerifier.
#[spandoc::spandoc]
async fn verify_block() -> Result<(), Report> {
    zebra_test::init();

    // Parse the genesis block
    let mut checkpoint_data = Vec::new();
    let block0 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash0 = block0.hash();
    checkpoint_data.push((
        block0.coinbase_height().expect("test block has height"),
        hash0,
    ));

    // Make a checkpoint list containing the genesis block
    let checkpoint_list: BTreeMap<block::Height, block::Hash> =
        checkpoint_data.iter().cloned().collect();
    let checkpoint_list = CheckpointList::from_list(checkpoint_list).map_err(|e| eyre!(e))?;

    let (chain_verifier, _) = verifiers_from_checkpoint_list(Network::Mainnet, checkpoint_list);

    let transcript = Transcript::from(BLOCK_VERIFY_TRANSCRIPT_GENESIS_TO_BLOCK_1.iter().cloned());
    transcript.check(chain_verifier).await.unwrap();

    Ok(())
}

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

    // Test that the chain::init function works. Most of the other tests use
    // init_from_verifiers.
    let chain_verifier = super::init(
        config.clone(),
        Network::Mainnet,
        zebra_state::in_memory::init(),
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

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet);

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

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet);

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

    let (chain_verifier, state_service) = verifiers_from_network(Network::Mainnet);

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

#[tokio::test]
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

    let mut state_service = zebra_state::in_memory::init();
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
                .call(zebra_state::Request::AddBlock {
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
        Network::Mainnet,
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
