//! State storage code for Zebra. 🦓
//!
//! ## Organizational Structure
//!
//! zebra-state tracks `Blocks` using two key-value trees
//!
//! * BlockHeaderHash -> Block
//! * BlockHeight -> Block
//!
//! Inserting a block into the service will create a mapping in each tree for that block.
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use zebra_chain::block::{Block, BlockHeaderHash};

pub mod in_memory;
pub mod on_disk;

/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The root directory for the state storage
    pub path: PathBuf,
}

impl Config {
    pub(crate) fn sled_config(&self) -> sled::Config {
        sled::Config::default().path(&self.path)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./.zebra-state"),
        }
    }
}

#[derive(Debug, PartialEq)]
/// A state request, used to manipulate the zebra-state on disk or in memory
pub enum Request {
    // TODO(jlusby): deprecate in the future based on our validation story
    /// Add a block to the zebra-state
    AddBlock {
        /// The block to be added to the state
        block: Arc<Block>,
    },
    /// Get a block from the zebra-state
    GetBlock {
        /// The hash used to identify the block
        hash: BlockHeaderHash,
    },
    /// Get the block that is the tip of the current chain
    GetTip,
    /// Ask the state if the given hash is part of the current best chain
    GetDepth {
        /// The hash to check against the current chain
        hash: BlockHeaderHash,
    },
}

#[derive(Debug, PartialEq)]
/// A state response
pub enum Response {
    /// The response to a `AddBlock` request indicating a block was successfully
    /// added to the state
    Added {
        /// The hash of the block that was added
        hash: BlockHeaderHash,
    },
    /// The response to a `GetBlock` request by hash
    Block {
        /// The block that was requested
        block: Arc<Block>,
    },
    /// The response to a `GetTip` request
    Tip {
        /// The hash of the block at the tip of the current chain
        hash: BlockHeaderHash,
    },
    /// The response to a `Contains` request indicating that the given has is in
    /// the current best chain
    Depth(
        /// The number of blocks above the given block in the current best chain
        Option<u32>,
    ),
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::Report;
    use color_eyre::eyre::{bail, ensure, eyre};
    use std::sync::Once;
    use tower::Service;
    use tracing_error::ErrorLayer;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{fmt, EnvFilter};
    use zebra_chain::serialization::ZcashDeserialize;

    static LOGGER_INIT: Once = Once::new();

    fn install_tracing() {
        LOGGER_INIT.call_once(|| {
            let fmt_layer = fmt::layer().with_target(false);
            let filter_layer = EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new("info"))
                .unwrap();

            tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt_layer)
                .with(ErrorLayer::default())
                .init();
        })
    }

    #[tokio::test]
    async fn test_round_trip() -> Result<(), Report> {
        install_tracing();

        let service = in_memory::init();
        round_trip(service).await?;

        let mut config = crate::Config::default();
        let tmp_dir = tempdir::TempDir::new("round_trip")?;
        config.path = tmp_dir.path().to_owned();
        let service = on_disk::init(config);
        get_tip(service).await?;

        Ok(())
    }

    async fn round_trip<S>(mut service: S) -> Result<(), Report>
    where
        S: Service<
            Request,
            Error = Box<dyn std::error::Error + Send + Sync + 'static>,
            Response = Response,
        >,
    {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_415000_BYTES[..])?.into();
        let hash = block.as_ref().into();

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
    async fn test_get_tip() -> Result<(), Report> {
        install_tracing();

        let service = in_memory::init();
        get_tip(service).await?;

        let mut config = crate::Config::default();
        let tmp_dir = tempdir::TempDir::new("get_tip")?;
        config.path = tmp_dir.path().to_owned();
        let service = on_disk::init(config);
        get_tip(service).await?;

        Ok(())
    }

    #[spandoc::spandoc]
    async fn get_tip<S>(mut service: S) -> Result<(), Report>
    where
        S: Service<
            Request,
            Error = Box<dyn std::error::Error + Send + Sync + 'static>,
            Response = Response,
        >,
    {
        install_tracing();

        let block0: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?.into();
        let block1: Arc<_> =
            Block::zcash_deserialize(&zebra_test_vectors::BLOCK_MAINNET_1_BYTES[..])?.into();

        let block0_hash: BlockHeaderHash = block0.as_ref().into();
        let block1_hash: BlockHeaderHash = block1.as_ref().into();
        let expected_hash: BlockHeaderHash = block1_hash;

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
