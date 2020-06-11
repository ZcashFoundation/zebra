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
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};

use zebra_chain::block::{Block, BlockHeaderHash};

mod script;
mod transaction;

/// The trait constraints that we expect from `zebra_state::ZebraState` errors.
type ZSE = Box<dyn error::Error + Send + Sync + 'static>;
/// The trait constraints that we expect from the `zebra_state::ZebraState` service.
/// `ZSF` is the `Future` type for `zebra_state::ZebraState`. This type is generic,
/// because `tower::Service` function calls require a `Sized` future type.
type ZS<ZSF> = Box<
    dyn Service<zebra_state::Request, Response = zebra_state::Response, Error = ZSE, Future = ZSF>
        + Send
        + 'static,
>;

/// Block verification service.
///
/// After verification, blocks are added to `state_service`. We use a generic
/// future `ZSF` for that service, so that the underlying state service can be
/// wrapped in other services as needed.
struct BlockVerifier<ZSF>
where
    ZSF: Future<Output = Result<zebra_state::Response, ZSE>> + Send + 'static,
{
    state_service: ZS<ZSF>,
}

/// The result type for the BlockVerifier Service.
type Response = BlockHeaderHash;

/// The error type for the BlockVerifier Service.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// The BlockVerifier service implementation.
///
/// After verification, blocks are added to the underlying state service.
impl<ZSF> Service<Block> for BlockVerifier<ZSF>
where
    ZSF: Future<Output = Result<zebra_state::Response, ZSE>> + Send + 'static,
{
    type Response = Response;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.state_service.poll_ready(context)
    }

    fn call(&mut self, block: Block) -> Self::Future {
        let header_hash: BlockHeaderHash = (&block).into();

        // Ignore errors for now.
        // TODO(jlusby): Error = Report, handle errors from state_service.
        // TODO(teor):
        //   - handle chain reorgs, adjust state_service "unique block height" conditions
        //   - handle block validation errors (including errors in the block's transactions)
        //   - handle state_service AddBlock errors, and add unit tests for those errors

        // `state_service.call` is OK here because we already called
        // `state_service.poll_ready` in our `poll_ready`.
        let add_block = self.state_service.call(zebra_state::Request::AddBlock {
            block: block.into(),
        });

        async move {
            add_block.await?;
            Ok(header_hash)
        }
        .boxed()
    }
}

/// Initialise the BlockVerifier service.
///
/// We use a generic type `ZS<ZSF>` for `state_service`, so that the
/// underlying state service can be wrapped in other services as needed.
/// For similar reasons, we also return a dynamic service type.
pub fn init<ZSF>(
    state_service: ZS<ZSF>,
) -> impl Service<
    Block,
    Response = Response,
    Error = Error,
    Future = impl Future<Output = Result<Response, Error>>,
> + Send
       + Clone
       + 'static
where
    ZSF: Future<Output = Result<zebra_state::Response, ZSE>> + Send + 'static,
{
    Buffer::new(BlockVerifier::<ZSF> { state_service }, 1)
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

    /// Initialise and return an unwrapped `BlockVerifier`.
    fn init_block_verifier<ZSF>(state_service: ZS<ZSF>) -> BlockVerifier<ZSF>
    where
        ZSF: Future<Output = Result<zebra_state::Response, ZSE>> + Send + 'static,
    {
        BlockVerifier::<ZSF> { state_service }
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn verify() -> Result<(), Report> {
        let block = Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])?;
        // TODO(teor): why does rustc say that _hash is unused?
        let _hash: BlockHeaderHash = (&block).into();

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
            matches!(verify_response, _hash),
            "unexpected response kind: {:?}",
            verify_response
        );

        Ok(())
    }

    #[tokio::test]
    #[spandoc::spandoc]
    async fn round_trip() -> Result<(), Report> {
        install_tracing();

        let block = Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        // TODO(teor): why does rustc say that _hash is unused?
        let _hash: BlockHeaderHash = (&block).into();

        let state_service = Box::new(zebra_state::in_memory::init());
        let mut block_verifier = init_block_verifier(state_service);

        let verify_response = block_verifier
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(block.clone())
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            matches!(verify_response, _hash),
            "unexpected response kind: {:?}",
            verify_response
        );

        let state_response = block_verifier
            .state_service
            .ready_and()
            .await
            .map_err(|e| eyre!(e))?
            .call(zebra_state::Request::GetBlock { hash: _hash })
            .await
            .map_err(|e| eyre!(e))?;

        match state_response {
            zebra_state::Response::Block {
                block: returned_block,
            } => assert_eq!(&block, returned_block.as_ref()),
            _ => bail!("unexpected response kind: {:?}", state_response),
        }

        Ok(())
    }
}
