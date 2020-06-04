use super::{Request, Response};
use futures::prelude::*;
use std::{
    error::Error,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};

mod block_index;

#[derive(Default)]
struct ZebraState {
    index: block_index::BlockIndex,
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
                let result = self.index.insert(block).map(|_| Response::Added);

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
                    .map(|block| block.as_ref().into())
                    .map(|hash| Response::Tip { hash })
                    .ok_or_else(|| "zebra-state contains no blocks".into());

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
