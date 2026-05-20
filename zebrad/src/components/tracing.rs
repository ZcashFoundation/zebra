//! Tracing and logging infrastructure for Zebra.

use std::{
    net::SocketAddr,
    ops::{Deref, DerefMut},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

mod component;
mod endpoint;

#[cfg(feature = "flamegraph")]
mod flame;

#[cfg(feature = "opentelemetry")]
mod otel;

pub use component::Tracing;
pub use endpoint::TracingEndpoint;

#[cfg(feature = "flamegraph")]
pub use flame::{layer, Grapher};

/// Tracing configuration section: outer config after cross-field defaults are applied.
///
/// This is a wrapper type that dereferences to the inner config type.
///
//
// TODO: replace with serde's finalizer attribute when that feature is implemented.
//       we currently use the recommended workaround of a wrapper struct with from/into attributes.
//       https://github.com/serde-rs/serde/issues/642#issuecomment-525432907
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(
    deny_unknown_fields,
    default,
    from = "InnerConfig",
    into = "InnerConfig"
)]
pub struct Config {
    inner: InnerConfig,
}

impl Deref for Config {
    type Target = InnerConfig;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Config {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl From<InnerConfig> for Config {
    fn from(mut inner: InnerConfig) -> Self {
        inner.log_file = runtime_default_log_file(inner.log_file, inner.progress_bar);

        Self { inner }
    }
}

impl From<Config> for InnerConfig {
    fn from(mut config: Config) -> Self {
        config.log_file = disk_default_log_file(config.log_file.clone(), config.progress_bar);

        config.inner
    }
}

/// Tracing configuration section: inner config used to deserialize and apply cross-field defaults.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct InnerConfig {
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
    /// # Security
    ///
    /// If you are running Zebra with elevated permissions ("root"), create the
    /// directory for this file before running Zebra, and make sure the Zebra user
    /// account has exclusive access to that directory, and other users can't modify
    /// its parent directories.
    ///
    /// # Example
    ///
    /// Given `flamegraph = "flamegraph"` we will generate a `flamegraph.svg` and
    /// a `flamegraph.folded` file in the current directory.
    ///
    /// If you provide a path with an extension the extension will be ignored and
    /// replaced with `.folded` and `.svg` for the respective files.
    pub flamegraph: Option<PathBuf>,

    /// Shows progress bars for block syncing, and mempool transactions, and peer networking.
    /// Also sends logs to the default log file path.
    ///
    /// This config field is ignored unless the `progress-bar` feature is enabled.
    pub progress_bar: Option<ProgressConfig>,

    /// If set to a path, write the tracing logs to that path.
    ///
    /// By default, logs are sent to the terminal standard output.
    /// But if the `progress_bar` config is activated, logs are sent to the standard log file path:
    /// - Linux: `$XDG_STATE_HOME/zebrad.log` or `$HOME/.local/state/zebrad.log`
    /// - macOS: `$HOME/Library/Application Support/zebrad.log`
    /// - Windows: `%LOCALAPPDATA%\zebrad.log` or `C:\Users\%USERNAME%\AppData\Local\zebrad.log`
    ///
    /// # Security
    ///
    /// If you are running Zebra with elevated permissions ("root"), create the
    /// directory for this file before running Zebra, and make sure the Zebra user
    /// account has exclusive access to that directory, and other users can't modify
    /// its parent directories.
    pub log_file: Option<PathBuf>,

    /// The use_journald flag sends tracing events to systemd-journald, on Linux
    /// distributions that use systemd.
    ///
    /// Install Zebra using `cargo install --features=journald` to enable this config.
    pub use_journald: bool,

    /// OpenTelemetry OTLP endpoint URL for distributed tracing.
    ///
    /// Install Zebra using `cargo install --features=opentelemetry` to enable this config.
    ///
    /// When `None` (default), OpenTelemetry is completely disabled with zero runtime overhead.
    /// When set, traces are exported via OTLP HTTP protocol.
    ///
    /// Example: `"http://localhost:4318"`
    ///
    /// Can also be set via `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable (lower precedence).
    pub opentelemetry_endpoint: Option<String>,

    /// Service name reported to OpenTelemetry collector.
    ///
    /// Defaults to `"zebra"` if not specified.
    ///
    /// Can also be set via `OTEL_SERVICE_NAME` environment variable.
    pub opentelemetry_service_name: Option<String>,

    /// Trace sampling percentage between 0 and 100.
    ///
    /// Controls what percentage of traces are exported:
    /// - `100` = 100% (all traces, default)
    /// - `10` = 10% (recommended for high-traffic production)
    /// - `0` = 0% (effectively disabled)
    ///
    /// Lower values reduce network/collector overhead for busy nodes.
    ///
    /// Note: This differs from the standard `OTEL_TRACES_SAMPLER_ARG` which uses
    /// a ratio (0.0-1.0). Zebra uses percentage (0-100) for consistency with
    /// other integer-based configuration options.
    pub opentelemetry_sample_percent: Option<u8>,
}

/// The progress bars that Zebra will show while running.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressConfig {
    /// Show a lot of progress bars.
    Detailed,

    /// Show a few important progress bars.
    //
    // TODO: actually hide some progress bars in this mode.
    #[default]
    #[serde(other)]
    Summary,
}

impl Config {
    /// Returns `true` if standard output should use color escapes.
    /// Automatically checks if Zebra is running in a terminal.
    pub fn use_color_stdout(&self) -> bool {
        self.force_use_color || (self.use_color && atty::is(atty::Stream::Stdout))
    }

    /// Returns `true` if standard error should use color escapes.
    /// Automatically checks if Zebra is running in a terminal.
    pub fn use_color_stderr(&self) -> bool {
        self.force_use_color || (self.use_color && atty::is(atty::Stream::Stderr))
    }

    /// Returns `true` if output that could go to standard output or standard error
    /// should use color escapes. Automatically checks if Zebra is running in a terminal.
    pub fn use_color_stdout_and_stderr(&self) -> bool {
        self.force_use_color
            || (self.use_color && atty::is(atty::Stream::Stdout) && atty::is(atty::Stream::Stderr))
    }
}

impl Default for InnerConfig {
    fn default() -> Self {
        // TODO: enable progress bars by default once they have been tested
        let progress_bar = None;

        Self {
            use_color: true,
            force_use_color: false,
            filter: None,
            buffer_limit: 128_000,
            endpoint_addr: None,
            flamegraph: None,
            progress_bar,
            log_file: runtime_default_log_file(None, progress_bar),
            use_journald: false,
            opentelemetry_endpoint: None,
            opentelemetry_service_name: None,
            opentelemetry_sample_percent: None,
        }
    }
}

/// Returns the runtime default log file path based on the `log_file` and `progress_bar` configs.
fn runtime_default_log_file(
    log_file: Option<PathBuf>,
    progress_bar: Option<ProgressConfig>,
) -> Option<PathBuf> {
    if let Some(log_file) = log_file {
        return Some(log_file);
    }

    // If the progress bar is active, we want to use a log file regardless of the config.
    // (Logging to a terminal erases parts of the progress bars, making both unreadable.)
    if progress_bar.is_some() {
        return default_log_file();
    }

    None
}

/// Returns the configured log file path using the runtime `log_file` and `progress_bar` config.
///
/// This is the inverse of [`runtime_default_log_file()`].
fn disk_default_log_file(
    log_file: Option<PathBuf>,
    progress_bar: Option<ProgressConfig>,
) -> Option<PathBuf> {
    // If the progress bar is active, and we've likely substituted the default log file path,
    // don't write that substitute to the config on disk.
    if progress_bar.is_some() && log_file == default_log_file() {
        return None;
    }

    log_file
}

/// Returns the default log file path.
fn default_log_file() -> Option<PathBuf> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|dir| dir.join("zebrad.log"))
}
