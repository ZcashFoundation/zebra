use super::{Request, Response};
use crate::config::Config;
use block_index::BlockIndex;
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
struct SledState {
    index: BlockIndex,
}

impl SledState {
    fn new(config: &Config) -> Self {
        Self {
            index: BlockIndex::new(config),
        }
    }
}

type Error = Box<dyn error::Error + Send + Sync + 'static>;

impl Service<Request> for SledState {
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
                let mut storage = self.index.clone();

                async move { storage.insert(block).map(|hash| Response::Added { hash }) }.boxed()
            }
            Request::GetBlock { hash } => {
                let storage = self.index.clone();
                async move {
                    storage
                        .get(hash)?
                        .map(|block| Response::Block { block })
                        .ok_or_else(|| "block could not be found".into())
                }
                .boxed()
            }
            Request::GetTip => {
                let storage = self.index.clone();
                async move {
                    storage
                        .get_tip()?
                        .map(|hash| Response::Tip { hash })
                        .ok_or_else(|| "zebra-state contains no blocks".into())
                }
                .boxed()
            }
        }
    }
}

pub fn init(
    config: Config,
) -> impl Service<
    Request,
    Response = Response,
    Error = Error,
    Future = impl Future<Output = Result<Response, Error>>,
> + Send
       + Clone
       + 'static {
    Buffer::new(SledState::new(&config), 1)
}
