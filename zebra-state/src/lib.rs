#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
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

#[derive(Debug)]
pub enum Request {
    // TODO(jlusby): deprecate in the future based on our validation story
    AddBlock { block: Arc<Block> },
    GetBlock { hash: BlockHeaderHash },
}

#[derive(Debug)]
pub enum Response {
    Added,
    Block { block: Arc<Block> },
}

pub mod in_memory {
    use super::*;
    use std::error::Error;

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
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::Report;
    use eyre::{bail, ensure, eyre};
    use zebra_chain::serialization::ZcashDeserialize;

    #[tokio::test]
    async fn round_trip() -> Result<(), Report> {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])?.into();
        let hash = block.as_ref().into();

        let mut service = in_memory::init();

        let response = service
            .call(Request::AddBlock {
                block: block.clone(),
            })
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            matches!(response, Response::Added),
            "unexpected response kind: {:?}",
            response
        );

        let block_response = service
            .call(Request::GetBlock { hash })
            .await
            .map_err(|e| eyre!(e))?;

        match block_response {
            Response::Block {
                block: returned_block,
            } => assert_eq!(block, returned_block),
            _ => bail!("unexpected response kind: {:?}", block_response),
        }

        Ok(())
    }
}
