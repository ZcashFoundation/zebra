//! Tests for chain verification

use super::*;

use crate::checkpoint::CheckpointList;

use color_eyre::eyre::Report;
use color_eyre::eyre::{bail, eyre};
use futures::{future::TryFutureExt, stream::FuturesUnordered};
use std::{collections::BTreeMap, mem::drop, sync::Arc, time::Duration};
use tokio::{stream::StreamExt, time::timeout};
use tower::{Service, ServiceExt};
use tracing_futures::Instrument;

use zebra_chain::block::{Block, BlockHeader};
use zebra_chain::serialization::ZcashDeserialize;
use zebra_chain::Network::{self, *};

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
        header: BlockHeader::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap(),
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
            Response = BlockHeaderHash,
            Error = Error,
            Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
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
            Response = BlockHeaderHash,
            Error = Error,
            Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
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
    let hash0: BlockHeaderHash = block0.as_ref().into();
    checkpoint_data.push((
        block0.coinbase_height().expect("test block has height"),
        hash0,
    ));

    // Make a checkpoint list containing the genesis block
    let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
        checkpoint_data.iter().cloned().collect();
    let checkpoint_list = CheckpointList::from_list(checkpoint_list).map_err(|e| eyre!(e))?;

    let (mut chain_verifier, _) = verifiers_from_checkpoint_list(Mainnet, checkpoint_list);

    /// SPANDOC: Make sure the verifier service is ready for block 0
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future for block 0
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block0.clone()),
    );
    /// SPANDOC: Verify block 0
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash0);

    let block1 = Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
    let hash1: BlockHeaderHash = block1.as_ref().into();

    /// SPANDOC: Make sure the verifier service is ready for block 1
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future for block 1
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block1.clone()),
    );
    /// SPANDOC: Verify block 1
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash1);

    Ok(())
}

#[tokio::test]
async fn verify_checkpoint_test() -> Result<(), Report> {
    verify_checkpoint().await
}

/// Test that checkpoint verifies work.
///
/// Also tests the `chain::init` function.
#[spandoc::spandoc]
async fn verify_checkpoint() -> Result<(), Report> {
    zebra_test::init();

    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash: BlockHeaderHash = block.as_ref().into();

    // Test that the chain::init function works. Most of the other tests use
    // init_from_verifiers.
    let mut chain_verifier = super::init(Mainnet, zebra_state::in_memory::init()).await;

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block.clone()),
    );
    /// SPANDOC: Verify the block
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash);

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

    let block = block_no_transactions();
    let hash: BlockHeaderHash = (&block).into();

    let (mut chain_verifier, mut state_service) = verifiers_from_network(Mainnet);

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future to verify the block
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block.into()),
    );
    /// SPANDOC: Verify the block
    // TODO(teor || jlusby): check error kind
    let _ = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .unwrap_err();

    /// SPANDOC: Make sure the state service is ready
    let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: The state should not contain failed blocks
    let _ = ready_state_service
        .call(zebra_state::Request::GetBlock { hash })
        .await
        .expect_err("failed block should not be in state");

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

    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash: BlockHeaderHash = block.as_ref().into();

    let (mut chain_verifier, mut state_service) = verifiers_from_network(Mainnet);

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block.clone()),
    );
    /// SPANDOC: Verify the block
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash);

    /// SPANDOC: Make sure the state service is ready
    let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Make sure the block was added to the state
    let state_response = ready_state_service
        .call(zebra_state::Request::GetBlock { hash })
        .await
        .map_err(|e| eyre!(e))?;

    if let zebra_state::Response::Block {
        block: returned_block,
    } = state_response
    {
        assert_eq!(block, returned_block);
    } else {
        bail!("unexpected response kind: {:?}", state_response);
    }

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

    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash: BlockHeaderHash = block.as_ref().into();

    let (mut chain_verifier, mut state_service) = verifiers_from_network(Mainnet);

    /// SPANDOC: Make sure the verifier service is ready (1/2)
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future to verify the block for the first time
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block.clone()),
    );
    /// SPANDOC: Verify the block for the first time
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash);

    /// SPANDOC: Make sure the state service is ready (1/2)
    let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Make sure the block was added to the state
    let state_response = ready_state_service
        .call(zebra_state::Request::GetBlock { hash })
        .await
        .map_err(|e| eyre!(e))?;

    if let zebra_state::Response::Block {
        block: returned_block,
    } = state_response
    {
        assert_eq!(block, returned_block);
    } else {
        bail!("unexpected response kind: {:?}", state_response);
    }

    /// SPANDOC: Make sure the verifier service is ready (2/2)
    let ready_verifier_service = chain_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Set up the future to verify the block for the first time
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block.clone()),
    );
    /// SPANDOC: Verify the block for the first time
    // TODO(teor): ignore duplicate block verifies?
    // TODO(teor || jlusby): check error kind
    let _ = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .unwrap_err();

    /// SPANDOC: Make sure the state service is ready (2/2)
    let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: But the state should still return the original block we added
    let state_response = ready_state_service
        .call(zebra_state::Request::GetBlock { hash })
        .await
        .map_err(|e| eyre!(e))?;

    if let zebra_state::Response::Block {
        block: returned_block,
    } = state_response
    {
        assert_eq!(block, returned_block);
    } else {
        bail!("unexpected response kind: {:?}", state_response);
    }

    Ok(())
}

#[tokio::test]
async fn continuous_blockchain_test() -> Result<(), Report> {
    continuous_blockchain(None).await?;
    for height in 0..=10 {
        continuous_blockchain(Some(BlockHeight(height))).await?;
    }
    Ok(())
}

/// Test a continuous blockchain in the BlockVerifier and CheckpointVerifier,
/// restarting verification at `restart_height`.
#[spandoc::spandoc]
async fn continuous_blockchain(restart_height: Option<BlockHeight>) -> Result<(), Report> {
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
        let hash: BlockHeaderHash = block.as_ref().into();
        blockchain.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // Parse only some blocks as checkpoints
    let mut checkpoints = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash: BlockHeaderHash = block.as_ref().into();
        checkpoints.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // The checkpoint list will contain only blocks 0 and 4
    let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> = checkpoints
        .iter()
        .map(|(_block, height, hash)| (*height, *hash))
        .collect();
    let checkpoint_list = CheckpointList::from_list(checkpoint_list).map_err(|e| eyre!(e))?;

    let mut state_service = zebra_state::in_memory::init();
    /// SPANDOC: Add blocks from 0..={?restart_height} to the state
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
        .map(|BlockHeight(height)| &blockchain[height as usize].0)
        .cloned();

    let block_verifier = crate::block::init(state_service.clone());
    let mut chain_verifier = super::init_from_verifiers(
        Mainnet,
        block_verifier,
        Some(checkpoint_list),
        state_service.clone(),
        initial_tip,
    );

    let mut handles = FuturesUnordered::new();

    /// SPANDOC: Verify blocks, restarting at {?restart_height}
    for (block, height, _hash) in blockchain
        .iter()
        .filter(|(_, height, _)| restart_height.map_or(true, |rh| *height > rh))
    {
        /// SPANDOC: Make sure the verifier service is ready for block {?height}
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
