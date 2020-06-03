use super::{Request, Response};
use futures::prelude::*;
use std::{
    collections::HashMap,
    error::Error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};
use zebra_chain::block::{Block, BlockHeaderHash};

#[derive(Default)]
struct ZebraState {
    blocks: HashMap<BlockHeaderHash, Arc<Block>>,
}

impl Service<Request> for ZebraState {
    type Response = Response;
    type Error = Box<dyn Error + Send + Sync + 'static>;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        match req {
            Request::AddBlock { block } => {
                let hash = block.as_ref().into();
                self.blocks.insert(hash, block);

                async { Ok(Response::Added) }.boxed()
            }
            Request::GetBlock { hash } => {
                let result = self
                    .blocks
                    .get(&hash)
                    .cloned()
                    .map(|block| Response::Block { block })
                    .ok_or_else(|| "block could not be found".into());

                async move { result }.boxed()
            }
        }
    }
}

pub fn init() -> impl Service<
    Request,
    Response = Response,
    Error = Box<dyn Error + Send + Sync + 'static>,
    Future = impl Future<Output = Result<Response, Box<dyn Error + Send + Sync + 'static>>>,
> + Send
       + Clone
       + 'static {
    Buffer::new(ZebraState::default(), 1)
}
