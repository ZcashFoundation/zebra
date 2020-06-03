use super::{Request, Response};
use futures::prelude::*;
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tower::{buffer::Buffer, Service};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

#[derive(Default)]
struct ZebraState {
    by_hash: HashMap<BlockHeaderHash, Arc<Block>>,
    by_height: BTreeMap<BlockHeight, Arc<Block>>,
}

impl ZebraState {
    fn insert(&mut self, block: impl Into<Arc<Block>>) {
        let block = block.into();
        let hash = block.as_ref().into();
        let height = block.coinbase_height().unwrap();

        assert!(
            self.by_hash.insert(hash, block.clone()).is_none(),
            "blocks shouldn't have the same hash"
        );
        assert!(
            self.by_height.insert(height, block).is_none(),
            "blocks with the same height are currently unsupported"
        );
    }

    fn get(&mut self, query: impl Into<BlockQuery>) -> Option<Arc<Block>> {
        match query.into() {
            BlockQuery::ByHash(hash) => self.by_hash.get(&hash),
            BlockQuery::ByHeight(height) => self.by_height.get(&height),
        }
        .cloned()
    }

    fn tip(&self) -> Option<Arc<Block>> {
        self.by_height
            .iter()
            .next_back()
            .map(|(_key, value)| value)
            .cloned()
    }
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
                self.insert(block);
                async { Ok(Response::Added) }.boxed()
            }
            Request::GetBlock { hash } => {
                let result = self
                    .get(hash)
                    .map(|block| Response::Block { block })
                    .ok_or_else(|| "block could not be found".into());

                async move { result }.boxed()
            }
            Request::GetTip => {
                let result = self
                    .tip()
                    .map(|block| block.as_ref().into())
                    .map(|hash| Response::Tip { hash })
                    .ok_or_else(|| "zebra-state contains no blocks".into());

                async move { result }.boxed()
            }
        }
    }
}

enum BlockQuery {
    ByHash(BlockHeaderHash),
    ByHeight(BlockHeight),
}

impl From<BlockHeaderHash> for BlockQuery {
    fn from(hash: BlockHeaderHash) -> Self {
        Self::ByHash(hash)
    }
}

impl From<BlockHeight> for BlockQuery {
    fn from(height: BlockHeight) -> Self {
        Self::ByHeight(height)
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
