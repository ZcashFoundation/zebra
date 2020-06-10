//! Block verification and chain state updates for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting blocks (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of the chain state (awaits a verified parent block)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.

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
/// After verification, blocks and their associated transactions are added to
/// `zebra_state::ZebraState`.
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
        let _: ZSF = self.state_service.call(zebra_state::Request::AddBlock {
            block: block.into(),
        });

        Box::pin(async move { Ok(header_hash) })
    }
}

/// Initialise the BlockVerifier service.
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
