//! Checkpoint-based block verification for Zebra.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to speed up the
//! initial chain sync for Zebra. This list is distributed with Zebra.
//!
//! The CheckpointVerifier queues pending blocks. Once there is a chain between
//! the next pair of checkpoints, it verifies all the blocks in that chain.
//! Verification starts at the first checkpoint, which is the genesis block for the
//! configured network.
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use futures_util::FutureExt;
use std::{
    collections::BTreeMap,
    error,
    future::Future,
    iter::successors,
    ops::{Bound, Bound::*},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::oneshot;
use tower::Service;

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

/// The inner error type for CheckpointVerifier.
///
/// CheckpointVerifier returns two layers of `Result`s. The outer error type is
/// fixed by the `tokio::sync::oneshot` channel receiver future.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// An unverified block, which is in the queue for checkpoint verification.
#[derive(Debug)]
struct QueuedBlock {
    /// The block data.
    block: Arc<Block>,
    /// `block`'s cached header hash.
    hash: BlockHeaderHash,
    /// The transmitting end of a oneshot channel.
    ///
    /// The receiving end of this oneshot is passed to the caller as the future.
    /// It has two layers of `Result`s, an inner `checkpoint::Error`, and an
    /// outer `tokio::sync::oneshot::error::RecvError`.
    tx: oneshot::Sender<Result<BlockHeaderHash, Error>>,
}

/// A list of unverified blocks at a particular height.
///
/// Typically contains zero or one blocks, but might contain more if a peer
/// has an old chain fork. (Or sends us a bad block.)
type QueuedBlockList = Vec<QueuedBlock>;

/// A block height verification range.
///
/// Implements `RangeBounds<BlockHeight>`.
type VerifyBounds = (Bound<BlockHeight>, Bound<BlockHeight>);

/// A checkpointing block verifier.
///
/// Verifies blocks using a supplied list of checkpoints. There must be at
/// least one checkpoint for the genesis block.
#[derive(Debug)]
struct CheckpointVerifier {
    // Inputs
    //
    /// Each checkpoint consists of a coinbase height and block header hash.
    ///
    /// Checkpoints should be chosen to avoid forks or chain reorganizations,
    /// which only happen in the last few hundred blocks in the chain.
    /// (zcashd allows chain reorganizations up to 99 blocks, and prunes
    /// orphaned side-chains after 288 blocks.)
    ///
    /// There must be a checkpoint for the genesis block at BlockHeight 0.
    /// (All other checkpoints are optional.)
    checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash>,

    // Cached Blocks
    //
    /// A cache of unverified blocks.
    ///
    /// Contains a list of unverified blocks at each block height. In most cases,
    /// the checkpoint verifier will store zero or one block at each height.
    ///
    /// Blocks are verified in order, when there is a chain from the next
    /// checkpoint, back to the `current_checkpoint_height`.Each pair of
    /// checkpoints is used to verify all the blocks between those checkpoints.
    ///
    /// The first checkpoint does not have any ancestors, so it only verifies the
    /// genesis block.
    queued: BTreeMap<BlockHeight, QueuedBlockList>,

    /// The range of heights that we are currently verifying. Extends from the
    /// most recently verified checkpoint (`Excluded`), to the next highest
    /// checkpoint (`Included`).
    ///
    /// `(Included(0), Included(0))` means that checkpoint verification has not
    /// started yet, and the next range to be verified only contains the genesis
    /// checkpoint.
    ///
    /// `None` means that checkpoint verification has finished.
    current_checkpoint_range: Option<VerifyBounds>,
}

/// The CheckpointVerifier implementation.
///
/// Contains non-service utility functions for CheckpointVerifiers.
impl CheckpointVerifier {
    /// Return a checkpoint verification service, using the provided `checkpoint_list`.
    ///
    /// The returned type is opaque to allow instrumentation or other wrappers, but
    /// can be boxed for storage. It is also `Clone` to allow sharing of a
    /// verification service.
    ///
    /// This function should be called only once for a particular checkpoint list (and
    /// network), rather than constructing multiple verification services based on the
    /// same checkpoint list.
    //
    // Currently only used in tests.
    //
    // We'll use this function in the overall verifier, which will split blocks
    // between BlockVerifier and CheckpointVerifier.
    #[cfg(test)]
    fn new(
        checkpoint_list: impl Into<BTreeMap<BlockHeight, BlockHeaderHash>>,
    ) -> Result<Self, Error> {
        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> = checkpoint_list.into();

        // An empty checkpoint list can't actually verify any blocks.
        match checkpoints.keys().cloned().next() {
            None => {
                return Err("there must be at least one checkpoint, for the genesis block".into())
            }
            Some(BlockHeight(0)) => {}
            _ => return Err("checkpoints must start at the genesis block height 0".into()),
        };

        Ok(CheckpointVerifier {
            checkpoint_list: checkpoints,
            queued: <BTreeMap<BlockHeight, QueuedBlockList>>::new(),
            // We start by verifying the genesis block, by itself
            current_checkpoint_range: Some((Included(BlockHeight(0)), Included(BlockHeight(0)))),
        })
    }

    /// If the block height of `block` is less than or equal to the maximum
    /// checkpoint height, returns that height.
    ///
    /// Returns an error if the block's height is greater than the maximum
    /// checkpoint. Also returns an error if the block or maximum heights are
    /// missing.
    fn check_block_height(&self, block: Arc<Block>) -> Result<BlockHeight, Error> {
        let block_height = block
            .coinbase_height()
            .ok_or("the block does not have a coinbase height")?;
        let max_checkpoint_height = self
            .get_max_checkpoint_height()
            .ok_or("the checkpoint list is empty")?;
        if block_height > max_checkpoint_height {
            return Err("the block is higher than the maximum checkpoint".into());
        }
        if let Some(previous_checkpoint_height) = self.get_previous_checkpoint_height() {
            if block_height <= previous_checkpoint_height {
                return Err("a block at this height has already been verified".into());
            }
        }
        Ok(block_height)
    }

    /// Return the block height of the highest checkpoint in the checkpoint list.
    ///
    /// If there is only a single checkpoint, then the maximum height will be
    /// zero. (The genesis block.)
    ///
    /// The maximum height is constant for each checkpoint list.
    ///
    /// Returns None if there are no checkpoints.
    fn get_max_checkpoint_height(&self) -> Option<BlockHeight> {
        self.checkpoint_list.keys().cloned().next_back()
    }

    /// Return the most recently verified checkpoint height.
    ///
    /// The height increases as blocks are verified.
    ///
    /// If verification has not started yet, returns None.
    /// If verification has finished, returns the maximum checkpoint height.
    fn get_previous_checkpoint_height(&self) -> Option<BlockHeight> {
        match self.current_checkpoint_range {
            Some((Included(BlockHeight(0)), _)) => None,
            Some((Excluded(height), _)) => Some(height),
            None => self.get_max_checkpoint_height(),
            _ => unreachable!(),
        }
    }

    /// Return the next checkpoint height that we want to verify.
    ///
    /// The height increases as blocks are verified.
    ///
    /// If verification has not started yet, returns zero (the genesis block).
    /// If verification has finished, returns None.
    fn get_next_checkpoint_height(&self) -> Option<BlockHeight> {
        match self.current_checkpoint_range {
            Some((_, Included(height))) => Some(height),
            None => None,
            _ => unreachable!(),
        }
    }

    /// Return the most recently verified checkpoint's hash.
    ///
    /// If verification has not started yet, returns None.
    /// If verification has finished, returns the maximum checkpoint's hash.
    fn get_previous_checkpoint_hash(&self) -> Option<BlockHeaderHash> {
        match self.get_previous_checkpoint_height() {
            // Every checkpoint height must have a hash
            Some(height) => Some(*self.checkpoint_list.get(&height).unwrap()),
            None => None,
        }
    }

    /// Return the hash of the next checkpoint we want to verify.
    ///
    /// If verification has not started yet, returns the hash of block zero
    /// (the genesis block). If verification has finished, returns None.
    fn get_next_checkpoint_hash(&self) -> Option<BlockHeaderHash> {
        match self.get_next_checkpoint_height() {
            // Every checkpoint height must have a hash
            Some(height) => Some(*self.checkpoint_list.get(&height).unwrap()),
            None => None,
        }
    }

    /// Return the height of the next checkpoint higher than `after_block_height`
    /// in the checkpoint list. Ignores the current checkpoint range.
    ///
    /// If `after_block_height` is None, assume verification has not started yet,
    /// and return zero (the genesis block).
    ///
    /// Returns None if there are no checkpoints. Also returns None if
    /// `after_block_height` is greater than or equal to the maximum height.
    fn find_descendant_checkpoint_height(
        &self,
        after_block_height: Option<BlockHeight>,
    ) -> Option<BlockHeight> {
        match after_block_height {
            None => self.checkpoint_list.keys().cloned().next(),
            Some(height) => self
                .checkpoint_list
                .range((Excluded(height), Unbounded))
                .next()
                .map(|(height, _)| *height),
        }
    }

    /// Increase the current checkpoint height to `verified_block_height`,
    /// if `verified_block_height` is the next checkpoint height.
    fn update_current_checkpoint_height(&mut self, verified_block_height: BlockHeight) {
        let previous_height = self.get_previous_checkpoint_height();
        // Ignore blocks that are below the previous checkpoint.
        //
        // We ignore out-of-order verification, such as:
        //  - the height is less than the previous checkpoint height, or
        //  - the previous checkpoint height is the maximum height (checkpoint verifies are finished),
        // because futures might not resolve in height order.
        if let Some(previous) = previous_height {
            if verified_block_height <= previous {
                return;
            }
        }

        // Ignore updates if the checkpoint list is empty, or verification has finished.
        if let Some(next_height) = self.get_next_checkpoint_height() {
            if verified_block_height != next_height {
                return;
            }
        }

        // Set the new range
        if let Some(new_checkpoint) =
            self.find_descendant_checkpoint_height(Some(verified_block_height))
        {
            // Increment the range
            self.current_checkpoint_range =
                Some((Excluded(verified_block_height), Included(new_checkpoint)));
        } else {
            // Verification has finished
            self.current_checkpoint_range = None;
        }
    }

    /// Queue `block` for verification, and return `(height, hash)`.
    ///
    /// Verification will finish when the chain to the next checkpoint is complete,
    /// and the caller will be notified via `tx`.
    ///
    /// If the block does not have a coinbase height, sends an error on `tx`, does
    /// not queue the block, and returns None.
    fn insert_queued_block(
        &mut self,
        block: Arc<Block>,
        tx: oneshot::Sender<Result<BlockHeaderHash, Error>>,
    ) -> Option<(BlockHeight, BlockHeaderHash)> {
        // Check for a valid height
        let height = match self.check_block_height(block.clone()) {
            Ok(height) => height,
            Err(error) => {
                // Sending might fail, depending on what the caller does with rx,
                // but there's nothing we can do about it.
                let _ = tx.send(Err(error));
                return None;
            }
        };

        // Add the block to the list of queued blocks at this height
        let hash = block.as_ref().into();
        let new_qblock = QueuedBlock { block, hash, tx };
        self.queued.entry(height).or_default().push(new_qblock);

        Some((height, hash))
    }
}

/// The CheckpointVerifier service implementation.
///
/// After verification, the block futures resolve to their hashes.
impl Service<Arc<Block>> for CheckpointVerifier {
    /// The CheckpointVerifier service has two layers of `Result`s, an inner
    /// `checkpoint::Error`, and an outer `tokio::sync::oneshot::error::RecvError`.
    type Response = Result<BlockHeaderHash, Error>;
    type Error = oneshot::error::RecvError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We don't expect the verifier to exert backpressure on its users.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report

        // Set up a oneshot channel as the future
        let (tx, rx) = oneshot::channel();

        // Queue the block for verification, returning early on error
        if self.insert_queued_block(block, tx).is_none() {
            return Box::pin(rx.boxed());
        }

        // Now try to verify, from the next checkpoint to the previous one.

        let next_checkpoint_height = self.get_next_checkpoint_height();
        // If the verifier has finished, or there are no checkpoints, queuing a block should
        // haved failed and returned early.
        let next_checkpoint_hash = self.get_next_checkpoint_hash().unwrap();

        // The previous height and hash are None, until the genesis block is verified
        let previous_checkpoint_height = self.get_previous_checkpoint_height();
        let previous_checkpoint_hash = self.get_previous_checkpoint_hash();

        // Walk the queued blocks, from next checkpoint, to the child of the previous checkpoint
        // (if we're just starting verifying, we only check the genesis block)
        if let Some(current_range) = self.current_checkpoint_range {
            let qrange = self.queued.range(current_range).rev();
            // Limited by qrange
            let mut expected_hash = next_checkpoint_hash;
            let expected_heights = successors(next_checkpoint_height, |n| {
                n.0.checked_sub(1).map(BlockHeight)
            });
            for ((&height, qblocks), expected_height) in qrange.zip(expected_heights) {
                // Check that the heights are continuous
                if height != expected_height {
                    return Box::pin(rx.boxed());
                }
                let mut next_hash = None;
                // Check if any of the queued blocks are part of the hash chain
                for qblock in qblocks {
                    if qblock.hash == expected_hash {
                        next_hash = Some(qblock.block.header.previous_block_hash);
                    }
                }
                if let Some(next_hash) = next_hash {
                    expected_hash = next_hash;
                } else {
                    return Box::pin(rx.boxed());
                }
            }

            // We completed this part of the chain
            // verify all the blocks, and discard all the bad blocks
            if Some(expected_hash) == previous_checkpoint_hash {
                // TODO: refactor duplicate code
                let mut expected_hash = next_checkpoint_hash;
                let qrange = self.queued.range_mut(current_range).rev();
                for (_, qblocks) in qrange {
                    let mut next_hash = None;
                    // Check if any of the queued blocks are part of the hash chain
                    for qblock in qblocks.drain(..) {
                        if qblock.hash == expected_hash {
                            if next_hash == None {
                                // The first valid block
                                next_hash = Some(qblock.block.header.previous_block_hash);
                                // Sending can fail, but only because the receiver has closed
                                // the channel. So there's nothing we can do about the error.
                                let _ = qblock.tx.send(Ok(qblock.hash));
                            } else {
                                // duplicate blocks
                                // Sending can fail, but there's nothing we can do about it.
                                let _ = qblock.tx.send(Err(
                                    "duplicate valid blocks at this height, only one was chosen"
                                        .into(),
                                ));
                            }
                        } else {
                            // bad blocks
                            // Sending can fail, but there's nothing we can do about it.
                            let _ = qblock.tx.send(Err(
                                "the block hash does not match the chained checkpoint hash".into(),
                            ));
                        }
                    }
                    // Can't fail, we just checked it in the previous loop
                    expected_hash = next_hash.unwrap();
                }

                // Now remove the empty vectors at all those heights
                if let (Some(BlockHeight(previous_height)), Some(BlockHeight(next_height))) =
                    (previous_checkpoint_height, next_checkpoint_height)
                {
                    for height in previous_height..=next_height {
                        let ov = self.queued.remove(&BlockHeight(height));
                        debug_assert_eq!(ov.map(|v| v.len()), Some(0));
                    }
                } else {
                    // Just delete the genesis block
                    let ov = self.queued.remove(&BlockHeight(0));
                    debug_assert_eq!(ov.map(|v| v.len()), Some(0));
                }

                // Finally, update the checkpoint bounds
                // The heights have to be valid here, because this part of the chain is valid
                self.update_current_checkpoint_height(next_checkpoint_height.unwrap());
            }
        } else {
            // We've finished verifying, and the queue should be empty
            debug_assert_eq!(self.queued.len(), 0);
            self.queued.clear();
        }

        Box::pin(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{eyre, Report};
    use std::time::Duration;
    use tokio::time::timeout;
    use tower::{Service, ServiceExt};

    use zebra_chain::serialization::ZcashDeserialize;

    /// The timeout we apply to each verify future during testing.
    ///
    /// The checkpoint verifier uses `tokio::sync::oneshot` channels as futures.
    /// If the verifier doesn't send a message on the channel, any tests that
    /// await the channel future will hang.
    ///
    /// This value is set to a large value, to avoid spurious failures due to
    /// high system load.
    const VERIFY_TIMEOUT_SECONDS: u64 = 10;

    #[tokio::test]
    #[spandoc::spandoc]
    async fn single_item_checkpoint_list() -> Result<(), Report> {
        zebra_test::init();

        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let hash0: BlockHeaderHash = block0.as_ref().into();

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), hash0)]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for block 0
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block0.clone()),
        );
        /// Wait for the response for block 0
        // TODO(teor || jlusby): check error kind
        let verify_response = verify_future
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, hash0);

        assert_eq!(
            checkpoint_verifier.get_previous_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(checkpoint_verifier.get_next_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn multi_item_checkpoint_list() -> Result<(), Report> {
        zebra_test::init();

        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            // TODO(teor): not continuous, so they hang
            //&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            //&zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((block.clone(), block.coinbase_height().unwrap(), hash));
        }

        // Make a checkpoint list containing all the blocks
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> = checkpoint_data
            .iter()
            .map(|(_block, height, hash)| (*height, *hash))
            .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(1))
        );

        // Now verify each block
        for (block, height, hash) in checkpoint_data {
            /// Make sure the verifier service is ready
            let ready_verifier_service = checkpoint_verifier
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?;

            /// Set up the future for block {?height}
            let verify_future = timeout(
                Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
                ready_verifier_service.call(block.clone()),
            );
            /// Wait for the response for block {?height}
            // TODO(teor || jlusby): check error kind
            let verify_response = verify_future
                .await
                .expect("timeout should not happen")
                .expect("oneshot channel should not fail")
                .map_err(|e| eyre!(e))?;

            assert_eq!(verify_response, hash);

            // We can't easily calculate the next checkpoint height, but the previous
            // loop iteration uses the next checkpoint height function to set the
            // current height we're testing here
            assert_eq!(
                checkpoint_verifier.get_previous_checkpoint_height(),
                Some(height)
            );
            assert_eq!(
                checkpoint_verifier.get_max_checkpoint_height(),
                Some(BlockHeight(1))
            );
        }

        assert_eq!(
            checkpoint_verifier.get_previous_checkpoint_height(),
            Some(BlockHeight(1))
        );
        assert_eq!(checkpoint_verifier.get_next_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(1))
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn block_higher_than_max_checkpoint_fail() -> Result<(), Report> {
        zebra_test::init();

        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let block415000 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), block0.as_ref().into())]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for block 415000
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(block415000.clone()),
        );
        /// Wait for the response for block 415000, and expect failure
        // TODO(teor || jlusby): check error kind
        let _ = verify_future
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .expect_err("bad block hash should fail");

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn wrong_checkpoint_hash_fail() -> Result<(), Report> {
        zebra_test::init();

        let good_block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let good_block0_hash: BlockHeaderHash = good_block0.as_ref().into();
        let mut bad_block0 =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        // Change the header hash
        bad_block0.header.version = 0;
        let bad_block0: Arc<Block> = bad_block0.into();

        // Make a checkpoint list containing the genesis block checkpoint
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(good_block0.coinbase_height().unwrap(), good_block0_hash)]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Make sure the verifier service is ready (1/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for bad block 0 (1/3)
        // TODO(teor || jlusby): check error kind
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        /// Wait for the response for block 0, and expect failure (1/3)
        // TODO(teor || jlusby): check error kind
        let _ = verify_future
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .expect_err("bad block hash should fail");

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Make sure the verifier service is ready (2/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for bad block 0 again (2/3)
        // TODO(teor || jlusby): check error kind
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        /// Wait for the response for block 0, and expect failure again (2/3)
        // TODO(teor || jlusby): check error kind
        let _ = verify_future
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .expect_err("bad block hash should fail");

        assert_eq!(checkpoint_verifier.get_previous_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_next_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Make sure the verifier service is ready (3/3)
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Set up the future for good block 0 (3/3)
        let verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(good_block0.clone()),
        );
        /// Wait for the response for good block 0, and expect success (3/3)
        // TODO(teor || jlusby): check error kind
        let verify_response = verify_future
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, good_block0_hash);

        assert_eq!(
            checkpoint_verifier.get_previous_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(checkpoint_verifier.get_next_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        Ok(())
    }
}
