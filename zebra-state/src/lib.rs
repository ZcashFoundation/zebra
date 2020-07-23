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

#![doc(html_favicon_url = "https://www.zfnd.org/images/zebra-favicon-128.png")]
#![doc(html_logo_url = "https://www.zfnd.org/images/zebra-icon.png")]
#![doc(html_root_url = "https://doc.zebra.zfnd.org/zebra_state")]
#![warn(missing_docs)]
#![allow(clippy::try_err)]
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{iter, sync::Arc};
use zebra_chain::{
    block::{Block, BlockHeaderHash},
    types::BlockHeight,
};

pub mod in_memory;
pub mod on_disk;

/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The root directory for the state storage
    pub cache_dir: Option<PathBuf>,
}

impl Config {
    /// Generate the appropriate `sled::Config` based on the provided
    /// `zebra_state::Config`.
    ///
    /// # Details
    ///
    /// This function should panic if the user of `zebra-state` doesn't configure
    /// a directory to store the state.
    pub(crate) fn sled_config(&self) -> sled::Config {
        let path = self
            .cache_dir
            .as_ref()
            .unwrap_or_else(|| {
                todo!("create a nice user facing error explaining how to set the cache directory")
            })
            .join("state");

        sled::Config::default().path(path)
    }
}

impl Default for Config {
    fn default() -> Self {
        let cache_dir = std::env::var("ZEBRAD_CACHE_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(|| dirs::cache_dir().map(|dir| dir.join("zebra")));

        Self { cache_dir }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Get a block locator list for the current best chain
    GetBlockLocator {
        /// The genesis block of the current best chain
        genesis: BlockHeaderHash,
    },
    /// Get the block that is the tip of the current chain
    GetTip,
    /// Ask the state if the given hash is part of the current best chain
    GetDepth {
        /// The hash to check against the current chain
        hash: BlockHeaderHash,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// The response to a `GetBlockLocator` request
    BlockLocator {
        /// The set of blocks that make up the block locator
        block_locator: Vec<BlockHeaderHash>,
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

/// Get the heights of the blocks for constructing a block_locator list
fn block_locator_heights(tip_height: BlockHeight) -> impl Iterator<Item = BlockHeight> {
    iter::successors(Some(1u32), |h| h.checked_mul(2))
        .flat_map(move |step| tip_height.0.checked_sub(step))
        .map(BlockHeight)
        .chain(iter::once(BlockHeight(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn test_no_path() {
        zebra_test::init();

        let bad_config = Config { cache_dir: None };
        let _unreachable = bad_config.sled_config();
    }
}
