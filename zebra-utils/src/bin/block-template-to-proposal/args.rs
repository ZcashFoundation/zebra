//! block-template-to-proposal arguments
//!
//! For usage please refer to the program help: `block-template-to-proposal --help`

use clap::Parser;

use zebra_chain::parameters::Network;
use zebra_rpc::client::BlockTemplateTimeSource;

/// block-template-to-proposal arguments
#[derive(Clone, Debug, Eq, PartialEq, Parser)]
#[command(version)]
pub struct Args {
    /// The network to use for the block proposal.
    #[arg(default_value = "Mainnet", short, long)]
    pub net: Network,

    /// The source of the time in the block proposal header.
    /// Format: "curtime", "mintime", "maxtime", ["clamped"]u32, "raw"u32
    /// Clamped times are clamped to the template's [`mintime`, `maxtime`].
    /// Raw times are used unmodified: this can produce invalid proposals.
    #[arg(default_value = "CurTime", short, long)]
    pub time_source: BlockTemplateTimeSource,

    /// The JSON block template.
    /// If this argument is not supplied, the template is read from standard input.
    ///
    /// The template and proposal structures are printed to stderr.
    #[arg(last = true)]
    pub template: Option<String>,
}
