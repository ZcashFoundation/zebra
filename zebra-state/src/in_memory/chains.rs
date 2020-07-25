#![allow(dead_code, unused_variables)]
use super::Error;
use crate::{Request, Response};
use futures::prelude::*;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{Service, ServiceExt};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

/// A representation of the chain state at a given height
#[derive(Clone, Debug)]
struct ChainState {
    block: Arc<Block>,
    // TODO: add more state here
}

/// A persistent data structure representing the end of a chain
struct VolatileChain(im::OrdMap<BlockHeight, ChainState>);

impl VolatileChain {
    fn contains(&self, height: BlockHeight) -> bool {
        todo!()
    }
}

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

impl<S> ChainsState<S> {
    fn insert(&mut self, block: impl Into<Arc<Block>>) -> Result<BlockHeaderHash, Error> {
        let block = block.into();
        let hash = block.hash();
        let height = block.coinbase_height().unwrap();
        let parent_height = BlockHeight(height.0 - 1);

        for chain in self
            .chains
            .iter_mut()
            .filter(|chain| chain.contains(parent_height))
        {
            let parent_state = chain
                .0
                .get(&parent_height)
                .expect("block with one less height must exist");
            let parent_hash = parent_state.block.hash();

            if parent_hash != block.header.previous_block_hash {
                continue;
            }

            let was_present = if chain.contains(height) {
                chain.0.insert(height, ChainState { block })
            } else {
                let (mut shared, _forked) = chain.0.split(&height);
                let was_present = shared.insert(height, ChainState { block });

                self.chains.push(VolatileChain(shared));

                was_present
            }
            .is_some();

            if was_present {
                unreachable!("chain state should not already exist in this map");
            }

            return Ok(hash);
        }

        Err("parent hash not found in chain state")?
    }

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
            Request::AddBlock { block } => {
                let result = self.insert(block).map(|hash| {
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
            Request::GetBlock { hash } => todo!(),
            Request::GetTip => todo!(),
            Request::GetDepth { hash } => todo!(),
            Request::GetBlockLocator { genesis } => todo!(),
        }
    }
}
