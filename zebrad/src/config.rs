//! Zebrad Config
//!
//! See instructions in `commands.rs` to specify the path to your
//! application's configuration file and/or command-line options
//! for specifying it.

use abscissa_core::Config;
use serde::{Deserialize, Serialize};

/// Zebrad Configuration
#[derive(Clone, Config, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ZebradConfig {
    /// Tracing configuration
    pub tracing: TracingSection,
}

/// Default configuration settings.
///
/// Note: if your needs are as simple as below, you can
/// use `#[derive(Default)]` on ZebradConfig instead.
impl Default for ZebradConfig {
    fn default() -> Self {
        Self {
            tracing: TracingSection::default(),
        }
    }
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
