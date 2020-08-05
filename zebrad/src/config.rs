//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use tracing_subscriber::EnvFilter;
use zebra_network::Config as NetworkSection;
use zebra_state::Config as StateSection;

/// Configuration for `zebrad`.
///
/// The `zebrad` config is a TOML-encoded version of this structure. The meaning
/// of each field is described in the documentation, although it may be necessary
/// to click through to the sub-structures for each section.
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZebradConfig {
    /// Metrics configuration
    pub metrics: MetricsSection,

    /// Networking configuration
    pub network: NetworkSection,

    /// State configuration
    pub state: StateSection,

    /// Tracing configuration
    pub tracing: TracingSection,
}

/// Tracing configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct TracingSection {
    /// The filter used for tracing events.
    ///
    /// The filter is used to create a `tracing-subscriber`
    /// [`EnvFilter`](https://docs.rs/tracing-subscriber/0.2.10/tracing_subscriber/filter/struct.EnvFilter.html#directives),
    /// and more details on the syntax can be found there or in the examples
    /// below.
    ///
    /// # Examples
    ///
    /// `warn,zebrad=info,zebra_network=debug` sets a global `warn` level, an
    /// `info` level for the `zebrad` crate, and a `debug` level for the
    /// `zebra_network` crate.
    ///
    /// ```ascii,no_run
    /// [block_verify{height=Some\(BlockHeight\(.*000\)\)}]=trace
    /// ```
    /// sets `trace` level for all events occurring in the context of a
    /// `block_verify` span whose `height` field ends in `000`, i.e., traces the
    /// verification of every 1000th block.
    pub filter: Option<String>,

    /// The endpoint address used for tracing.
    pub endpoint_addr: SocketAddr,

    /// The path to write a flamegraph of tracing spans too.
    ///
    /// This path is not used verbatim when writing out the flamegraph. This is
    /// because the flamegraph is written out as two parts. First the flamegraph
    /// is constantly persisted to the disk in a "folded" representation that
    /// records collapsed stack traces of the tracing spans that are active.
    /// Then, when the application is finished running the destructor will flush
    /// the flamegraph output to the folded file and then read that file and
    /// generate the final flamegraph from it as an SVG.
    ///
    /// The need to create two files means that we will slightly manipulate the
    /// path given to us to create the two representations.
    ///
    /// # Example
    ///
    /// Given `flamegraph = "flamegraph"` we will generate a `flamegraph.svg`
    /// and a `flamegraph.folded` file in the current directory.
    ///
    /// If you provide a path with an extension the extension will be ignored and
    /// replaced with `.folded` and `.svg` for the respective files.
    pub flamegraph: Option<PathBuf>,
}

impl TracingSection {
    pub fn populated() -> Self {
        Self {
            filter: Some("info".to_owned()),
            endpoint_addr: "0.0.0.0:3000".parse().unwrap(),
            flamegraph: None,
        }
    }

    /// Constructs an EnvFilter for use in our tracing subscriber.
    ///
    /// The env filter controls filtering of spans and events, but not how
    /// they're emitted. Creating an env filter alone doesn't enable logging, it
    /// needs to be used in conjunction with other layers like a fmt subscriber,
    /// for logs, or an error layer, for SpanTraces.
    pub fn env_filter(&self) -> EnvFilter {
        self.filter.as_deref().unwrap_or("info").into()
    }
}

impl Default for TracingSection {
    fn default() -> Self {
        Self::populated()
    }
}

/// Metrics configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsSection {
    /// The endpoint address used for metrics.
    pub endpoint_addr: SocketAddr,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            endpoint_addr: "0.0.0.0:9999".parse().unwrap(),
        }
    }
}

#[cfg(test)]
mod test {
    use color_eyre::eyre::Result;

    #[test]
    fn test_toml_ser() -> Result<()> {
        let default_config = super::ZebradConfig::default();
        println!("Default config: {:?}", default_config);

        println!("Toml:\n{}", toml::Value::try_from(&default_config)?);

        Ok(())
    }
}
