#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![allow(clippy::try_err)]
use std::sync::Arc;
use zebra_chain::block::{Block, BlockHeaderHash};

pub mod in_memory;

#[derive(Debug, PartialEq)]
pub enum Request {
    // TODO(jlusby): deprecate in the future based on our validation story
    AddBlock { block: Arc<Block> },
    GetBlock { hash: BlockHeaderHash },
    GetTip,
}

#[derive(Debug, PartialEq)]
pub enum Response {
    Added { hash: BlockHeaderHash },
    Block { block: Arc<Block> },
    Tip { hash: BlockHeaderHash },
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::Report;
    use eyre::{bail, ensure, eyre};
    use tower::Service;
    use zebra_chain::serialization::ZcashDeserialize;

    fn install_tracing() {
        use tracing_error::ErrorLayer;
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::{fmt, EnvFilter};

        let fmt_layer = fmt::layer().with_target(false);
        let filter_layer = EnvFilter::try_from_default_env()
            .or_else(|_| EnvFilter::try_new("info"))
            .unwrap();

        tracing_subscriber::registry()
            .with(filter_layer)
            .with(fmt_layer)
            .with(ErrorLayer::default())
            .init();
    }

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
            response == Response::Added { hash },
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

    #[tokio::test]
    #[spandoc::spandoc]
    async fn get_tip() -> Result<(), Report> {
        install_tracing();

        let block0: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?.into();
        let block1: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_1_BYTES[..])?.into();

        let block0_hash: BlockHeaderHash = block0.as_ref().into();
        let block1_hash: BlockHeaderHash = block1.as_ref().into();
        let expected_hash: BlockHeaderHash = block1_hash;

        let mut service = in_memory::init();

        /// insert the higher block first
        let response = service
            .call(Request::AddBlock { block: block1 })
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            response == Response::Added { hash: block1_hash },
            "unexpected response kind: {:?}",
            response
        );

        /// genesis block second
        let response = service
            .call(Request::AddBlock {
                block: block0.clone(),
            })
            .await
            .map_err(|e| eyre!(e))?;

        ensure!(
            response == Response::Added { hash: block0_hash },
            "unexpected response kind: {:?}",
            response
        );

        let block_response = service.call(Request::GetTip).await.map_err(|e| eyre!(e))?;

        /// assert that the higher block is returned as the tip even tho it was least recently inserted
        match block_response {
            Response::Tip { hash } => assert_eq!(expected_hash, hash),
            _ => bail!("unexpected response kind: {:?}", block_response),
        }

        Ok(())
    }
}
