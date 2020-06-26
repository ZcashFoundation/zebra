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
    ops::Bound::*,
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

/// A checkpointing block verifier.
///
/// Verifies blocks using a supplied list of checkpoints. There must be at
/// least one checkpoint for the genesis block.
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

    /// The height of the most recently verified checkpoint.
    ///
    /// `None` means that checkpoint verification has not started yet, and the
    /// next checkpoint to be verified is the genesis checkpoint.
    ///
    /// If the current checkpoint height is equal to the maximum checkpoint
    /// height, checkpoint verification has finished.
    current_checkpoint_height: Option<BlockHeight>,
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
            current_checkpoint_height: None,
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

    /// Return the current checkpoint height.
    ///
    /// The current checkpoint increases as blocks are verified.
    ///
    /// If verification has not started yet, the current checkpoint will be None.
    /// If verification has finished, returns the maximum checkpoint height.
    //
    // Currently only used in tests.
    #[cfg(test)]
    fn get_current_checkpoint_height(&self) -> Option<BlockHeight> {
        self.current_checkpoint_height
    }

    /// Return the height of the next checkpoint higher than `after_block_height`
    /// in the checkpoint list.
    ///
    /// If `after_block_height` is None, assume verification has not started yet,
    /// and return zero (the genesis block).
    ///
    /// Returns None if there are no checkpoints.
    /// Returns the maximum height if `after_block_height` is greater than or equal
    /// to the maximum height.
    fn get_next_checkpoint_height(
        &self,
        after_block_height: Option<BlockHeight>,
    ) -> Option<BlockHeight> {
        match after_block_height {
            None => self.checkpoint_list.keys().cloned().next(),
            Some(height) => self
                .checkpoint_list
                .range((Excluded(height), Unbounded))
                .next()
                .map_or_else(
                    || self.get_max_checkpoint_height(),
                    |(height, _hash)| Some(*height),
                ),
        }
    }

    /// Increase the current checkpoint height to `verified_block_height`,
    /// if `verified_block_height` is the next checkpoint height.
    fn update_current_checkpoint_height(&mut self, verified_block_height: BlockHeight) {
        // Ignore blocks that are below the current checkpoint.
        //
        // We ignore out-of-order verification, such as:
        //  - the height is less than the current checkpoint height, or
        //  - the current checkpoint height is the maximum height (checkpoint verifies are finished),
        // because futures might not resolve in height order.
        if let Some(current) = self.current_checkpoint_height {
            if verified_block_height <= current {
                return;
            }
        }

        // Ignore updates if the checkpoint list is empty, or verification has finished.
        if let Some(next_height) = self.get_next_checkpoint_height(self.current_checkpoint_height) {
            if verified_block_height == next_height {
                self.current_checkpoint_height = Some(next_height);
            }
        }
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

        // Check for a valid height
        let height = match self.check_block_height(block.clone()) {
            Ok(height) => height,
            Err(error) => {
                // Sending can not fail, because the matching rx is still in this scope.
                tx.send(Err(error)).unwrap();
                return Box::pin(rx.boxed());
            }
        };

        // Queue the block for verification
        // Verification will finish when the chain to the next checkpoint is complete
        let new_qblock = QueuedBlock {
            block: block.clone(),
            hash: block.as_ref().into(),
            tx,
        };

        // Add this block to the list of queued blocks at this height
        self.queued.entry(height).or_default().push(new_qblock);

        // TODO(teor):
        //   - implement chaining from checkpoints to their ancestors
        //   - should the state contain a mapping from previous_block_hash to block?
        let checkpoint_hash = match self.checkpoint_list.get(&height) {
            Some(&hash) => hash,
            None => return Box::pin(rx.boxed()),
        };

        let mut matching_qblocks = Vec::new();
        // Get the blocks at this height back out of the queue
        for potential_qblock in self.queued.entry(height).or_default().drain(..) {
            if potential_qblock.hash != checkpoint_hash {
                // The block is on a side-chain
                // Sending can fail, but only because the receiver has closed
                // the channel. So there's nothing we can do about the error.
                let _ = potential_qblock.tx.send(Err(
                    "the block hash does not match the checkpoint hash".into(),
                ));
            } else {
                matching_qblocks.push(potential_qblock);
            }
        }

        // Now process the matching blocks (there should be at most one)
        for matching_qblock in matching_qblocks.drain(..) {
            self.update_current_checkpoint_height(height);
            // Sending can fail, but there's nothing we can do about it.
            let _ = matching_qblock.tx.send(Ok(matching_qblock.hash));
        }

        Box::pin(rx.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{eyre, Report};
    use tower::{Service, ServiceExt};

    use zebra_chain::serialization::ZcashDeserialize;

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_single_item_list() -> Result<(), Report> {
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

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
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
        /// Verify block 0
        let verify_response = ready_verifier_service
            .call(block0.clone())
            .await
            .expect("oneshot channel should not fail")
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, hash0);

        assert_eq!(
            checkpoint_verifier.get_current_checkpoint_height(),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
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
    async fn checkpoint_multi_item_list() -> Result<(), Report> {
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

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(434873))
        );

        // Now verify each block
        for (block, height, hash) in checkpoint_data {
            /// Make sure the verifier service is ready
            let ready_verifier_service = checkpoint_verifier
                .ready_and()
                .await
                .map_err(|e| eyre!(e))?;

            /// Verify the block
            let verify_response = ready_verifier_service
                .call(block.clone())
                .await
                .expect("oneshot channel should not fail")
                .map_err(|e| eyre!(e))?;

            assert_eq!(verify_response, hash);

            // We can't easily calculate the next checkpoint height, but the previous
            // loop iteration uses the next checkpoint height function to set the
            // current height we're testing here
            assert_eq!(
                checkpoint_verifier.get_current_checkpoint_height(),
                Some(height)
            );
            assert_eq!(
                checkpoint_verifier.get_max_checkpoint_height(),
                Some(BlockHeight(434873))
            );
        }

        assert_eq!(
            checkpoint_verifier.get_current_checkpoint_height(),
            Some(BlockHeight(434873))
        );
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
            Some(BlockHeight(434873))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(434873))
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_not_present_fail() -> Result<(), Report> {
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

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
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
        /// Try to verify block 415000, and expect failure
        // TODO(teor || jlusby): check error kind
        ready_verifier_service
            .call(block415000.clone())
            .await
            .expect("oneshot channel should not fail")
            .unwrap_err();

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
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
    async fn checkpoint_wrong_hash_fail() -> Result<(), Report> {
        zebra_test::init();

        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;

        // Make a checkpoint list containing the genesis block height,
        // but use the wrong hash
        let genesis_checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), BlockHeaderHash([0; 32]))]
                .iter()
                .cloned()
                .collect();

        let mut checkpoint_verifier =
            CheckpointVerifier::new(genesis_checkpoint_list).map_err(|e| eyre!(e))?;

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
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
        /// Try to verify block 0, and expect failure
        // TODO(teor || jlusby): check error kind
        ready_verifier_service
            .call(block0.clone())
            .await
            .expect("oneshot channel should not fail")
            .unwrap_err();

        assert_eq!(checkpoint_verifier.get_current_checkpoint_height(), None);
        assert_eq!(
            checkpoint_verifier
                .get_next_checkpoint_height(checkpoint_verifier.get_current_checkpoint_height()),
            Some(BlockHeight(0))
        );
        assert_eq!(
            checkpoint_verifier.get_max_checkpoint_height(),
            Some(BlockHeight(0))
        );

        Ok(())
    }
}
