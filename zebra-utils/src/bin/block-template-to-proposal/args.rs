//! block-template-to-proposal arguments
//!
//! For usage please refer to the program help: `block-template-to-proposal --help`

use structopt::StructOpt;

use zebra_rpc::client::types::TimeSource;

/// block-template-to-proposal arguments
#[derive(Clone, Debug, Eq, PartialEq, StructOpt)]
pub struct Args {
    /// The source of the time in the block proposal header.
    /// Format: "curtime", "mintime", "maxtime", ["clamped"]u32, "raw"u32
    /// Clamped times are clamped to the template's [`mintime`, `maxtime`].
    /// Raw times are used unmodified: this can produce invalid proposals.
    #[structopt(default_value = "CurTime", short, long)]
    pub time_source: TimeSource,

    /// The JSON block template.
    /// If this argument is not supplied, the template is read from standard input.
    ///
    /// The template and proposal structures are printed to stderr.
    #[structopt(last = true)]
    pub template: Option<String>,
}
