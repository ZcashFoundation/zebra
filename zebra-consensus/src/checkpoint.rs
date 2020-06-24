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
    collections::HashMap,
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::types::BlockHeight;

struct CheckpointVerifier<S> {
    /// The underlying `ZebraState`.
    state_service: S,

    /// Each checkpoint consists of a coinbase height and block header hash.
    ///
    /// Checkpoints should be chosen to avoid forks or chain reorganizations,
    /// which only happen in the last few hundred blocks in the chain.
    /// (zcashd allows chain reorganizations up to 99 blocks, and prunes
    /// orphaned side-chains after 288 blocks.)
    checkpoint_list: Arc<HashMap<BlockHeight, BlockHeaderHash>>,
}

/// The error type for the CheckpointVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

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

        async move {
            if checkpoint_list.is_empty() {
                return Err("the checkpoint list is empty".into());
            };

            let block_height = block
                .coinbase_height()
                .ok_or("the block does not have a coinbase height")?;

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
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

// TODO(teor):
//   - add a function for the maximum checkpoint height
//     (We can pre-calculate the result in init(), if we want.)
//   - check that block.coinbase_height() <= max_checkpoint_height

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
pub fn init<S>(
    state_service: S,
    checkpoint_list: impl Into<Arc<HashMap<BlockHeight, BlockHeaderHash>>>,
) -> impl Service<
    Arc<Block>,
    Response = BlockHeaderHash,
    Error = Error,
    Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
> + Send
       + 'static
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    CheckpointVerifier {
        state_service,
        checkpoint_list: checkpoint_list.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::{bail, ensure, eyre, Report};
    use tower::{util::ServiceExt, Service};

    use zebra_chain::serialization::ZcashDeserialize;

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_single_item_list() -> Result<(), Report> {
        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let hash0: BlockHeaderHash = block0.as_ref().into();

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: HashMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), hash0)]
                .iter()
                .cloned()
                .collect();

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier = super::init(state_service.clone(), genesis_checkpoint_list);

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

        ensure!(
            verify_response == hash0,
            "unexpected response kind: {:?}",
            verify_response
        );

        /// Make sure the state service is ready
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure the block was added to the state
        let state_response = ready_state_service
            .call(zebra_state::Request::GetBlock { hash: hash0 })
            .await
            .map_err(|e| eyre!(e))?;

        match state_response {
            zebra_state::Response::Block {
                block: returned_block,
            } => assert_eq!(block0, returned_block),
            _ => bail!("unexpected response kind: {:?}", state_response),
        }

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_not_present_fail() -> Result<(), Report> {
        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let block415000 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

        // Make a checkpoint list containing only the genesis block
        let genesis_checkpoint_list: HashMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), block0.as_ref().into())]
                .iter()
                .cloned()
                .collect();

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier = super::init(state_service.clone(), genesis_checkpoint_list);

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Try to verify block 415000, and expect failure
        // TODO(teor || jlusby): check error kind
        ready_verifier_service
            .call(block415000.clone())
            .await;

        ensure!(
            // TODO(teor || jlusby): check error kind
            verify_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );


        /// Make sure the state service is ready (1/2)
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure neither block is in the state: expect GetBlock 415000 to fail.
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block415000.as_ref().into(),
            })
            .await;

        ensure!(
            // TODO(teor || jlusby): check error kind
            state_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        /// Make sure the state service is ready (2/2)
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Make sure neither block is in the state: expect GetBlock 0 to fail.
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block0.as_ref().into(),
            })
            .await;

        ensure!(
            // TODO(teor || jlusby): check error kind
            state_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn checkpoint_wrong_hash_fail() -> Result<(), Report> {
        let block0 =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;

        // Make a checkpoint list containing the genesis block height,
        // but use the wrong hash
        let genesis_checkpoint_list: HashMap<BlockHeight, BlockHeaderHash> =
            [(block0.coinbase_height().unwrap(), BlockHeaderHash([0; 32]))]
                .iter()
                .cloned()
                .collect();

        let mut state_service = Box::new(zebra_state::in_memory::init());
        let mut checkpoint_verifier = super::init(state_service.clone(), genesis_checkpoint_list);

        /// Make sure the verifier service is ready
        let ready_verifier_service = checkpoint_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?;
        /// Try to verify block 0, and expect failure
        // TODO(teor || jlusby): check error kind
        ready_verifier_service
            .call(block0.clone())
            .await;

        ensure!(
            // TODO(teor || jlusby): check error kind
            verify_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        /// Make sure the state service is ready
        let ready_state_service = state_service.ready_and().await.map_err(|e| eyre!(e))?;
        /// Now make sure block 0 is not in the state
        // TODO(teor || jlusby): check error kind
        ready_state_service
            .call(zebra_state::Request::GetBlock {
                hash: block0.as_ref().into(),
            })
            .await;

        ensure!(
            // TODO(teor || jlusby): check error kind
            state_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        Ok(())
    }
}
