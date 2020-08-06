#![allow(dead_code, unused_variables)]
use super::Error;
use crate::{Request, Response};
use futures::prelude::*;
use std::{
    collections::BTreeSet,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};
use zebra_chain::{block::Block, types::BlockHeight};

/// A representation of the chain state at a given height
#[derive(Clone, Debug)]
struct ChainState {
    block: Arc<Block>,
    // TODO: add more state here
}

/// A persistent data structure representing the end of a chain
#[derive(Debug, Clone)]
struct Chain(im::OrdMap<BlockHeight, ChainState>);

/// Representation of the full chain context at a certain block
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainContext {
    recent: Option<Chain>,
    // TODO include a reference to the inner service so we can query the disk
    // for context that we don't have in the recent chain.
}

impl ChainContext {
    fn append(self, block: Arc<Block>) -> Chain {
        todo!()
    }
}

impl PartialEq for Chain {
    fn eq(&self, other: &Self) -> bool {
        todo!()
    }
}

impl Eq for Chain {}

impl PartialOrd for Chain {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        todo!()
    }
}

impl Ord for Chain {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        todo!()
    }
}

impl Chain {
    fn contains(&self, height: BlockHeight) -> bool {
        todo!()
    }
}

struct ChainSet(BTreeSet<Chain>);

impl ChainSet {
    fn insert(&mut self, parent: ChainContext, block: Arc<Block>) -> Result<(), Error> {
        if let Some(chain) = parent.recent.as_ref() {
            if self.0.contains(chain) {
                self.0.remove(chain);
            }
        }

        let chain = parent.append(block);

        self.0.insert(chain);

        Ok(())
    }
}

/// A service wrapper that tracks multiple chains, handles reorgs, and persists
/// blocks to disk once they're past the reorg limit
pub(crate) struct ChainsState<S> {
    /// The inner state service that only tracks a single chain
    inner: S,
    /// The set of chains
    //
    // might need to use a map type and pop / reinsert with (TODO based on the final design)
    chains: ChainSet,
}

impl<S> ChainsState<S> {
    /// Remove blocks from chains that should be persisted to the storage layer.
    fn pop_finalized_block(&mut self) -> Option<Arc<Block>> {
        todo!()
    }
}

impl<S> Service<Request> for ChainsState<S>
where
    S: Service<Request, Response = Response, Error = Error>,
    S: Clone + Send + 'static,
    S::Future: Send + 'static,
{
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
            Request::CommitBlock { block, context } => {
                let hash = block.hash();
                let result = self.chains.insert(context, block);
                let result = result.map(|()| {
                    let finalized = self.pop_finalized_block();
                    (hash, finalized)
                });
                let mut storage = self.inner.clone();

                async move {
                    let (hash, finalized_block) = result?;

                    if let Some(block) = finalized_block {
                        storage
                            .ready_and()
                            .await?
                            .call(Request::AddBlock { block })
                            .await
                            .expect(
                                "block has already been validated and should insert without errors",
                            );
                    }

                    Ok(Response::Added { hash })
                }
                .boxed()
            }
            Request::GetChainContext { hash } => todo!(),
        }
    }
}
