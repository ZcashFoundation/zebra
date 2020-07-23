use super::Error;
use crate::{Request, Response};
use futures::prelude::*;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::Service;
use zebra_chain::{block::Block, types::BlockHeight};

struct ChainState {
    block: Arc<Block>,
}

type VolatileChain = im::OrdMap<BlockHeight, ChainState>;

/// A service wrapper that tracks multiple chains, handles reorgs, and persists
/// blocks to disk once they're past the reorg limit
pub(crate) struct ChainsState<S> {
    /// The inner state service that only tracks a single chain
    inner: S,
    /// The set of chains
    //
    // might need to use a map type and pop / reinsert with cummulative work as the index
    chains: Vec<VolatileChain>,
}

impl<S> Service<Request> for ChainsState<S> {
    type Response = Response;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        tracing::debug!(?req);
        match req {
            Request::AddBlock { block } => todo!(),
            Request::GetBlock { hash } => todo!(),
            Request::GetTip => todo!(),
            Request::GetDepth { hash } => todo!(),
            Request::GetBlockLocator { genesis } => todo!(),
        }
    }
}
