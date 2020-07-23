use super::Error;
use crate::{Request, Response};
use futures::prelude::*;
use futures::Future;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;
use zebra_chain::block::BlockHeaderHash;

mod block_index;

#[derive(Default)]
pub(super) struct InMemoryState {
    index: block_index::BlockIndex,
}

impl InMemoryState {
    fn contains(&mut self, _hash: BlockHeaderHash) -> Result<Option<u32>, Error> {
        todo!()
    }
}

impl Service<Request> for InMemoryState {
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
                let result = self
                    .index
                    .insert(block)
                    .map(|hash| Response::Added { hash });

                async { result }.boxed()
            }
            Request::GetBlock { hash } => {
                let result = self
                    .index
                    .get(hash)
                    .map(|block| Response::Block { block })
                    .ok_or_else(|| "block could not be found".into());

                async move { result }.boxed()
            }
            Request::GetTip => {
                let result = self
                    .index
                    .get_tip()
                    .map(|block| block.hash())
                    .map(|hash| Response::Tip { hash })
                    .ok_or_else(|| "zebra-state contains no blocks".into());

                async move { result }.boxed()
            }
            Request::GetDepth { hash } => {
                let res = self.contains(hash);

                async move {
                    let depth = res?;

                    Ok(Response::Depth(depth))
                }
                .boxed()
            }
            Request::GetBlockLocator { genesis } => {
                let tip = self.index.get_tip();
                let tip = match tip {
                    Some(tip) => tip,
                    None => {
                        return async move {
                            Ok(Response::BlockLocator {
                                block_locator: vec![genesis],
                            })
                        }
                        .boxed()
                    }
                };

                let tip_height = tip
                    .coinbase_height()
                    .expect("tip block will have a coinbase height");

                let block_locator = crate::block_locator_heights(tip_height)
                    .map(|height| {
                        self.index
                            .get_main_chain_at(height)
                            .expect("there should be no holes in the chain")
                    })
                    .collect();

                async move { Ok(Response::BlockLocator { block_locator }) }.boxed()
            }
        }
    }
}
