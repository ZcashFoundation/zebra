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

mod block_index;

#[derive(Default)]
struct InMemoryState {
    index: block_index::BlockIndex,
}

type Error = Box<dyn error::Error + Send + Sync + 'static>;

impl Service<Request> for InMemoryState {
    type Response = Response;
    type Error = Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
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
                    .map(|hash| Response::Tip { hash })
                    .ok_or_else(|| "zebra-state contains no blocks".into());

                async move { result }.boxed()
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
