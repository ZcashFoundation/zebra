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

pub mod in_memory;
pub mod on_disk;

/// Configuration for networking code.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The root directory for the state storage
    pub path: Option<PathBuf>,
}

impl Config {
    /// Generate the appropriate `sled::Config` based on the provided
    /// `zebra_state::Config`.
    ///
    /// # Details
    ///
    /// This function should panic if the user of `zebra-state` doesn't configure
    /// a directory to store the state.
    ///
    /// Currently this is only partially implemented by leveraging a `SpanTrace`
    /// to print a TODO to the user which isn't as nice as what we'd eventually
    /// want but it definitely gets the point across.
    ///
    /// # Current Format
    ///
    /// <pre><font color="#CC0000">The application panicked (crashed).</font>
    /// Message:  <font color="#06989A">called `Option::unwrap()` on a `None` value</font>
    /// Location: <font color="#75507B">zebra-state/src/lib.rs</font>:<font color="#75507B">40</font>
    ///
    /// Backtrace omitted.
    ///
    /// Run with RUST_BACKTRACE=1 environment variable to display it.
    /// Run with RUST_BACKTRACE=full to include source snippets.
    ///   ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ SPANTRACE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    ///
    ///    0: <font color="#F15D22">zebra_state::sled_config::comment</font> with <font color="#34E2E2">text=TODO(jlusby): Replace this unwrap with a nice user-facing</font>
    /// <font color="#34E2E2">   explaining error that the cache dir must be specified</font>
    ///       at <font color="#75507B">zebra-state/src/lib.rs</font>:<font color="#75507B">36</font></pre>
    #[spandoc::spandoc]
    pub(crate) fn sled_config(&self) -> sled::Config {
        /**
         * SPANDOC: TODO(jlusby): Replace this unwrap with a nice user-facing
         * explaining error that the cache dir must be specified
         */
        let path = self.path.as_ref().unwrap().join("state");

        sled::Config::default().path(path)
    }
}

impl Default for Config {
    fn default() -> Self {
        let path = std::env::var("ZEBRAD_CACHE_DIR")
            .map(PathBuf::from)
            .ok()
            .or_else(dirs::cache_dir)
            .map(|dir| dir.join("zebra"));

        Self { path }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic]
    fn test_no_path() {
        zebra_test::init();

        let bad_config = Config { path: None };
        let _unreachable = bad_config.sled_config();
    }
}
