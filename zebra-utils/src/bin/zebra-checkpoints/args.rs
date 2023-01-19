//! zebra-checkpoints arguments
//!
//! For usage please refer to the program help: `zebra-checkpoints --help`

use structopt::StructOpt;
use thiserror::Error;

use std::str::FromStr;

/// Backend
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Zebra,
    Zcash,
}

impl FromStr for Mode {
    type Err = InvalidModeError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "zebra" => Ok(Mode::Zebra),
            "zcash" => Ok(Mode::Zcash),
            _ => Err(InvalidModeError(string.to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("Invalid mode: {0}")]
pub struct InvalidModeError(String);

/// zebra-checkpoints arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// Mode
    #[structopt(default_value = "zebra", short, long)]
    pub mode: Mode,

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
