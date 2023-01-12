//! block-template-to-proposal arguments
//!
//! For usage please refer to the program help: `block-template-to-proposal --help`

use std::str::FromStr;

use structopt::StructOpt;

use zebra_chain::{serialization::DateTime32, BoxError};

/// The source of the time in the block proposal header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimeSource {
    /// The `curtime` field in the template.
    CurTime,

    /// The `mintime` field in the template.
    MinTime,

    /// The `maxtime` field in the template.
    MaxTime,

    /// The supplied time, clamped within the template's `[mintime, maxtime]`.
    Clamped(DateTime32),

    /// The raw supplied time, ignoring the `mintime` and `maxtime` in the template.
    ///
    /// Warning: this can create an invalid block proposal.
    Raw(DateTime32),
}

impl FromStr for TimeSource {
    type Err = BoxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use TimeSource::*;

        match s.to_lowercase().as_str() {
            "curtime" => Ok(CurTime),
            "mintime" => Ok(MinTime),
            "maxtime" => Ok(MaxTime),
            // "raw"u32
            s if s.strip_prefix("raw").is_some() => {
                Ok(Raw(s.strip_prefix("raw").unwrap().parse()?))
            }
            // "clamped"u32 or just u32
            _ => Ok(Clamped(s.strip_prefix("clamped").unwrap_or(s).parse()?)),
        }
    }
}

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
