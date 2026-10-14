//! Tests for checkpoint-based block verification

#![allow(clippy::unwrap_in_result)]

use std::{cmp::min, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::{
    stream::{FuturesUnordered, StreamExt},
    TryFutureExt,
};
use tokio::time::timeout;
use tracing_futures::Instrument;

use zebra_chain::{parameters::Network::*, serialization::ZcashDeserialize};
use zebra_test::mock_service::MockService;

use super::*;

mod retention;

/// The timeout we apply to each verify future during testing.
///
/// The checkpoint verifier uses `tokio::sync::oneshot` channels as futures.
/// If the verifier doesn't send a message on the channel, any tests that
/// await the channel future will hang.
///
/// This value is set to a large value, to avoid spurious failures due to
/// high system load.
const VERIFY_TIMEOUT_SECONDS: u64 = 10;

#[tokio::test(flavor = "multi_thread")]
async fn single_item_checkpoint_list_test() -> Result<(), Report> {
    single_item_checkpoint_list().await
}

#[spandoc::spandoc]
async fn single_item_checkpoint_list() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block0 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash0 = block0.hash();

    // Make a checkpoint list containing only the genesis block
    let genesis_checkpoint_list: BTreeMap<block::Height, block::Hash> =
        [(block0.coinbase_height().unwrap(), hash0)]
            .iter()
            .cloned()
            .collect();

    let state_service = zebra_state::init_test(&Mainnet).await;
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(genesis_checkpoint_list, &Mainnet, None, state_service)
            .map_err(|e| eyre!(e))?;

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for block 0
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block0.clone()),
    );
    /// SPANDOC: Wait for the response for block 0
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash0);

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        FinishedVerifying
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn multi_item_checkpoint_list_test() -> Result<(), Report> {
    multi_item_checkpoint_list().await
}

#[spandoc::spandoc]
async fn multi_item_checkpoint_list() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Parse all the blocks
    let mut checkpoint_data = Vec::new();
    for b in &[
        // This list is used as a checkpoint list, and as a list of blocks to
        // verify. So it must be continuous.
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // Make a checkpoint list containing all the blocks
    let checkpoint_list: BTreeMap<block::Height, block::Hash> = checkpoint_data
        .iter()
        .map(|(_block, height, hash)| (*height, *hash))
        .collect();

    let state_service = zebra_state::init_test(&Mainnet).await;
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(checkpoint_list, &Mainnet, None, state_service)
            .map_err(|e| eyre!(e))?;

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(1)
    );

    // Now verify each block
    for (block, height, hash) in checkpoint_data {
        /// SPANDOC: Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;

        /// SPANDOC: Set up the future for block {?height}
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block.clone()),
        );
        /// SPANDOC: Wait for the response for block {?height}
        // TODO(teor || jlusby): check error kind
        let verify_response = verify_future
            .map_err(|e| eyre!(e))
            .await
            .expect("timeout should not happen")
            .expect("future should succeed");

        assert_eq!(verify_response, hash);

        if height < checkpoint_verifier.checkpoint_list.max_height() {
            assert_eq!(
                checkpoint_verifier.previous_checkpoint_height(),
                PreviousCheckpoint(height)
            );
            assert_eq!(
                checkpoint_verifier.target_checkpoint_height(),
                WaitingForBlocks
            );
        } else {
            assert_eq!(
                checkpoint_verifier.previous_checkpoint_height(),
                FinalCheckpoint
            );
            assert_eq!(
                checkpoint_verifier.target_checkpoint_height(),
                FinishedVerifying
            );
        }
        assert_eq!(
            checkpoint_verifier.checkpoint_list.max_height(),
            block::Height(1)
        );
    }

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        FinishedVerifying
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(1)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn continuous_blockchain_no_restart() -> Result<(), Report> {
    for network in Network::iter() {
        continuous_blockchain(None, network).await?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn continuous_blockchain_restart() -> Result<(), Report> {
    for height in 0..zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.len() {
        continuous_blockchain(Some(block::Height(height.try_into().unwrap())), Mainnet).await?;
    }
    for height in 0..zebra_test::vectors::CONTINUOUS_TESTNET_BLOCKS.len() {
        continuous_blockchain(
            Some(block::Height(height.try_into().unwrap())),
            Network::new_default_testnet(),
        )
        .await?;
    }
    Ok(())
}

/// Test a continuous blockchain on `network`, restarting verification at `restart_height`.
//
// This span is far too verbose for use during normal testing.
// Turn the SPANDOC: comments into doc comments to re-enable.
//#[spandoc::spandoc]
async fn continuous_blockchain(
    restart_height: Option<block::Height>,
    network: Network,
) -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // A continuous blockchain
    let blockchain = network.blockchain_iter();

    let blockchain: Vec<_> = blockchain
        .map(|(height, b)| {
            let block = Arc::<Block>::zcash_deserialize(*b).unwrap();
            let hash = block.hash();
            let coinbase_height = block.coinbase_height().unwrap();
            assert_eq!(*height, coinbase_height.0);
            (block, coinbase_height, hash)
        })
        .collect();
    let blockchain_len = blockchain.len();

    // Use some of the blocks as checkpoints
    // We use these indexes so that we test:
    //   - checkpoints don't have to be the same length
    //   - checkpoints start at genesis
    //   - checkpoints end at the end of the range (there's no point in having extra blocks)
    let expected_max_height = block::Height((blockchain_len - 1).try_into().unwrap());
    let checkpoint_list = [
        &blockchain[0],
        &blockchain[blockchain_len / 3],
        &blockchain[blockchain_len / 2],
        &blockchain[blockchain_len - 1],
    ];
    let checkpoint_list: BTreeMap<block::Height, block::Hash> = checkpoint_list
        .iter()
        .map(|(_block, height, hash)| (*height, *hash))
        .collect();

    // SPANDOC: Verify blocks, restarting at {?restart_height} {?network}
    {
        let initial_tip = restart_height.map(|block::Height(height)| {
            (blockchain[height as usize].1, blockchain[height as usize].2)
        });
        let state_service = zebra_state::init_test(&Mainnet).await;
        let mut checkpoint_verifier = CheckpointVerifier::from_list(
            checkpoint_list,
            &network,
            initial_tip,
            state_service.clone(),
        )
        .map_err(|e| eyre!(e))?;

        // Setup checks
        if restart_height.is_some() {
            assert!(
                restart_height <= Some(checkpoint_verifier.checkpoint_list.max_height()),
                "restart heights after the final checkpoint are not supported by this test"
            );
        }
        if restart_height
            .map(|h| h == checkpoint_verifier.checkpoint_list.max_height())
            .unwrap_or(false)
        {
            assert_eq!(
                checkpoint_verifier.previous_checkpoint_height(),
                FinalCheckpoint
            );
            assert_eq!(
                checkpoint_verifier.target_checkpoint_height(),
                FinishedVerifying
            );
        } else {
            assert_eq!(
                checkpoint_verifier.previous_checkpoint_height(),
                restart_height.map(InitialTip).unwrap_or(BeforeGenesis)
            );
            assert_eq!(
                checkpoint_verifier.target_checkpoint_height(),
                WaitingForBlocks
            );
        }
        assert_eq!(
            checkpoint_verifier.checkpoint_list.max_height(),
            expected_max_height
        );

        let mut handles = FuturesUnordered::new();

        // Now verify each block
        for (block, height, _hash) in blockchain {
            // Commit directly to the state until after the (fake) restart height
            if let Some(restart_height) = restart_height {
                if height <= restart_height {
                    let mut state_service = state_service.clone();
                    // SPANDOC: Make sure the state service is ready for block {?height}
                    let ready_state_service = state_service.ready().map_err(|e| eyre!(e)).await?;

                    // SPANDOC: Add block directly to the state {?height}
                    ready_state_service
                        .call(zebra_state::Request::CommitCheckpointVerifiedBlock(
                            block.clone().into(),
                        ))
                        .await
                        .map_err(|e| eyre!(e))?;

                    // Skip verification for (fake) previous blocks
                    continue;
                }
            }

            // SPANDOC: Make sure the verifier service is ready for block {?height}
            let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;

            // SPANDOC: Set up the future for block {?height}
            let verify_future = timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                ready_verifier_service.call(block.clone()),
            );

            // SPANDOC: spawn verification future in the background for block {?height}
            let handle = tokio::spawn(verify_future.in_current_span());
            handles.push(handle);

            // Execution checks
            if height < checkpoint_verifier.checkpoint_list.max_height() {
                assert_eq!(
                    checkpoint_verifier.target_checkpoint_height(),
                    WaitingForBlocks
                );
            } else {
                assert_eq!(
                    checkpoint_verifier.previous_checkpoint_height(),
                    FinalCheckpoint
                );
                assert_eq!(
                    checkpoint_verifier.target_checkpoint_height(),
                    FinishedVerifying
                );
            }
        }

        // Check that we have the correct number of verify tasks
        if let Some(block::Height(restart_height)) = restart_height {
            let restart_height = restart_height as usize;
            if restart_height == blockchain_len - 1 {
                assert_eq!(
                    handles.len(),
                    0,
                    "unexpected number of verify tasks for restart height: {restart_height:?}",
                );
            } else {
                assert_eq!(
                    handles.len(),
                    blockchain_len - restart_height - 1,
                    "unexpected number of verify tasks for restart height: {restart_height:?}",
                );
            }
        } else {
            assert_eq!(
                handles.len(),
                blockchain_len,
                "unexpected number of verify tasks with no restart height",
            );
        }

        // SPANDOC: wait on spawned verification tasks for restart height {?restart_height} {?network}
        while let Some(result) = handles.next().await {
            result??.map_err(|e| eyre!(e))?;
        }

        // Final checks
        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            FinalCheckpoint,
            "unexpected previous checkpoint for restart height: {restart_height:?}",
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            FinishedVerifying,
            "unexpected target checkpoint for restart height: {restart_height:?}",
        );
        assert_eq!(
            checkpoint_verifier.checkpoint_list.max_height(),
            expected_max_height,
            "unexpected max checkpoint height for restart height: {restart_height:?}",
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn block_higher_than_max_checkpoint_fail_test() -> Result<(), Report> {
    block_higher_than_max_checkpoint_fail().await
}

#[spandoc::spandoc]
async fn block_higher_than_max_checkpoint_fail() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block0 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let block415000 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

    // Make a checkpoint list containing only the genesis block
    let genesis_checkpoint_list: BTreeMap<block::Height, block::Hash> =
        [(block0.coinbase_height().unwrap(), block0.as_ref().into())]
            .iter()
            .cloned()
            .collect();

    let state_service = zebra_state::init_test(&Mainnet).await;
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(genesis_checkpoint_list, &Mainnet, None, state_service)
            .map_err(|e| eyre!(e))?;

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for block 415000
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block415000.clone()),
    );
    /// SPANDOC: Wait for the response for block 415000, and expect failure
    // TODO(teor || jlusby): check error kind
    let _ = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect_err("bad block hash should fail");

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_checkpoint_hash_fail_test() -> Result<(), Report> {
    wrong_checkpoint_hash_fail().await
}

#[spandoc::spandoc]
async fn wrong_checkpoint_hash_fail() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let good_block0 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let good_block0_hash = good_block0.hash();

    // Change the header hash
    let mut bad_block0 = good_block0.clone();
    let bad_block0_mut = Arc::make_mut(&mut bad_block0);
    Arc::make_mut(&mut bad_block0_mut.header).version = 5;

    // Make a checkpoint list containing the genesis block checkpoint
    let genesis_checkpoint_list: BTreeMap<block::Height, block::Hash> =
        [(good_block0.coinbase_height().unwrap(), good_block0_hash)]
            .iter()
            .cloned()
            .collect();

    let state_service = zebra_state::init_test(&Mainnet).await;
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(genesis_checkpoint_list, &Mainnet, None, state_service)
            .map_err(|e| eyre!(e))?;

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Make sure the verifier service is ready (1/3)
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for bad block 0 (1/3)
    // TODO(teor || jlusby): check error kind
    let bad_verify_future_1 = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(bad_block0.clone()),
    );
    // We can't await the future yet, because bad blocks aren't cleared
    // until the chain is verified

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Make sure the verifier service is ready (2/3)
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for bad block 0 again (2/3)
    // TODO(teor || jlusby): check error kind
    let bad_verify_future_2 = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(bad_block0.clone()),
    );
    // We can't await the future yet, because bad blocks aren't cleared
    // until the chain is verified

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Make sure the verifier service is ready (3/3)
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for good block 0 (3/3)
    let good_verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(good_block0.clone()),
    );
    /// SPANDOC: Wait for the response for good block 0, and expect success (3/3)
    // TODO(teor || jlusby): check error kind
    let verify_response = good_verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("future should succeed");

    assert_eq!(verify_response, good_block0_hash);

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        FinishedVerifying
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    // Now, await the bad futures, which should have completed

    /// SPANDOC: Wait for the response for block 0, and expect failure (1/3)
    // TODO(teor || jlusby): check error kind
    let _ = bad_verify_future_1
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect_err("bad block hash should fail");

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        FinishedVerifying
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    /// SPANDOC: Wait for the response for block 0, and expect failure again (2/3)
    // TODO(teor || jlusby): check error kind
    let _ = bad_verify_future_2
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect_err("bad block hash should fail");

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        FinishedVerifying
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(0)
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn checkpoint_drop_cancel_test() -> Result<(), Report> {
    checkpoint_drop_cancel().await
}

#[spandoc::spandoc]
async fn checkpoint_drop_cancel() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Parse all the blocks
    let mut checkpoint_data = Vec::new();
    for b in &[
        // Continuous blocks are verified
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        // Other blocks can't verify, so they are rejected on drop
        &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        checkpoint_data.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    // Make a checkpoint list containing all the blocks
    let checkpoint_list: BTreeMap<block::Height, block::Hash> = checkpoint_data
        .iter()
        .map(|(_block, height, hash)| (*height, *hash))
        .collect();

    let state_service = zebra_state::init_test(&Mainnet).await;
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(checkpoint_list, &Mainnet, None, state_service)
            .map_err(|e| eyre!(e))?;

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert_eq!(
        checkpoint_verifier.checkpoint_list.max_height(),
        block::Height(434873)
    );

    let mut futures = Vec::new();
    // Now collect verify futures for each block
    for (block, height, hash) in checkpoint_data {
        /// SPANDOC: Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;

        /// SPANDOC: Set up the future for block {?height}
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block.clone()),
        );

        futures.push((verify_future, height, hash));

        // Only continuous checkpoints verify
        assert_eq!(
            checkpoint_verifier.previous_checkpoint_height(),
            PreviousCheckpoint(block::Height(min(height.0, 1)))
        );
        assert_eq!(
            checkpoint_verifier.target_checkpoint_height(),
            WaitingForBlocks
        );
        assert_eq!(
            checkpoint_verifier.checkpoint_list.max_height(),
            block::Height(434873)
        );
    }

    // Now drop the verifier, to cancel the futures
    drop(checkpoint_verifier);

    for (verify_future, height, hash) in futures {
        /// SPANDOC: Check the response for block {?height}
        let verify_response = verify_future
            .map_err(|e| eyre!(e))
            .await
            .expect("timeout should not happen");

        if height <= block::Height(1) {
            let verify_hash =
                verify_response.expect("Continuous checkpoints should have succeeded before drop");
            assert_eq!(verify_hash, hash);
        } else {
            // TODO(teor || jlusby): check error kind
            verify_response.expect_err("Pending futures should fail on drop");
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn hard_coded_mainnet_test() -> Result<(), Report> {
    hard_coded_mainnet().await
}

#[spandoc::spandoc]
async fn hard_coded_mainnet() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block0 =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let hash0 = block0.hash();

    let state_service = zebra_state::init_test(&Mainnet).await;
    // Use the hard-coded checkpoint list
    let mut checkpoint_verifier = CheckpointVerifier::new(&Network::Mainnet, None, state_service);

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        BeforeGenesis
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    assert!(checkpoint_verifier.checkpoint_list.max_height() > block::Height(0));

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = checkpoint_verifier.ready().map_err(|e| eyre!(e)).await?;
    /// SPANDOC: Set up the future for block 0
    let verify_future = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        ready_verifier_service.call(block0.clone()),
    );
    /// SPANDOC: Wait for the response for block 0
    // TODO(teor || jlusby): check error kind
    let verify_response = verify_future
        .map_err(|e| eyre!(e))
        .await
        .expect("timeout should not happen")
        .expect("block should verify");

    assert_eq!(verify_response, hash0);

    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        PreviousCheckpoint(block::Height(0))
    );
    assert_eq!(
        checkpoint_verifier.target_checkpoint_height(),
        WaitingForBlocks
    );
    // The lists will get bigger over time, so we just pick a recent height
    assert!(checkpoint_verifier.checkpoint_list.max_height() > block::Height(900_000));

    Ok(())
}

/// Duplicate block errors must stay classified as duplicate requests after the
/// state wraps them, so they don't restart the syncer during checkpoint sync.
#[test]
fn state_commit_duplicate_errors_are_duplicate_requests() {
    let duplicate = zs::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zs::KnownBlock::Finalized,
    };

    // Box the error the same way the state's `CommitCheckpointVerifiedBlock`
    // handler does. This mirrors the wrapping manually, so it won't fail
    // automatically if the state changes its error type — keep it in sync by hand.
    let source: BoxError = Box::new(zs::CommitCheckpointVerifiedError::from(duplicate));

    let err = VerifyCheckpointError::CommitCheckpointVerified(source);

    assert!(err.is_duplicate_request());
    assert_eq!(err.misbehavior_score(), 0);
}

/// Checkpoint verification only checks the block hash, which doesn't commit to a
/// block's authorizing data, so a same-hash forged body reaches contextual
/// validation in the finalized state. Check the state's suggested misbehaviour
/// score survives the boxed wrapper and reaches the syncer.
#[test]
fn state_auth_commitment_errors_score_the_serving_peer() {
    let commitment_error =
        zebra_chain::block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
            expected: [1; 32],
            actual: [2; 32],
        };
    let commit_error = zs::CommitBlockError::ValidateContextError(Box::new(
        zs::ValidateContextError::InvalidBlockCommitment(commitment_error),
    ));

    // Box the error the same way the state's `CommitCheckpointVerifiedBlock`
    // handler does. This mirrors the wrapping manually, so it won't fail
    // automatically if the state changes its error type — keep it in sync by hand.
    let source: BoxError = Box::new(zs::CommitCheckpointVerifiedError::from(commit_error));

    let err = VerifyCheckpointError::CommitCheckpointVerified(source);

    assert!(
        !err.is_duplicate_request(),
        "a forged body is not a duplicate request, so it must not be treated as benign"
    );
    assert_eq!(
        err.misbehavior_score(),
        100,
        "an authorizing-data commitment mismatch must score the peer that served the body"
    );
}

/// A checkpoint block that fails to commit must reset the verifier to the state
/// tip, even if the caller dropped its verify future before the commit failed.
///
/// The syncer drops in-flight verify futures when it cancels its downloads.
/// If the reset depended on that future, the verifier would stay ahead of the
/// state, and reject the honest block as `Finished`.
#[tokio::test(flavor = "multi_thread")]
async fn commit_failure_resets_progress_after_dropped_future() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let genesis =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let block_1 = Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
    let genesis_tip = (block::Height(0), genesis.hash());
    let block_1_hash = block_1.hash();

    let mut state_service = MockService::build().for_unit_tests();
    let mut checkpoint_verifier = CheckpointVerifier::from_list(
        [genesis_tip, (block::Height(1), block_1_hash)],
        &Mainnet,
        Some(genesis_tip),
        state_service.clone(),
    )
    .map_err(|e| eyre!(e))?;

    // Verify the final checkpoint, then drop the verify future before it commits.
    let verify_future = checkpoint_verifier
        .ready()
        .await
        .map_err(|e| eyre!(e))?
        .call(block_1.clone());
    drop(verify_future);
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );

    // The state rejects the block, and its tip is still the genesis block.
    state_service
        .expect_request_that(|request| {
            matches!(
                request,
                zs::Request::CommitCheckpointVerifiedBlock(block) if block.hash == block_1_hash
            )
        })
        .await
        .respond_error("forged block body".into());
    state_service
        .expect_request(zs::Request::Tip)
        .await
        .respond(zs::Response::Tip(Some(genesis_tip)));

    // Once the reset arrives, the verifier accepts the block again.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(VERIFY_TIMEOUT_SECONDS);
    let mut verify_future = loop {
        let mut verify_future = checkpoint_verifier
            .ready()
            .await
            .map_err(|e| eyre!(e))?
            .call(block_1.clone());

        match (&mut verify_future).now_or_never() {
            Some(Err(VerifyCheckpointError::Finished)) => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "the verifier was not reset to the state tip"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Some(result) => panic!("unexpected verify result before commit: {result:?}"),
            None => break verify_future,
        }
    };

    state_service
        .expect_request_that(|request| {
            matches!(
                request,
                zs::Request::CommitCheckpointVerifiedBlock(block) if block.hash == block_1_hash
            )
        })
        .await
        .respond(zs::Response::Committed(block_1_hash));

    let verified_hash = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        &mut verify_future,
    )
    .await??;
    assert_eq!(verified_hash, block_1_hash);
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );

    Ok(())
}

/// When a checkpoint block fails to commit, its children in the same range are
/// queued again, so only the invalid block needs to be submitted again.
///
/// A peer can serve a body with the right hash but forged authorizing data.
/// The state rejects it, and drops its children, even if their bodies are valid.
#[tokio::test(flavor = "multi_thread")]
async fn commit_failure_requeues_children_of_invalid_block() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let genesis =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let block_1 = Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
    let block_2 = Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..])?;
    let genesis_tip = (block::Height(0), genesis.hash());
    let block_1_hash = block_1.hash();
    let block_2_hash = block_2.hash();

    let mut state_service = MockService::build().for_unit_tests();
    let mut checkpoint_verifier = CheckpointVerifier::from_list(
        [genesis_tip, (block::Height(2), block_2_hash)],
        &Mainnet,
        Some(genesis_tip),
        state_service.clone(),
    )
    .map_err(|e| eyre!(e))?;

    // Verify the range, and drop the verify futures like a cancelled syncer.
    for block in [&block_1, &block_2] {
        drop(
            checkpoint_verifier
                .ready()
                .await
                .map_err(|e| eyre!(e))?
                .call(block.clone()),
        );
    }
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );

    // The state rejects block 1, so it drops block 2. The failure responses
    // and tip requests can arrive in any order.
    for _ in 0..4 {
        let responder = state_service.expect_request_that(|_| true).await;
        match responder.request().clone() {
            zs::Request::CommitCheckpointVerifiedBlock(block) if block.hash == block_1_hash => {
                responder.respond_error("forged block body".into())
            }
            // The state drops the children of a block that failed to commit.
            zs::Request::CommitCheckpointVerifiedBlock(_) => {
                responder.respond_error(Box::new(zs::CommitCheckpointVerifiedError::from(
                    zs::CommitBlockError::new_write_task_exited(),
                )))
            }
            zs::Request::Tip => responder.respond(zs::Response::Tip(Some(genesis_tip))),
            request => panic!("unexpected state request: {request:?}"),
        }
    }

    // Wait for both failures, then put them back for the verifier to apply.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(VERIFY_TIMEOUT_SECONDS);
    let mut failures = Vec::new();
    while failures.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "commit tasks did not report both failures"
        );
        match checkpoint_verifier.reset_receiver.try_recv() {
            Ok(failure) => failures.push(failure),
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    // The child is sent back with all its precalculated data, so it doesn't
    // need to be checked again.
    let checked_block_2 = zs::CheckpointVerifiedBlock::new(block_2.clone(), None);
    assert!(
        !checked_block_2.new_outputs.is_empty(),
        "block 2 has outputs, so this test checks they are rebuilt"
    );
    let children: Vec<_> = failures
        .iter()
        .map(|failure| {
            assert_eq!(failure.tip, Some(genesis_tip));
            failure.child.as_ref().map(|child| &child.block)
        })
        .collect();
    assert!(
        children.contains(&None),
        "the invalid block is not queued again"
    );
    assert!(
        children.contains(&Some(&checked_block_2)),
        "its child is queued again"
    );
    for failure in failures {
        checkpoint_verifier
            .reset_sender
            .send(failure)
            .expect("the verifier holds the receiver");
    }

    // Submitting only an honest block 1 completes the range again.
    let mut verify_future = checkpoint_verifier
        .ready()
        .await
        .map_err(|e| eyre!(e))?
        .call(block_1.clone());
    assert!(
        (&mut verify_future).now_or_never().is_none(),
        "block 1 is verified, and waiting for its commit"
    );
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );

    // The blocks are sent to the state in height order.
    for hash in [block_1_hash, block_2_hash] {
        state_service
            .expect_request_that(|request| {
                matches!(
                    request,
                    zs::Request::CommitCheckpointVerifiedBlock(block) if block.hash == hash
                )
            })
            .await
            .respond(zs::Response::Committed(hash));
    }

    let verified_hash = timeout(
        Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
        &mut verify_future,
    )
    .await??;
    assert_eq!(verified_hash, block_1_hash);

    // A late failure from the first epoch doesn't rewind the verifier,
    // and its child isn't queued below the verifier's progress.
    let (tx, mut late_child_rx) = tokio::sync::oneshot::channel();
    checkpoint_verifier
        .reset_sender
        .send(CommitFailure {
            epoch: 0,
            tip: Some(genesis_tip),
            child: Some(QueuedBlock {
                block: checked_block_2,
                tx,
                retained: RetainedBlockPermit(Arc::new(AtomicUsize::new(1))),
            }),
        })
        .expect("the verifier holds the receiver");
    // If the verifier was rewound, it would queue the block, and wait for its commit.
    let result = checkpoint_verifier
        .ready()
        .await
        .map_err(|e| eyre!(e))?
        .call(block_1.clone())
        .now_or_never();
    assert!(
        matches!(result, Some(Err(VerifyCheckpointError::Finished))),
        "unexpected result: {result:?}"
    );
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );
    assert!(
        checkpoint_verifier.queued.is_empty(),
        "the late child is dropped"
    );
    assert!(
        matches!(
            late_child_rx.try_recv(),
            Ok(Err(VerifyCheckpointError::Finished))
        ),
        "the late child's commit task gets an error"
    );

    Ok(())
}
