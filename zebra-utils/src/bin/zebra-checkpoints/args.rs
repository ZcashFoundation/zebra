//! zebra-checkpoints arguments
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`

#![deny(missing_docs)]
#![allow(clippy::try_err)]

use structopt::StructOpt;

/// zebra-checkpoints arguments
#[derive(Debug, StructOpt)]
pub struct Args {
    /// Path to zcash-cli command
    #[structopt(default_value = "zcash-cli", short, long)]
    pub cli: String,

    /// Start looking for checkpoints after this height.
    /// If there is no last checkpoint, we start looking at the Genesis block (height 0).
    #[structopt(short, long)]
    pub last_checkpoint: Option<u32>,

    /// Passthrough args for `zcash-cli`
    #[structopt(last = true)]
    pub zcli_args: Vec<String>,
}
