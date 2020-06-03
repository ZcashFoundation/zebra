#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
use std::sync::Arc;
use zebra_chain::block::{Block, BlockHeaderHash};

pub mod in_memory;

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

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::Report;
    use eyre::{bail, ensure, eyre};
    use tower::Service;
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
