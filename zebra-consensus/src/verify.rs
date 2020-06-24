//! Block verification and chain state updates for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (awaits a verified parent block)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use chrono::Utc;
use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service, ServiceExt};

use zebra_chain::block::{Block, BlockHeaderHash};

mod block;
mod script;
mod transaction;

struct BlockVerifier<S> {
    state_service: S,
}

/// The error type for the BlockVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The BlockVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<S> Service<Arc<Block>> for BlockVerifier<S>
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
        // TODO(teor):
        //   - handle chain reorgs
        //   - adjust state_service "unique block height" conditions
        let mut state_service = self.state_service.clone();

        async move {
            // Since errors cause an early exit, try to do the
            // quick checks first.

            let now = Utc::now();
            block::node_time_check(block.header.time, now)?;

            // `Tower::Buffer` requires a 1:1 relationship between `poll()`s
            // and `call()`s, because it reserves a buffer slot in each
            // `call()`.
            let add_block = state_service
                .ready_and()
                .await?
                .call(zebra_state::Request::AddBlock { block });

            match add_block.await? {
                zebra_state::Response::Added { hash } => Ok(hash),
                _ => Err("adding block to zebra-state failed".into()),
            }
        }
        .boxed()
    }
}

/// Return a block verification service, using the provided state service.
///
/// The block verifier holds a state service of type `S`, used as context for
/// block validation and to which newly verified blocks will be committed. This
/// state is pluggable to allow for testing or instrumentation.
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
) -> impl Service<
    Arc<Block>,
    Response = BlockHeaderHash,
    Error = Error,
    Future = impl Future<Output = Result<BlockHeaderHash, Error>>,
> + Send
       + Clone
       + 'static
where
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>
        + Send
        + Clone
        + 'static,
    S::Future: Send + 'static,
{
    Buffer::new(BlockVerifier { state_service }, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::{Duration, Utc};
    use color_eyre::eyre::Report;
    use color_eyre::eyre::{bail, ensure, eyre};
    use tower::{util::ServiceExt, Service};
    use zebra_chain::serialization::ZcashDeserialize;

    #[tokio::test]
    #[spandoc::spandoc]
    async fn verify() -> Result<(), Report> {
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();

        let state_service = Box::new(zebra_state::in_memory::init());
        let mut block_verifier = super::init(state_service);

        let verify_response = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(block.clone())
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            verify_response == hash,
            "unexpected response kind: {:?}",
            verify_response
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn round_trip() -> Result<(), Report> {
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();

        let mut state_service = zebra_state::in_memory::init();
        let mut block_verifier = super::init(state_service.clone());

        let verify_response = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(block.clone())
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            verify_response == hash,
            "unexpected response kind: {:?}",
            verify_response
        );

        let state_response = state_service
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock { hash })
            .await
            .map_err(|e| eyre!(e))?;

        match state_response {
            zebra_state::Response::Block {
                block: returned_block,
            } => assert_eq!(block, returned_block),
            _ => bail!("unexpected response kind: {:?}", state_response),
        }

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn verify_fail_add_block() -> Result<(), Report> {
        zebra_test::init();

        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();

        let mut state_service = zebra_state::in_memory::init();
        let mut block_verifier = super::init(state_service.clone());

        // Add the block for the first time
        let verify_response = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(block.clone())
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            verify_response == hash,
            "unexpected response kind: {:?}",
            verify_response
        );

        let state_response = state_service
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock { hash })
            .await
            .map_err(|e| eyre!(e))?;

        match state_response {
            zebra_state::Response::Block {
                block: returned_block,
            } => assert_eq!(block, returned_block),
            _ => bail!("unexpected response kind: {:?}", state_response),
        }

        // Now try to add the block again, verify should fail
        // TODO(teor): ignore duplicate block verifies?
        let verify_result = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(block.clone())
            .await;

        ensure!(
            // TODO(teor || jlusby): check error string
            verify_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        // But the state should still return the original block we added
        let state_response = state_service
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock { hash })
            .await
            .map_err(|e| eyre!(e))?;

        match state_response {
            zebra_state::Response::Block {
                block: returned_block,
            } => assert_eq!(block, returned_block),
            _ => bail!("unexpected response kind: {:?}", state_response),
        }

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn verify_fail_future_time() -> Result<(), Report> {
        let mut block =
            <Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

        let mut state_service = zebra_state::in_memory::init();
        let mut block_verifier = super::init(state_service.clone());

        // Modify the block's time
        // Changing the block header also invalidates the header hashes, but
        // those checks should be performed later in validation, because they
        // are more expensive.
        let three_hours_in_the_future = Utc::now()
            .checked_add_signed(Duration::hours(3))
            .ok_or("overflow when calculating 3 hours in the future")
            .map_err(|e| eyre!(e))?;
        block.header.time = three_hours_in_the_future;

        let arc_block: Arc<Block> = block.into();

        // Try to add the block, and expect failure
        let verify_result = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(arc_block.clone())
            .await;

        ensure!(
            // TODO(teor || jlusby): check error string
            verify_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        // Now make sure the block isn't in the state
        let state_result = state_service
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock {
                hash: arc_block.as_ref().into(),
            })
            .await;

        ensure!(
            // TODO(teor || jlusby): check error string
            state_result.is_err(),
            "unexpected result kind: {:?}",
            verify_result
        );

        Ok(())
    }
}
