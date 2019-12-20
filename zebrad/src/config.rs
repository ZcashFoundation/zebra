//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use serde::{Deserialize, Serialize};

use zebra_network::Config as NetworkSection;

/// Zebrad Configuration
#[derive(Clone, Default, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ZebradConfig {
    /// Tracing configuration
    pub tracing: TracingSection,
    /// Networking configuration,
    pub network: NetworkSection,
}

/// Tracing configuration section.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TracingSection {
    /// The filter used for tracing events.
    pub filter: String,
}

impl Default for TracingSection {
    fn default() -> Self {
        Self {
            filter: "info".to_owned(),
        }
    }
}
