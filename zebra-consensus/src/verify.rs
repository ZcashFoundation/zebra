//! Block verification and chain state updates for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (awaits a verified parent block)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

use futures_util::FutureExt;
use std::{
    error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};

use zebra_chain::block::{Block, BlockHeaderHash};

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
    S: Service<zebra_state::Request, Response = zebra_state::Response, Error = Error>,
    S::Future: Send + 'static,
{
    type Response = BlockHeaderHash;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.state_service.poll_ready(context)
    }

    fn call(&mut self, block: Arc<Block>) -> Self::Future {
        let header_hash: BlockHeaderHash = block.as_ref().into();

        // Ignore errors for now.
        // TODO(jlusby): Error = Report, handle errors from state_service.
        // TODO(teor):
        //   - handle chain reorgs, adjust state_service "unique block height" conditions
        //   - handle block validation errors (including errors in the block's transactions)
        //   - handle state_service AddBlock errors, and add unit tests for those errors

        // `state_service.call` is OK here because we already called
        // `state_service.poll_ready` in our `poll_ready`.
        let add_block = self
            .state_service
            .call(zebra_state::Request::AddBlock { block });

        async move {
            add_block.await?;
            Ok(header_hash)
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
        + 'static,
    S::Future: Send + 'static,
{
    Buffer::new(BlockVerifier { state_service }, 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::Report;
    use eyre::{bail, ensure, eyre};
    use tower::{util::ServiceExt, Service};
    use zebra_chain::serialization::ZcashDeserialize;

    fn install_tracing() {
        use tracing_error::ErrorLayer;
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::{fmt, EnvFilter};

        let fmt_layer = fmt::layer().with_target(false);
        let filter_layer = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn verify() -> Result<(), Report> {
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])?;
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
        install_tracing();

        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])?;
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
}
