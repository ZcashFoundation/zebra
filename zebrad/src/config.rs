//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use serde::{Deserialize, Serialize};

/// Configuration for `zebrad`.
///
/// The `zebrad` config is a TOML-encoded version of this structure. The meaning
/// of each field is described in the documentation, although it may be necessary
/// to click through to the sub-structures for each section.
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct ZebradConfig {
    /// Consensus configuration
    pub consensus: zebra_consensus::Config,

    /// Metrics configuration
    pub metrics: crate::components::metrics::Config,

    /// Networking configuration
    pub network: zebra_network::Config,

    /// State configuration
    pub state: zebra_state::Config,

    /// Tracing configuration
    pub tracing: crate::components::tracing::Config,

    /// Sync configuration
    pub sync: crate::components::sync::Config,

    /// Mempool configuration
    pub mempool: crate::components::mempool::Config,

    /// RPC configuration
    pub rpc: zebra_rpc::config::Config,

    #[serde(skip_serializing_if = "zebra_rpc::config::mining::Config::skip_getblocktemplate")]
    /// Mining configuration
    pub mining: zebra_rpc::config::mining::Config,
}
