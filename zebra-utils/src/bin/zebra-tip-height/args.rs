//! zebra-tip-height arguments
//!
//! For usage please refer to the program help: `zebra-tip-height --help`

#![deny(missing_docs)]
#![allow(clippy::try_err)]

use std::path::PathBuf;

use structopt::StructOpt;

use zebra_chain::parameters::Network;

/// zebra-tip-height arguments
#[derive(Debug, StructOpt)]
pub struct Args {
    /// Path to Zebra's cached state.
    #[structopt(short, long, parse(from_os_str))]
    pub cache_dir: PathBuf,

    /// The network to obtain the chain tip.
    #[structopt(default_value = "mainnet", short, long)]
    pub network: Network,
}
