//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use zebra_consensus::Config as ConsensusSection;
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
    /// Consensus configuration
    pub consensus: ConsensusSection,

    /// Metrics configuration
    pub metrics: MetricsSection,

    /// Networking configuration
    pub network: NetworkSection,

    /// State configuration
    pub state: StateSection,

    /// Tracing configuration
    pub tracing: TracingSection,

    /// Sync configuration
    pub sync: SyncSection,
}

/// Tracing configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct TracingSection {
    /// Whether to use colored terminal output, if available.
    ///
    /// Colored terminal output is automatically disabled if an output stream
    /// is connected to a file. (Or another non-terminal device.)
    ///
    /// Defaults to `true`, which automatically enables colored output to
    /// terminals.
    pub use_color: bool,

    /// The filter used for tracing events.
    ///
    /// The filter is used to create a `tracing-subscriber`
    /// [`EnvFilter`](https://docs.rs/tracing-subscriber/0.2.10/tracing_subscriber/filter/struct.EnvFilter.html#directives),
    /// and more details on the syntax can be found there or in the examples
    /// below.
    ///
    /// If no filter is specified (`None`), the filter is set to `info` if the
    /// `-v` flag is given and `warn` if it is not given.
    ///
    /// # Examples
    ///
    /// `warn,zebrad=info,zebra_network=debug` sets a global `warn` level, an
    /// `info` level for the `zebrad` crate, and a `debug` level for the
    /// `zebra_network` crate.
    ///
    /// ```ascii,no_run
    /// [block_verify{height=Some\(block::Height\(.*000\)\)}]=trace
    /// ```
    /// sets `trace` level for all events occurring in the context of a
    /// `block_verify` span whose `height` field ends in `000`, i.e., traces the
    /// verification of every 1000th block.
    pub filter: Option<String>,

    /// The address used for an ad-hoc RPC endpoint allowing dynamic control of the tracing filter.
    ///
    /// If this is set to None, the endpoint is disabled.
    pub endpoint_addr: Option<SocketAddr>,

    /// Controls whether to write a flamegraph of tracing spans.
    ///
    /// If this is set to None, flamegraphs are disabled. Otherwise, it specifies
    /// an output file path, as described below.
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
    /// Given `flamegraph = "flamegraph"` we will generate a `flamegraph.svg` and
    /// a `flamegraph.folded` file in the current directory.
    ///
    /// If you provide a path with an extension the extension will be ignored and
    /// replaced with `.folded` and `.svg` for the respective files.
    pub flamegraph: Option<PathBuf>,
}

impl Default for TracingSection {
    fn default() -> Self {
        Self {
            use_color: true,
            filter: None,
            endpoint_addr: None,
            flamegraph: None,
        }
    }
}

/// Metrics configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsSection {
    /// The address used for the Prometheus metrics endpoint.
    ///
    /// The endpoint is disabled if this is set to `None`.
    pub endpoint_addr: Option<SocketAddr>,
}

impl Default for MetricsSection {
    fn default() -> Self {
        Self {
            endpoint_addr: None,
        }
    }
}

/// Sync configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct SyncSection {
    /// The maximum number of concurrent block requests during sync.
    ///
    /// This is set to a low value by default, to avoid task and
    /// network contention. Increasing this value may improve
    /// performance on machines with many cores and a fast network
    /// connection.
    pub max_concurrent_block_requests: usize,

    /// Controls how far ahead of the chain tip the syncer tries to
    /// download before waiting for queued verifications to complete.
    ///
    /// Increasing this limit increases the buffer size, so it reduces
    /// the impact of an individual block request failing.  The block
    /// size limit is 2MB, so in theory, this could represent multiple
    /// gigabytes of data, if we downloaded arbitrary blocks. However,
    /// because we randomly load balance outbound requests, and separate
    /// block download from obtaining block hashes, an adversary would
    /// have to control a significant fraction of our peers to lead us
    /// astray.
    ///
    /// This value is clamped to an implementation-defined lower bound.
    pub lookahead_limit: usize,
}

impl Default for SyncSection {
    fn default() -> Self {
        Self {
            max_concurrent_block_requests: 50,
            lookahead_limit: 2_000,
        }
    }
}
