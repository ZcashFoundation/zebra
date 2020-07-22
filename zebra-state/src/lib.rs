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
use std::sync::Arc;

use zebra_chain::block::{Block, BlockHeaderHash};
use zebra_chain::Network::{self, *};

pub mod in_memory;
pub mod on_disk;

/// Configuration for the state service.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The directory used to store cached state.
    ///
    /// Each network has a separate state, which is stored in "mainnet" and
    /// "testnet" sub-directories.
    pub path: PathBuf,
}

impl Config {
    pub(crate) fn sled_config(&self, network: Network) -> sled::Config {
        let path_suffix = match network {
            Mainnet => "mainnet",
            Testnet => "testnet",
        };
        let mut network_path = self.path.clone();
        network_path.push(path_suffix);
        sled::Config::default().path(&network_path)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            path: PathBuf::from("./.zebra-state"),
        }
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
