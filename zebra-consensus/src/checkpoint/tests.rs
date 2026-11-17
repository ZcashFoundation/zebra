//! Tests for checkpoint-based block verification

#![allow(clippy::unwrap_in_result)]

use std::{cmp::min, time::Duration};

use color_eyre::eyre::{eyre, Report};
use futures::stream::{FuturesUnordered, StreamExt};
use tokio::time::timeout;
use tracing::Instrument;

use zebra_chain::{
    parameters::{Network::*, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashSerialize},
};
use zebra_test::mock_service::MockService;

use crate::{error::TransactionError, router::RouterError};

use super::*;

mod body_replay;
mod commit_tasks;
mod queue_pressure;

/// The timeout we apply to each verify future during testing.
///
/// The checkpoint verifier uses `tokio::sync::oneshot` channels as futures.
/// If the verifier doesn't send a message on the channel, any tests that
/// await the channel future will hang.
///
/// This value is set to a large value, to avoid spurious failures due to
/// high system load.
const VERIFY_TIMEOUT_SECONDS: u64 = 10;

/// Returns `block` with its coinbase height rewritten to `claimed_height`, and its coinbase expiry
/// height rewritten to `expiry_height` if that is set. The header, and so the block hash, are
/// unchanged.
///
/// Round-trips through serialization, so the result has no hashes cached from `block`.
pub(crate) fn rewrite_coinbase_height(
    block: &Block,
    claimed_height: block::Height,
    expiry_height: Option<block::Height>,
) -> Result<Arc<Block>, Report> {
    let mut coinbase = block.transactions[0].as_ref().clone();
    let mut inputs = coinbase.inputs();
    let transparent::Input::Coinbase { height, .. } = &mut inputs[0] else {
        unreachable!("the first transaction of a test vector has a coinbase input");
    };
    *height = claimed_height;
    coinbase = coinbase.with_transparent_inputs(inputs);
    if let Some(expiry_height) = expiry_height {
        coinbase.set_expiry_height(expiry_height);
    }

    let mut rewritten = block.clone();
    rewritten.transactions[0] = Arc::new(coinbase);
    let rewritten =
        Arc::<Block>::zcash_deserialize(rewritten.zcash_serialize_to_vec()?.as_slice())?;

    assert_eq!(rewritten.hash(), block.hash());
    assert_eq!(rewritten.coinbase_height(), Some(claimed_height));

    Ok(rewritten)
}

/// A peer can answer a request for a block hash with the requested header and a rewritten
/// coinbase height. The checkpoint verifier must reject that body as not matching its header
/// before it uses the height, whatever height the body claims:
/// - an already verified height must not get the block dropped as a duplicate
///   (GHSA-g9rc-89v4-hhvf),
/// - after the final checkpoint, the block must not be dropped as `Finished`, which `call()` used
///   to return before checking the block, and
/// - an unverified height must not take a queue slot at a height that isn't the block's own.
///
/// Before V5 the coinbase height is part of the transaction ID, so the Merkle root catches the
/// rewrite. From V5 on it isn't, so the coinbase expiry height must catch it, even for a claimed
/// height before NU5.
#[tokio::test]
async fn rewritten_coinbase_heights_are_rejected_before_the_height_check() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let nu5_height = NetworkUpgrade::Nu5
        .activation_height(&Mainnet)
        .expect("NU5 is active on Mainnet");
    let final_checkpoint_height = Mainnet.checkpoint_list().max_height();

    for (bytes, coinbase_version) in [
        (&zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES[..], 4),
        (&zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES[..], 5),
    ] {
        let block = Arc::<Block>::zcash_deserialize(bytes)?;
        let block_height = block.coinbase_height().expect("test vectors have a height");
        let coinbase = &block.transactions[0];
        assert_eq!(coinbase.version(), coinbase_version);

        let parent = (block_height.previous()?, block.header.previous_block_hash);
        // Already verified, including a pre-NU5 height for the V5 block, then not yet verified.
        let claimed_heights = [
            block::Height(block_height.0 - 3),
            parent.0,
            block::Height(block_height.0 + 1),
        ];
        if coinbase_version == 5 {
            assert!(claimed_heights[0] < nu5_height);
        }

        for initial_tip in [parent, (final_checkpoint_height, block::Hash([0; 32]))] {
            let state_service = MockService::build().for_unit_tests();
            let mut checkpoint_verifier =
                CheckpointVerifier::new(&Mainnet, Some(initial_tip), state_service);

            for claimed_height in claimed_heights {
                // Rewriting only the height keeps the V5 transaction ID, but not the V4 one.
                // Also rewriting the expiry height changes the V5 transaction ID too.
                let mut forgeries = vec![(
                    rewrite_coinbase_height(&block, claimed_height, None)?,
                    coinbase_version == 4,
                )];
                if coinbase_version == 5 {
                    forgeries.push((
                        rewrite_coinbase_height(&block, claimed_height, Some(claimed_height))?,
                        true,
                    ));
                }

                for (forged, changes_transaction_id) in forgeries {
                    let error = checkpoint_verifier
                        .ready()
                        .await?
                        .call(forged)
                        .await
                        .expect_err("a rewritten coinbase height must be rejected");

                    if changes_transaction_id {
                        assert!(
                            matches!(
                                error,
                                VerifyCheckpointError::VerifyBlock(VerifyBlockError::Block {
                                    source: BlockError::BadMerkleRoot { .. },
                                })
                            ),
                            "unexpected error for claimed height {claimed_height:?}: {error:?}"
                        );
                    } else {
                        assert!(
                            matches!(
                                error,
                                VerifyCheckpointError::VerifyBlock(VerifyBlockError::Block {
                                    source: BlockError::Transaction(
                                        TransactionError::CoinbaseExpiryBlockHeight {
                                            expiry_height: Some(expiry_height),
                                            block_height: rejected_height,
                                            ..
                                        }
                                    ),
                                }) if expiry_height == block_height && rejected_height == claimed_height
                            ),
                            "unexpected error for claimed height {claimed_height:?}: {error:?}"
                        );
                    }

                    assert!(error.is_body_header_mismatch());
                    assert!(!error.is_duplicate_request());
                    let error = RouterError::from(error);
                    assert!(error.is_body_header_mismatch());
                    assert_eq!(error.misbehavior_score(), 100);
                    assert!(checkpoint_verifier.queued.is_empty());
                }
            }

            // Control: the genuine body is queued, or gets `Finished` after the final checkpoint.
            if initial_tip.0 == final_checkpoint_height {
                let error = checkpoint_verifier
                    .ready()
                    .await?
                    .call(block.clone())
                    .await
                    .expect_err("checkpoint verification has finished");
                assert!(matches!(error, VerifyCheckpointError::Finished));
                assert!(!error.is_body_header_mismatch());
            } else {
                let _request = checkpoint_verifier.queue_block(block.clone(), None)?;
                assert_eq!(checkpoint_verifier.queued[&block_height].len(), 1);
            }
        }

        // Control: a genuine block that is already verified is still a benign duplicate.
        let state_service = MockService::build().for_unit_tests();
        let checkpoint_verifier =
            CheckpointVerifier::new(&Mainnet, Some((block_height, block.hash())), state_service);
        let error = checkpoint_verifier
            .check_block(block.clone())
            .expect_err("the block is already verified");
        assert!(matches!(
            error,
            VerifyCheckpointError::AlreadyVerified { .. }
        ));
        assert!(error.is_duplicate_request());
        assert!(!error.is_body_header_mismatch());
    }

    Ok(())
}

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
        .retained_blocks
        .fetch_add(1, Ordering::Relaxed);
    checkpoint_verifier
        .reset_sender
        .send(CommitFailure {
            epoch: 0,
            tip: Some(genesis_tip),
            child: Some(QueuedBlock {
                bytes: checked_block_2.retained_bytes(),
                block: checked_block_2,
                tx,
                notifications: None,
                retained: RetainedBlockPermit(checkpoint_verifier.retained_blocks.clone()),
            }),
            body_replay: None,
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

/// Returns a configured testnet that doesn't check proof of work, so tests can make blocks.
fn network_without_pow() -> Network {
    zebra_chain::parameters::testnet::Parameters::build()
        .with_disable_pow(true)
        .to_network()
        .expect("the parameters are valid")
}

/// Returns a copy of `block` with its coinbase height set to `height`.
fn with_coinbase_height(block: &Block, height: u32) -> Block {
    let mut block = block.clone();

    let mut inputs = block.transactions[0].inputs();
    let transparent::Input::Coinbase {
        height: coinbase_height,
        ..
    } = &mut inputs[0]
    else {
        panic!("the first transaction is a coinbase");
    };
    *coinbase_height = block::Height(height);
    block.transactions[0] = Arc::new(
        (*block.transactions[0])
            .clone()
            .with_transparent_inputs(inputs),
    );

    block
}

/// Returns a copy of mainnet block 1 at `height`, with `parent` as its previous block.
fn block_at(height: u32, parent: block::Hash) -> Arc<Block> {
    let block_1 = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
        .expect("the test vector is valid");
    let mut block = with_coinbase_height(&block_1, height);

    let merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
    let header = Arc::make_mut(&mut block.header);
    header.previous_block_hash = parent;
    header.merkle_root = merkle_root;

    Arc::new(block)
}

/// Returns a chain of [`block_at`] blocks from the mainnet genesis block to `tip_height`, and
/// checkpoints for it at `checkpoint_heights`.
fn chain_with_checkpoints(
    tip_height: u32,
    checkpoint_heights: &[u32],
) -> (Vec<Arc<Block>>, Vec<(block::Height, block::Hash)>) {
    let mut chain = vec![Arc::<Block>::zcash_deserialize(
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
    )
    .expect("the test vector is valid")];
    for height in 1..=tip_height {
        let parent = chain.last().expect("the chain has a block").hash();
        chain.push(block_at(height, parent));
    }

    let checkpoints = checkpoint_heights
        .iter()
        .map(|&height| (block::Height(height), chain[height as usize].hash()))
        .collect();

    (chain, checkpoints)
}

/// Internal callers bypass network decoding, so the verifier enforces the block size limit.
/// A decoded block at the limit is still admitted.
#[tokio::test]
async fn oversized_internal_blocks_are_rejected() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (chain, checkpoints) = chain_with_checkpoints(4, &[0, 4]);
    let with_script = |script_len| {
        let mut block = chain[1].clone();
        let block_mut = Arc::make_mut(&mut block);
        let coinbase = Arc::make_mut(&mut block_mut.transactions[0]);
        let mut outputs = coinbase.outputs();
        outputs[0].lock_script = transparent::Script::new(&vec![0; script_len]);
        coinbase.set_outputs(outputs);
        let merkle_root = block_mut.transactions.iter().map(|tx| tx.hash()).collect();
        Arc::make_mut(&mut block_mut.header).merkle_root = merkle_root;
        block
    };
    let limit = usize::try_from(block::MAX_BLOCK_BYTES)?;
    // Keep the script length's CompactSize prefix five bytes wide while filling the block.
    let overhead = with_script(65_536).zcash_serialized_size() - 65_536;
    let at_limit = with_script(limit - overhead).zcash_serialize_to_vec()?;
    assert_eq!(at_limit.len(), limit);
    let decoded = Arc::<Block>::zcash_deserialize(&at_limit[..])?;

    let mut verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        MockService::build().for_unit_tests(),
    )
    .map_err(|e| eyre!(e))?;

    assert!(verifier.queue_block(decoded, None).is_ok());
    assert!(matches!(
        verifier.queue_block(with_script(limit - overhead + 1), None),
        Err(VerifyCheckpointError::VerifyBlock(
            VerifyBlockError::OversizedBlock
        ))
    ));
    Ok(())
}

/// When the queue is out of memory, a block replaces the highest queued blocks that aren't linked
/// to the next checkpoint, and a block above all of them is rejected. So blocks that Zebra can't
/// verify yet can't take the memory that the next checkpoint range needs.
#[tokio::test(flavor = "multi_thread")]
async fn full_queue_fails_highest_unlinked_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (chain, checkpoints) = chain_with_checkpoints(12, &[0, 4, 8, 12]);
    let mut state_service = MockService::build().for_unit_tests();
    let mut checkpoint_verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        state_service.clone(),
    )
    .map_err(|e| eyre!(e))?;

    // Fill the queue with the blocks of a later checkpoint range, leaving room for half a block.
    let later: Vec<_> = chain[5..=8]
        .iter()
        .map(|block| checkpoint_verifier.call(block.clone()))
        .collect();
    checkpoint_verifier.max_queued_bytes = checkpoint_verifier.queued_bytes * 9 / 8;

    assert!(
        matches!(
            checkpoint_verifier.call(chain[9].clone()).now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ),
        "a block above the full queue is rejected"
    );

    let mut range = Vec::new();
    for (height, replaced_height) in [(1, 8), (2, 7), (4, 6), (3, 5)] {
        range.push(checkpoint_verifier.call(chain[height].clone()));
        assert!(
            !checkpoint_verifier
                .queued
                .contains_key(&block::Height(replaced_height)),
            "block {height} replaces block {replaced_height}"
        );
    }
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        PreviousCheckpoint(block::Height(4))
    );
    assert_eq!(checkpoint_verifier.queued_bytes, 0);

    for later in later {
        assert!(matches!(
            timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), later).await?,
            Err(VerifyCheckpointError::QueuedLimit)
        ));
    }

    for block in &chain[1..=4] {
        let hash = block.hash();
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
    for verify_future in range {
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), verify_future).await??;
    }

    Ok(())
}

/// A height full of blocks that aren't in the checkpointed chain can't stall checkpoint
/// verification: once the blocks above it link the needed block to a checkpoint, it replaces
/// one of them.
#[tokio::test(flavor = "multi_thread")]
async fn needed_block_replaces_unlinked_block_at_full_height() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let (chain, checkpoints) = chain_with_checkpoints(4, &[0, 4]);
    let mut state_service = MockService::build().for_unit_tests();
    let mut checkpoint_verifier = CheckpointVerifier::from_list(
        checkpoints.clone(),
        &network_without_pow(),
        Some(checkpoints[0]),
        state_service.clone(),
    )
    .map_err(|e| eyre!(e))?;

    let side_blocks: Vec<_> = (1..=MAX_QUEUED_BLOCKS_PER_HEIGHT)
        .map(|i| checkpoint_verifier.call(block_at(2, block::Hash([i as u8; 32]))))
        .collect();
    assert!(
        matches!(
            checkpoint_verifier.call(chain[2].clone()).now_or_never(),
            Some(Err(VerifyCheckpointError::QueuedLimit))
        ),
        "the block isn't linked to the checkpoint yet"
    );

    let range: Vec<_> = [4, 3, 2, 1]
        .into_iter()
        .map(|height| checkpoint_verifier.call(chain[height].clone()))
        .collect();
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        FinalCheckpoint
    );

    let mut side_block_errors = Vec::new();
    for side_block in side_blocks {
        side_block_errors.push(
            timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), side_block)
                .await?
                .expect_err("side chain blocks fail"),
        );
    }
    assert_eq!(
        side_block_errors
            .iter()
            .filter(|e| matches!(e, VerifyCheckpointError::QueuedLimit))
            .count(),
        1,
        "the needed block replaces one side chain block, and the others fail verification"
    );

    for block in &chain[1..=4] {
        let hash = block.hash();
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
    for verify_future in range {
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), verify_future).await??;
    }

    Ok(())
}

/// A block hash doesn't cover a V5 coinbase height, but the checkpoint checks bind it to the
/// header before queueing, so copies of a block linked to the next checkpoint at other heights
/// are rejected, and can't stop the block that the checkpoint needs next from being queued.
#[tokio::test(flavor = "multi_thread")]
async fn copies_of_linked_block_at_other_heights_are_rejected() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let deserialize = |bytes: &[u8]| Arc::<Block>::zcash_deserialize(bytes);
    let genesis = deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
    let block_106 = deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES[..])?;
    let block_107 = deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES[..])?;
    let block_108 = deserialize(&zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES[..])?;
    let block_107_hash = block_107.hash();
    let block_108_hash = block_108.hash();

    let tip = (block::Height(1_687_106), block_106.hash());
    let checkpoints = [
        (block::Height(0), genesis.hash()),
        tip,
        (block::Height(1_687_108), block_108_hash),
        (block::Height(1_687_200), block::Hash([0xff; 32])),
    ];
    let mut state_service = MockService::build().for_unit_tests();
    let mut checkpoint_verifier =
        CheckpointVerifier::from_list(checkpoints, &Mainnet, Some(tip), state_service.clone())
            .map_err(|e| eyre!(e))?;

    let mut range = vec![checkpoint_verifier.call(block_108.clone())];
    for height in 1_687_109..=1_687_112 {
        let copy = with_coinbase_height(&block_108, height);
        assert_eq!(copy.hash(), block_108_hash);
        let copy = checkpoint_verifier.call(Arc::new(copy));
        assert!(
            !checkpoint_verifier
                .queued
                .contains_key(&block::Height(height)),
            "the checkpoint checks reject a copy with another V5 coinbase height"
        );
        let error = timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), copy)
            .await?
            .expect_err("the copy fails verification");
        assert!(
            error.is_body_header_mismatch(),
            "the copy is rejected as a forged body, not for its height or a full queue: {error:?}"
        );
    }
    range.push(checkpoint_verifier.call(block_107.clone()));
    assert_eq!(
        checkpoint_verifier.previous_checkpoint_height(),
        PreviousCheckpoint(block::Height(1_687_108)),
        "the needed block is queued"
    );

    for hash in [block_107_hash, block_108_hash] {
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
    for verify_future in range {
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), verify_future).await??;
    }

    Ok(())
}

/// A queued local child retains its notification when the caller drops before checkpoint validation.
#[tokio::test(flavor = "multi_thread")]
async fn mined_checkpoint_retains_notification_after_caller_drop() {
    let _init_guard = zebra_test::init();
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let genesis =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap();
        let child =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .unwrap();
        let state = zs::init_test(&Mainnet).await;
        let mut verifier = CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis.hash()),
                (block::Height(1), child.hash()),
            ],
            &Mainnet,
            None,
            state.clone(),
        )
        .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let (validated, validation) = tokio::sync::watch::channel(None);
        let notifications = zs::MinedBlockNotifications::new(committed, validated);

        drop(verifier.verify(child.clone(), Some(notifications)));
        assert_eq!(
            verifier.oneshot(genesis.clone()).await.unwrap(),
            genesis.hash(),
            "the missing parent is admitted independently of its queued child",
        );
        assert_eq!(
            commits.recv().await.unwrap(),
            (child.hash(), block::Height(1)),
            "the existing commit owner retains notifications after the caller drops",
        );
        assert!(validation.borrow().is_none());
        assert_eq!(
            state
                .oneshot(zs::Request::Block(child.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(Some(child)),
        );
    })
    .await
    .expect("checkpoint notifications must survive caller cancellation");
}

#[derive(Clone, Copy)]
enum ReplacementObserver {
    Healthy,
    Full,
    Closed,
}

async fn ordinary_checkpoint_replacement(observer: ReplacementObserver) {
    timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
        let genesis =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap();
        let child =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])
                .unwrap();
        let state = zs::init_test(&Mainnet).await;
        let mut verifier = CheckpointVerifier::from_list(
            [
                (block::Height(0), genesis.hash()),
                (block::Height(1), child.hash()),
            ],
            &Mainnet,
            None,
            state.clone(),
        )
        .unwrap();
        let (committed, mut commits) = tokio::sync::mpsc::channel(1);
        let existing = (block::Hash([9; 32]), block::Height(0));
        match observer {
            ReplacementObserver::Healthy => {}
            ReplacementObserver::Full => committed.try_send(existing).unwrap(),
            ReplacementObserver::Closed => commits.close(),
        }
        let (validated, validation) = tokio::sync::watch::channel(None);
        drop(verifier.verify(
            child.clone(),
            Some(zs::MinedBlockNotifications::new(committed, validated)),
        ));
        let replacement = verifier.call(child.clone());
        assert_eq!(
            verifier.call(genesis.clone()).await.unwrap(),
            genesis.hash()
        );
        assert_eq!(replacement.await.unwrap(), child.hash());
        assert_eq!(
            state
                .oneshot(zs::Request::Block(child.hash().into()))
                .await
                .unwrap(),
            zs::Response::Block(Some(child.clone())),
        );
        assert!(validation.borrow().is_none());
        match observer {
            ReplacementObserver::Healthy => assert_eq!(
                commits.recv().await,
                Some((child.hash(), block::Height(1))),
                "ordinary replacement must retain the healthy mined observer",
            ),
            ReplacementObserver::Full => {
                assert_eq!(commits.recv().await, Some(existing));
                assert!(commits.try_recv().is_err());
            }
            ReplacementObserver::Closed => assert!(commits.recv().await.is_none()),
        }
    })
    .await
    .expect("a superseded mined observer must not gate the ordinary replacement");
}

#[tokio::test]
async fn ordinary_checkpoint_duplicate_preserves_healthy_mined_observer() {
    let _init_guard = zebra_test::init();
    ordinary_checkpoint_replacement(ReplacementObserver::Healthy).await;
}

#[tokio::test]
async fn ordinary_checkpoint_duplicate_ignores_unavailable_mined_observers() {
    let _init_guard = zebra_test::init();
    ordinary_checkpoint_replacement(ReplacementObserver::Full).await;
    ordinary_checkpoint_replacement(ReplacementObserver::Closed).await;
}

/// An old child's recovery can arrive after an ordinary copy is queued.
#[tokio::test]
async fn late_child_recovery_preserves_ordinary_checkpoint_observer() {
    let _init_guard = zebra_test::init();
    for observer in [
        ReplacementObserver::Healthy,
        ReplacementObserver::Full,
        ReplacementObserver::Closed,
    ] {
        // Each variant initializes its own state, so each gets its own deadline.
        timeout(Duration::from_secs(VERIFY_TIMEOUT_SECONDS), async {
            let blocks: Vec<_> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
                .values()
                .take(3)
                .map(|bytes| Arc::<Block>::zcash_deserialize(*bytes).unwrap())
                .collect();
            let tip = (block::Height(0), blocks[0].hash());
            let state = zs::init_test(&Mainnet).await;
            state
                .clone()
                .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                    blocks[0].clone().into(),
                ))
                .await
                .unwrap();
            let mut verifier = CheckpointVerifier::from_list(
                [tip, (block::Height(2), blocks[2].hash())],
                &Mainnet,
                Some(tip),
                state,
            )
            .unwrap();

            // The parent's failure resets the verifier before the old child's recovery arrives.
            verifier
                .reset_sender
                .send(CommitFailure {
                    epoch: 0,
                    tip: Some(tip),
                    child: None,
                    body_replay: None,
                })
                .unwrap();
            let ordinary_body = Arc::new((*blocks[2]).clone());
            let ordinary = verifier.call(ordinary_body.clone());
            assert_eq!(verifier.epoch, 1);

            let (committed, mut commits) = tokio::sync::mpsc::channel(1);
            let existing = (block::Hash([9; 32]), block::Height(0));
            match observer {
                ReplacementObserver::Healthy => {}
                ReplacementObserver::Full => committed.try_send(existing).unwrap(),
                ReplacementObserver::Closed => commits.close(),
            }
            let (validated, validation) = tokio::sync::watch::channel(None);
            let (tx, old_response) = oneshot::channel();
            let old_child: CheckpointVerifiedBlock = blocks[2].clone().into();
            // Model the reservation still held by the old child's pending recovery owner.
            verifier.retained_blocks.fetch_add(1, Ordering::Relaxed);
            verifier
                .reset_sender
                .send(CommitFailure {
                    epoch: 0,
                    tip: Some(tip),
                    child: Some(QueuedBlock {
                        bytes: old_child.retained_bytes(),
                        block: old_child,
                        tx,
                        notifications: Some(zs::MinedBlockNotifications::new(committed, validated)),
                        retained: RetainedBlockPermit(verifier.retained_blocks.clone()),
                    }),
                    body_replay: None,
                })
                .unwrap();
            // Drain the late recovery without supplying the missing parent yet.
            assert!(matches!(
                verifier.call(blocks[0].clone()).await,
                Err(VerifyCheckpointError::AlreadyVerified { .. })
            ));
            assert!(Arc::ptr_eq(
                &verifier.queued[&block::Height(2)][0].block.block,
                &ordinary_body,
            ));
            assert!(matches!(
                old_response.await.unwrap(),
                Err(VerifyCheckpointError::NewerRequest { .. })
            ));
            assert_eq!(verifier.retained_blocks.load(Ordering::Relaxed), 1);

            assert_eq!(
                verifier.call(blocks[1].clone()).await.unwrap(),
                blocks[1].hash(),
            );
            assert_eq!(ordinary.await.unwrap(), blocks[2].hash());
            assert_eq!(verifier.retained_blocks.load(Ordering::Relaxed), 0);
            assert!(validation.borrow().is_none());
            match observer {
                ReplacementObserver::Healthy => {
                    assert_eq!(
                        commits.recv().await,
                        Some((blocks[2].hash(), block::Height(2)))
                    );
                }
                ReplacementObserver::Full => {
                    assert_eq!(commits.recv().await, Some(existing));
                    assert!(commits.try_recv().is_err());
                }
                ReplacementObserver::Closed => assert!(commits.recv().await.is_none()),
            }
        })
        .await
        .expect("late recovery must preserve notifications without gating ordinary commits");
    }
}
