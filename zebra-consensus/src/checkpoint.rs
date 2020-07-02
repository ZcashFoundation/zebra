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
    ops::{Bound, Bound::*, RangeBounds},
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

    // Queued Blocks
    //
    /// A queue of unverified blocks.
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
    /// If checkpoint verification has not started yet, the current range only
    /// contains the genesis checkpoint.
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
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_list.into_iter().collect();

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
    /// If verification has not started yet, returns None. To verify the genesis
    /// block, check that the parent block hash is all zeroes. (Bitcoin "null".)
    ///
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

    /// Return `(height, hash)` for the next block needed to verify the
    /// current checkpoint range. Walks the chain of queued blocks, starting
    /// from the next checkpoint, and following the parent hash of each block.
    ///
    /// If the `height` is equal to the previous checkpoint height, then the
    /// `hash` should be equal to the previous checkpoint hash. When verifying
    /// the genesis block, returns `(None, [0; 32])` because the genesis block
    /// has no parent block. (And in Bitcoin, `null` is `[0; 32]`.)
    ///
    /// If there are no queued blocks for the current range, returns the next
    /// checkpoint height and hash. If checkpoint verification has finished,
    /// returns `(None, None)`.
    fn find_checkpoint_chain_end(&self) -> (Option<BlockHeight>, Option<BlockHeaderHash>) {
        // If checkpoint verification has finished, there is no chain end.
        if self.current_checkpoint_range.is_none() {
            return (None, None);
        }

        // Walk the queued blocks backwards, from the next checkpoint, to the
        // child of the previous checkpoint. (Except for the first range, where
        // we only check the genesis block.)

        // We just checked for a valid checkpoint range
        let next_checkpoint_height = self.get_next_checkpoint_height().unwrap();
        let next_checkpoint_hash = self.get_next_checkpoint_hash().unwrap();
        let current_range = self.current_checkpoint_range.unwrap();

        let qrange = self.queued.range(current_range).rev();
        // We could try to calculate the correct range here, but it's easier to
        // just limit the heights using `qrange.zip()`. We also make sure
        // BlockHeight can't underflow.
        let expected_heights = successors(Some(next_checkpoint_height), |n| {
            n.0.checked_sub(1).map(BlockHeight)
        });

        // The hash we expect this block to have
        let mut expected_hash = next_checkpoint_hash;
        // The height of the last block we checked
        let mut last_height = None;

        for ((&height, qblocks), expected_height) in qrange.zip(expected_heights) {
            // Check that the heights are continuous
            if height != expected_height {
                // There is a gap in the block chain
                // Missing blocks are not an error - wait for more blocks
                return (Some(expected_height), Some(expected_hash));
            }

            // Check if any queued block at this height is part of the chain
            // TODO(teor): consider using find_map() with a qblock method
            let parent_hash = qblocks
                .iter()
                .skip_while(|qblock| qblock.hash != expected_hash)
                .map(|qblock| qblock.block.header.previous_block_hash)
                .next();

            if let Some(parent_hash) = parent_hash {
                expected_hash = parent_hash;
                last_height = Some(height);
            } else {
                // There was no valid block in the queued block list
                // Missing blocks are not an error - wait for more blocks
                //
                // This check also handles zero-length vecs, which should be
                // impossible
                return (Some(expected_height), Some(expected_hash));
            }
        }

        // If the loop didn't execute, we're waiting for the next checkpoint block
        // TODO(teor): add tests for this case
        if last_height.is_none() {
            return (Some(next_checkpoint_height), Some(next_checkpoint_hash));
        }

        // At this point, last_height is the height of the last block, but
        // expected_hash is the hash of the last block's parent block.
        //

        // If we're at the genesis block, we want to return None for the parent
        // block height.
        let parent_height = last_height.unwrap().0.checked_sub(1).map(BlockHeight);
        (parent_height, Some(expected_hash))
    }

    /// Check all the blocks in the current checkpoint range. Send `Ok` for the
    /// blocks that are in the chain, and `Err` for side-chain blocks.
    ///
    /// Does nothing if verification has finished.
    ///
    /// Check that the current checkpoint range is complete before calling this
    /// function.
    fn submit_current_checkpoint_range(&mut self) {
        // If checkpoint verification has finished, there should be no queued blocks.
        if self.current_checkpoint_range.is_none() {
            return;
        }

        let next_checkpoint_height = self.get_next_checkpoint_height().unwrap();
        let current_range = self.current_checkpoint_range.unwrap();

        // Verify all the blocks and discard all the bad blocks in the current range.
        let mut expected_hash = self.get_next_checkpoint_hash().unwrap();
        let qrange = self.queued.range_mut(current_range).rev();
        for (_, qblocks) in qrange {
            // Find a queued block at each height that is part of the hash chain
            //
            // There are two possible outcomes here:
            //   - there is one block, and it matches the chain (the common case)
            //   - there are multiple blocks, and at least one block matches the chain
            //     (if there are duplicate blocks, one succeeds, and the others fail)

            // The caller should check for a continuous chain.
            debug_assert!(!qblocks.is_empty());

            let mut next_parent_hash = None;
            for qblock in qblocks.drain(..) {
                if qblock.hash == expected_hash {
                    if next_parent_hash == None {
                        // The first valid block at the current height
                        next_parent_hash = Some(qblock.block.header.previous_block_hash);
                        // TODO(teor): These futures are sent in reverse order. Make sure
                        // the overall verifier adds blocks to the state in height order.
                        // Sending can fail, but there's nothing we can do about it.
                        let _ = qblock.tx.send(Ok(qblock.hash));
                    } else {
                        // Reject duplicate blocks at the same height
                        let _ = qblock.tx.send(Err(
                            "duplicate valid blocks at this height, only one was chosen".into(),
                        ));
                    }
                } else {
                    // A bad block, that isn't part of the chain.
                    let _ = qblock.tx.send(Err(
                        "the block hash does not match the chained checkpoint hash".into(),
                    ));
                }
            }
            // Can't fail, we just checked it in the previous loop
            expected_hash = next_parent_hash.unwrap();
        }

        // Double-check that all the block lists are empty
        #[cfg(any(dev, test))]
        {
            let qrange = self.queued.range(current_range).rev();
            for (_, qblocks) in qrange {
                debug_assert_eq!(qblocks.len(), 0);
            }
        }

        // Now remove the empty vectors at all those heights
        self.drop_range(current_range);

        // Finally, update the checkpoint bounds
        // The heights have to be valid here, because this part of the chain is valid
        self.update_current_checkpoint_height(next_checkpoint_height);
    }

    /// Drop the queued blocks in `range`, and return an error for their
    /// futures based on `err_str`.
    ///
    /// Also clears the corresponding keys in `self.queued`.
    fn reject_range_with_error<R>(&mut self, range: R, err_str: &str)
    where
        R: RangeBounds<BlockHeight> + Clone,
    {
        let qrange = self.queued.range_mut(range.clone());
        for (_, qblocks) in qrange {
            for qblock in qblocks.drain(..) {
                // Sending can fail, but there's nothing we can do about it.
                let _ = qblock.tx.send(Err(err_str.to_string().into()));
            }
        }
        self.drop_range(range);

        // Do not update the current range.
        // Instead, let the caller decide what to do.
    }

    /// Drop the queued blocks in `range`.
    ///
    /// Does not reject their futures, use `reject_range_with_error()` for that.
    fn drop_range<R>(&mut self, range: R)
    where
        R: RangeBounds<BlockHeight>,
    {
        let drop_keys: Vec<BlockHeight> =
            self.queued.range(range).map(|(key, _value)| *key).collect();
        for k in drop_keys {
            self.queued.remove(&k);
        }
        // Do not update the current range.
        // Instead, let the caller decide what to do.
    }
}

/// CheckpointVerifier rejects pending futures on drop.
impl Drop for CheckpointVerifier {
    fn drop(&mut self) {
        self.reject_range_with_error(.., "checkpoint verifier was dropped");
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

        // Try to verify from the next checkpoint to the previous one.
        //
        // If this code shows up in profiles, we can try the following optimisations:
        //   - only check the chain when the length of the HashMap is greater than or equal to
        //     the length of self.current_checkpoint_range, or
        //   - cache the height of the last continuous chain as a new field in self, and start
        //     at that height during the next check.

        // Verification begins with the genesis block, or the first block after
        // the previous checkpoint.
        //
        // The parent of the genesis block has height `None`.
        let previous_checkpoint_height = self.get_previous_checkpoint_height();
        // The genesis block's previous hash field is all zeroes
        // TODO(teor): put this constant somewhere shared between both validators
        let previous_checkpoint_hash = self
            .get_previous_checkpoint_hash()
            .unwrap_or(BlockHeaderHash([0; 32]));

        // If checkpoint verification has finished, we should haved returned when
        // `insert_queued_block()` failed
        let (end_height, end_hash) = self.find_checkpoint_chain_end();
        // TODO(teor): rewrite as a match statement
        if end_height.is_some() && end_height > previous_checkpoint_height {
            // We need more blocks before we can checkpoint
            return Box::pin(rx.boxed());
        } else if end_height != previous_checkpoint_height {
            // This should be unreachable
            // If not, we are in an unrecoverable state
            // TODO(teor): return an error and stop checkpointing?
            panic!("the checkpoint verifier searched outside the current range");
        } else if end_hash.is_none() {
            // This should be unreachable
            // If not, we are in an unrecoverable state
            // TODO(teor): return an error and stop checkpointing?
            panic!("the checkpoint verifier tried to verify more blocks after finishing");
        }

        // At this point, we know the chain ended at the the previous checkpoint.
        // Verify the end_hash against the previous checkpoint hash.
        if end_hash != Some(previous_checkpoint_hash) {
            // We just checked for a valid checkpoint range
            let current_range = self.current_checkpoint_range.unwrap();

            // Somehow, we have a chain back from the next
            // checkpoint, which doesn't match the previous
            // checkpoint. This is either a checkpoint list
            // error, or a bug.
            self.reject_range_with_error(
                current_range,
                "the chain from the next checkpoint does not match the previous checkpoint",
            );

            // TODO(teor): Signal error to overall validator, and disable checkpoint verification?
            // Or keep verifying, and let the network sync try to find the correct blocks?
        }

        // Now we know that the hash matches the previous checkpoint, and we
        // have completed this part of the chain.
        self.submit_current_checkpoint_range();

        Box::pin(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{eyre, Report};
    use std::{cmp::min, mem::drop, time::Duration};
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
        let bad_verify_future_1 = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        // We can't await the future yet, because bad blocks aren't cleared
        // until the chain is verified

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
        let bad_verify_future_2 = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(bad_block0.clone()),
        );
        // We can't await the future yet, because bad blocks aren't cleared
        // until the chain is verified

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
        let good_verify_future = timeout(
            Duration::from_secs(VERIFY_TIMEOUT_SECONDS),
            ready_verifier_service.call(good_block0.clone()),
        );
        /// Wait for the response for good block 0, and expect success (3/3)
        // TODO(teor || jlusby): check error kind
        let verify_response = good_verify_future
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

        // Now, await the bad futures, which should have completed

        /// Wait for the response for block 0, and expect failure (1/3)
        // TODO(teor || jlusby): check error kind
        let _ = bad_verify_future_1
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .expect_err("bad block hash should fail");

        assert_eq!(
            checkpoint_verifier.get_previous_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(checkpoint_verifier.get_next_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        /// Wait for the response for block 0, and expect failure again (2/3)
        // TODO(teor || jlusby): check error kind
        let _ = bad_verify_future_2
            .await
            .expect("timeout should not happen")
            .expect("oneshot channel should not fail")
            .expect_err("bad block hash should fail");

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
    async fn checkpoint_drop_cancel() -> Result<(), Report> {
        zebra_test::init();

        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
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
            Some(BlockHeight(434873))
        );

        let mut futures = Vec::new();
        // Now collect verify futures for each block
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

            futures.push((verify_future, height, hash));

            // We can't easily calculate the next checkpoint height, but the previous
            // loop iteration uses the next checkpoint height function to set the
            // current height we're testing here

            // Only continuous checkpoints verify
            assert_eq!(
                checkpoint_verifier.get_previous_checkpoint_height(),
                Some(BlockHeight(min(height.0, 1)))
            );
            assert_eq!(
                checkpoint_verifier.get_max_checkpoint_height(),
                Some(BlockHeight(434873))
            );
        }

        // Now drop the verifier, to cancel the futures
        drop(checkpoint_verifier);

        for (verify_future, height, hash) in futures {
            /// Check the response for block {?height}
            let verify_response = verify_future
                .await
                .expect("timeout should not happen")
                .expect("oneshot channel should not fail");

            if height <= BlockHeight(1) {
                // The futures for continuous checkpoints should have succeeded before drop
                let verify_hash = verify_response.map_err(|e| eyre!(e))?;
                assert_eq!(verify_hash, hash);
            } else {
                // TODO(teor || jlusby): check error kind
                verify_response.expect_err("Pending futures should fail on drop");
            }
        }

        Ok(())
    }
}
