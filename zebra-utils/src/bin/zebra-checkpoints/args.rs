//! zebra-checkpoints arguments
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`

use std::str::FromStr;

use structopt::StructOpt;
use thiserror::Error;

use zebra_chain::block::Height;

/// The backend type the zebra-checkpoints utility will use to get data from.
///
/// This changes which RPCs the tool calls, and which fields it expects them to have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Expect a Zebra-style backend with limited RPCs and fields.
    ///
    /// Calls these specific RPCs:
    /// - `getblock` with `verbose=0`, manually calculating `hash`, `height`, and `size`
    /// - `getblockchaininfo`, expecting a `blocks` field
    ///
    /// Supports both `zebrad` and `zcashd` nodes.
    Zebrad,

    /// Expect a `zcashd`-style backend with all available RPCs and fields.
    ///
    /// Calls these specific RPCs:
    /// - `getblock` with `verbose=1`, expecting `hash`, `height`, and `size` fields
    /// - `getblockchaininfo`, expecting a `blocks` field
    ///
    /// Currently only supported with `zcashd`.
    Zcashd,
}

impl FromStr for Backend {
    type Err = InvalidBackendError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "zebrad" => Ok(Backend::Zebrad),
            "zcashd" => Ok(Backend::Zcashd),
            _ => Err(InvalidBackendError(string.to_owned())),
        }
    }
}

/// An error indicating that the supplied string is not a valid [`Backend`] name.
#[derive(Debug, Error)]
#[error("Invalid mode: {0}")]
pub struct InvalidBackendError(String);

/// zebra-checkpoints arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// Backend type
    #[structopt(default_value = "zebrad", short, long)]
    pub backend: Backend,

    /// Path to zcash-cli command
    #[structopt(default_value = "zcash-cli", short, long)]
    pub cli: String,

    /// Start looking for checkpoints after this height.
    /// If there is no last checkpoint, we start looking at the Genesis block (height 0).
    #[structopt(short, long)]
    pub last_checkpoint: Option<Height>,

    /// Passthrough args for `zcash-cli`
    #[structopt(last = true)]
    pub zcli_args: Vec<String>,
}
