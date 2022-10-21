//! Tracing and logging infrastructure for Zebra.

use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

mod component;
mod endpoint;

#[cfg(feature = "flamegraph")]
mod flame;

pub use component::Tracing;
pub use endpoint::TracingEndpoint;

#[cfg(feature = "flamegraph")]
pub use flame::{layer, Grapher};

/// Tracing configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Whether to use colored terminal output, if available.
    ///
    /// Colored terminal output is automatically disabled if an output stream
    /// is connected to a file. (Or another non-terminal device.)
    ///
    /// Defaults to `true`, which automatically enables colored output to
    /// terminals.
    pub use_color: bool,

    /// Whether to force the use of colored terminal output, even if it's not available.
    ///
    /// Will force Zebra to use colored terminal output even if it does not detect that the output
    /// is a terminal that supports colors.
    ///
    /// Defaults to `false`, which keeps the behavior of `use_color`.
    pub force_use_color: bool,

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

    /// The buffer_limit size sets the number of log lines that can be queued by the tracing subscriber
    /// to be written to stdout before logs are dropped.
    ///
    /// Defaults to 128,000 with a minimum of 100.
    pub buffer_limit: usize,

    /// The address used for an ad-hoc RPC endpoint allowing dynamic control of the tracing filter.
    ///
    /// Install Zebra using `cargo install --features=filter-reload` to enable this config.
    ///
    /// If this is set to None, the endpoint is disabled.
    pub endpoint_addr: Option<SocketAddr>,

    /// Controls whether to write a flamegraph of tracing spans.
    ///
    /// Install Zebra using `cargo install --features=flamegraph` to enable this config.
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

    /// The use_journald flag sends tracing events to systemd-journald, on Linux
    /// distributions that use systemd.
    ///
    /// Install Zebra using `cargo install --features=journald` to enable this config.
    pub use_journald: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            use_color: true,
            force_use_color: false,
            filter: None,
            buffer_limit: 128_000,
            endpoint_addr: None,
            flamegraph: None,
            use_journald: false,
        }
    }
}
