//! Checkpoint-based block verification for Zebra.
//!
//! Checkpoint-based verification uses a list of checkpoint hashes to speed up the
//! initial chain sync for Zebra. This list is distributed with Zebra.
//!
//! The CheckpointVerifier compares each block's `BlockHeaderHash` against the known
//! checkpoint hashes. If it matches, then the block is verified, and added to the
//! `ZebraState`. Otherwise, if the block's height is lower than the maximum checkpoint
//! height, the block awaits the verification of its child block.
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
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

/// A list of unverified blocks at a particular height.
///
/// Typically contains zero or one block.
type BlockList = Vec<Arc<Block>>;

/// A checkpointing block verifier.
///
/// Verifies blocks using a supplied list of checkpoints. There must be at
/// least one checkpoint for the genesis block.
struct CheckpointVerifier<S> {
    // Inputs
    //
    /// The underlying `ZebraState`, possibly wrapped in other services.
    state_service: S,

    /// Each checkpoint consists of a coinbase height and block header hash.
    ///
    /// Checkpoints should be chosen to avoid forks or chain reorganizations,
    /// which only happen in the last few hundred blocks in the chain.
    /// (zcashd allows chain reorganizations up to 99 blocks, and prunes
    /// orphaned side-chains after 288 blocks.)
    ///
    /// There must be a checkpoint for the genesis block at BlockHeight 0.
    /// (All other checkpoints are optional.)
    checkpoint_list: Arc<BTreeMap<BlockHeight, BlockHeaderHash>>,

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
    queued: Arc<Mutex<BTreeMap<BlockHeight, BlockList>>>,

    /// The height of the most recently verified checkpoint.
    ///
    /// `None` means that checkpoint verification has not started yet. The next
    /// checkpoint to be verified is the genesis checkpoint.
    ///
    /// If the current checkpoint height is equal to the maximum checkpoint
    /// height, checkpoint verification has finished.
    current_checkpoint_height: Arc<Mutex<Option<BlockHeight>>>,
}

/// The error type for CheckpointVerifier.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The CheckpointVerifier implementation.
///
/// Contains non-service utility functions for CheckpointVerifiers.
impl<S> CheckpointVerifier<S> {
    /// Return a checkpoint verification service, using the provided state service.
    ///
    /// The checkpoint verifier holds a state service of type `S`, into which newly
    /// verified blocks will be committed. This state is pluggable to allow for
    /// testing or instrumentation.
    ///
    /// The returned type is opaque to allow instrumentation or other wrappers, but
    /// can be boxed for storage. It is also `Clone` to allow sharing of a
    /// verification service.
    ///
    /// This function should be called only once for a particular state service (and
    /// the result be shared) rather than constructing multiple verification services
    /// backed by the same state layer.
    //
    // Currently only used in tests.
    //
    // We'll use this function in the overall verifier, which will split blocks
    // between BlockVerifier and CheckpointVerifier.
    #[cfg(test)]
    fn new(
        state_service: S,
        checkpoint_list: impl Into<Arc<BTreeMap<BlockHeight, BlockHeaderHash>>>,
    ) -> Result<Self, Error>
    where
        S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
            + Send
            + Clone
            + 'static,
        S::Future: Send + 'static,
    {
        let checkpoints: Arc<BTreeMap<BlockHeight, BlockHeaderHash>> = checkpoint_list.into();

        // An empty checkpoint list can't actually verify any blocks.
        match checkpoints.keys().cloned().next() {
            None => {
                return Err("there must be at least one checkpoint, for the genesis block".into())
            }
            Some(BlockHeight(0)) => {}
            _ => return Err("checkpoints must start at the genesis block height 0".into()),
        };

        Ok(CheckpointVerifier {
            state_service,
            checkpoint_list: checkpoints,
            queued: Arc::new(Mutex::new(<BTreeMap<BlockHeight, BlockList>>::new())),
            current_checkpoint_height: Arc::new(Mutex::new(None)),
        })
    }
}

// These functions are not methods, because they are used in async blocks.
// And we don't want to share the whole CheckpointVerifier in `&self`.
// They are not associated functions, to avoid spurious generics.

/// Return the block height of the highest checkpoint in `checkpoint_list`.
///
/// If there is only a single checkpoint, then the maximum height will be
/// zero. (The genesis block.)
///
/// The maximum height is constant for each checkpoint list.
///
/// This function returns None if there are no checkpoints.
fn get_max_checkpoint_height(
    checkpoint_list: Arc<BTreeMap<BlockHeight, BlockHeaderHash>>,
) -> Option<BlockHeight> {
    checkpoint_list.keys().cloned().next_back()
}

/// Return the block height inside the shared `current_checkpoint_height`,
/// by acquiring the mutex lock.
///
/// The current checkpoint increases as blocks are verified.
///
/// If verification has not started yet, the current checkpoint will be None.
/// If verification has finished, returns the maximum checkpoint height.
//
// Currently only used in tests.
#[cfg(test)]
fn get_current_checkpoint_height(
    current_checkpoint_height: Arc<Mutex<Option<BlockHeight>>>,
) -> Option<BlockHeight> {
    *current_checkpoint_height.lock().unwrap()
}

/// Return the height of the next checkpoint after `block_height` in
/// `checkpoint_list`.
///
/// If `block_height` is None, assume verification has not started yet,
/// and return zero (the genesis block).
///
/// Returns None if there are no checkpoints.
/// Returns the maximum height if verification has finished, or the
/// block height is greater than or equal to the maximum height.
fn get_next_checkpoint_height(
    block_height: Option<BlockHeight>,
    checkpoint_list: Arc<BTreeMap<BlockHeight, BlockHeaderHash>>,
) -> Option<BlockHeight> {
    match block_height {
        None => checkpoint_list.keys().cloned().next(),
        Some(height) => checkpoint_list
            .range((Excluded(height), Unbounded))
            .next()
            .map_or_else(
                || get_max_checkpoint_height(checkpoint_list.clone()),
                |(height, _hash)| Some(*height),
            ),
    }
}

/// Increase `current_checkpoint_height` to `verified_block_height`,
/// if `verified_block_height` is the next checkpoint height.
fn update_current_checkpoint_height(
    current_checkpoint_height: Arc<Mutex<Option<BlockHeight>>>,
    checkpoint_list: Arc<BTreeMap<BlockHeight, BlockHeaderHash>>,
    verified_block_height: BlockHeight,
) {
    let mut current_height = current_checkpoint_height.lock().unwrap();

    // Ignore blocks that are below the current checkpoint.
    //
    // We ignore out-of-order verification, such as:
    //  - the height is less than the current checkpoint height, or
    //  - the current checkpoint height is the maximum height (checkpoint verifies are finished),
    // because futures might not resolve in height order.
    if let Some(current) = *current_height {
        if verified_block_height <= current {
            return;
        }
    }

    // Ignore updates if the checkpoint list is empty, or verification has finished.
    if let Some(next_height) = get_next_checkpoint_height(*current_height, checkpoint_list) {
        if verified_block_height == next_height {
            *current_height = Some(next_height);
        }
    }
}

/// The CheckpointVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<S> Service<Arc<Block>> for CheckpointVerifier<S>
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        // We don't expect the state to exert backpressure on verifier users,
        // so we don't need to call `state_service.poll_ready()` here.
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        // TODO(jlusby): Error = Report, handle errors from state_service.
        let mut state_service = self.state_service.clone();
        let checkpoint_list = self.checkpoint_list.clone();
        let current_checkpoint_height = self.current_checkpoint_height.clone();

        async move {
            let block_height = block
                .coinbase_height()
                .ok_or("the block does not have a coinbase height")?;
            let max_checkpoint_height = get_max_checkpoint_height(checkpoint_list.clone())
                .ok_or("the checkpoint list is empty")?;
            if block_height > max_checkpoint_height {
                return Err("the block is higher than the maximum checkpoint".into());
            }

            // TODO(teor):
            //   - implement chaining from checkpoints to their ancestors
            //   - should the state contain a mapping from previous_block_hash to block?
            let checkpoint_hash = match checkpoint_list.get(&block_height) {
                Some(&hash) => hash,
                None => return Err("the block's height is not a checkpoint height".into()),
            };

            // Hashing is expensive, so we do it as late as possible
            if BlockHeaderHash::from(block.as_ref()) != checkpoint_hash {
                // The block is on a side-chain
                return Err("the block hash does not match the checkpoint hash".into());
            }

            // `Tower::Buffer` requires a 1:1 relationship between `poll()`s
            // and `call()`s, because it reserves a buffer slot in each
            // `call()`.
            let add_block = state_service
                .ready_and()
                .await?
                .call(zebra_state::Request::AddBlock {
                    block: block.clone(),
                });

            match add_block.await? {
                zebra_state::Response::Added { hash } => {
                    update_current_checkpoint_height(
                        current_checkpoint_height,
                        checkpoint_list,
                        block_height,
                    );
                    Ok(hash)
                }
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{bail, eyre, Report};
    use tower::{util::ServiceExt, Service};

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

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier =
            CheckpointVerifier::new(state_service.clone(), genesis_checkpoint_list)
                .map_err(|e| eyre!(e))?;

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
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
            .map_err(|e| eyre!(e))?;

        assert_eq!(verify_response, hash0);

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
            Some(BlockHeight(0))
        );

        /// Make sure the state service is ready
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure the block was added to the state
        let state_response = ready_state_service
            .call(zebra_state::Request::GetBlock { hash: hash0 })
            .await
            .map_err(|e| eyre!(e))?;

        if let zebra_state::Response::Block {
            block: returned_block,
        } = state_response
        {
            assert_eq!(block0, returned_block);
        } else {
            bail!("unexpected response kind: {:?}", state_response);
        }

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

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier =
            CheckpointVerifier::new(state_service.clone(), checkpoint_list)
                .map_err(|e| eyre!(e))?;

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
            Some(BlockHeight(434873))
        );

        // Now verify each block, and check the state
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
                .map_err(|e| eyre!(e))?;

            assert_eq!(verify_response, hash);

            /// Make sure the state service is ready
            let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
            /// Make sure the block was added to the state
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

            // The previous loop iteration uses the next checkpoint height function
            // to set this current height
            assert_eq!(
                get_current_checkpoint_height(
                    checkpoint_verifier.current_checkpoint_height.clone()
                ),
                Some(height)
            );
            assert_eq!(
                get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
                Some(BlockHeight(434873))
            );
        }

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            Some(BlockHeight(434873))
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(434873))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
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

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier =
            CheckpointVerifier::new(state_service.clone(), genesis_checkpoint_list)
                .map_err(|e| eyre!(e))?;

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
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
            .unwrap_err();

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
            Some(BlockHeight(0))
        );

        /// Make sure the state service is ready (1/2)
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure neither block is in the state: expect GetBlock 415000 to fail.
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block415000.as_ref().into(),
            })
            .await
            .unwrap_err();

        /// Make sure the state service is ready (2/2)
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure neither block is in the state: expect GetBlock 0 to fail.
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block0.as_ref().into(),
            })
            .await
            .unwrap_err();

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

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier =
            CheckpointVerifier::new(state_service.clone(), genesis_checkpoint_list)
                .map_err(|e| eyre!(e))?;

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
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
            .unwrap_err();

        assert_eq!(
            get_current_checkpoint_height(checkpoint_verifier.current_checkpoint_height.clone()),
            None
        );
        assert_eq!(
            get_next_checkpoint_height(
                *checkpoint_verifier
                    .current_checkpoint_height
                    .lock()
                    .unwrap(),
                checkpoint_verifier.checkpoint_list.clone()
            ),
            Some(BlockHeight(0))
        );
        assert_eq!(
            get_max_checkpoint_height(checkpoint_verifier.checkpoint_list.clone()),
            Some(BlockHeight(0))
        );

        /// Make sure the state service is ready
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Now make sure block 0 is not in the state
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block0.as_ref().into(),
            })
            .await
            .unwrap_err();

        Ok(())
    }
}
