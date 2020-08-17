//! A basic implementation of the zebra-state service entirely in memory
//!
//! This service is provided as an independent implementation of the
//! zebra-state service to use in verifying the correctness of `on_disk`'s
//! `Service` implementation.
use super::{Request, Response};
use futures::prelude::*;
use std::{
    error,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};
use zebra_chain::block;

mod block_index;

#[derive(Default)]
struct InMemoryState {
    index: block_index::BlockIndex,
}

impl InMemoryState {
    fn contains(&mut self, _hash: block::Hash) -> Result<Option<u32>, Error> {
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

/// Return's a type that implement's the `zebra_state::Service` entirely in
/// memory using `HashMaps`
pub fn init() -> impl Service<
    Request,
    Response = Response,
    Error = Error,
    Future = impl Future<Output = Result<Response, Error>>,
> + Send
       + Clone
       + 'static {
    Buffer::new(InMemoryState::default(), 1)
}

type Error = Box<dyn error::Error + Send + Sync + 'static>;
