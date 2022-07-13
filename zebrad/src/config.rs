//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use std::{net::SocketAddr, path::PathBuf};

use serde::{Deserialize, Serialize};

use zebra_consensus::Config as ConsensusSection;
use zebra_network::Config as NetworkSection;
use zebra_rpc::config::Config as RpcSection;
use zebra_state::Config as StateSection;

use crate::components::{mempool::Config as MempoolSection, sync};

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

    /// Mempool configuration
    pub mempool: MempoolSection,

    /// RPC configuration
    pub rpc: RpcSection,
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

impl Default for TracingSection {
    fn default() -> Self {
        Self {
            use_color: true,
            force_use_color: false,
            filter: None,
            endpoint_addr: None,
            flamegraph: None,
            use_journald: false,
        }
    }
}

/// Metrics configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsSection {
    /// The address used for the Prometheus metrics endpoint.
    ///
    /// Install Zebra using `cargo install --features=prometheus` to enable this config.
    ///
    /// The endpoint is disabled if this is set to `None`.
    pub endpoint_addr: Option<SocketAddr>,
}

// we like our default configs to be explicit
#[allow(unknown_lints)]
#[allow(clippy::derivable_impls)]
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
    /// The number of parallel block download requests.
    ///
    /// This is set to a low value by default, to avoid task and
    /// network contention. Increasing this value may improve
    /// performance on machines with a fast network connection.
    #[serde(alias = "max_concurrent_block_requests")]
    pub download_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the checkpoint verifier.
    ///
    /// Increasing this limit increases the buffer size, so it reduces
    /// the impact of an individual block request failing. However, it
    /// also increases memory and CPU usage if block validation stalls,
    /// or there are some large blocks in the pipeline.
    ///
    /// The block size limit is 2MB, so in theory, this could represent multiple
    /// gigabytes of data, if we downloaded arbitrary blocks. However,
    /// because we randomly load balance outbound requests, and separate
    /// block download from obtaining block hashes, an adversary would
    /// have to control a significant fraction of our peers to lead us
    /// astray.
    ///
    /// For reliable checkpoint syncing, Zebra enforces a
    /// [`MIN_CHECKPOINT_CONCURRENCY_LIMIT`](sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT).
    ///
    /// This is set to a high value by default, to avoid verification pipeline stalls.
    /// Decreasing this value reduces RAM usage.
    #[serde(alias = "lookahead_limit")]
    pub checkpoint_verify_concurrency_limit: usize,

    /// The number of blocks submitted in parallel to the full verifier.
    ///
    /// This is set to a low value by default, to avoid verification timeouts on large blocks.
    /// Increasing this value may improve performance on machines with many cores.
    pub full_verify_concurrency_limit: usize,

    /// The number of threads used to verify signatures, proofs, and other CPU-intensive code.
    ///
    /// Set to `0` by default, which uses one thread per available CPU core.
    /// For details, see [the rayon documentation](https://docs.rs/rayon/latest/rayon/struct.ThreadPoolBuilder.html#method.num_threads).
    pub parallel_cpu_threads: usize,
}

impl Default for SyncSection {
    fn default() -> Self {
        Self {
            // 2/3 of the default outbound peer limit.
            download_concurrency_limit: 50,

            // A few max-length checkpoints.
            checkpoint_verify_concurrency_limit: sync::DEFAULT_CHECKPOINT_CONCURRENCY_LIMIT,

            // This default is deliberately very low, so Zebra can verify a few large blocks in under 60 seconds,
            // even on machines with only a few cores.
            //
            // This lets users see the committed block height changing in every progress log,
            // and avoids hangs due to out-of-order verifications flooding the CPUs.
            //
            // TODO:
            // - limit full verification concurrency based on block transaction counts?
            // - move more disk work to blocking tokio threads,
            //   and CPU work to the rayon thread pool inside blocking tokio threads
            full_verify_concurrency_limit: 20,

            // Use one thread per CPU.
            //
            // If this causes tokio executor starvation, move CPU-intensive tasks to rayon threads,
            // or reserve a few cores for tokio threads, based on `num_cpus()`.
            parallel_cpu_threads: 0,
        }
    }
}
