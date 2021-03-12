use serde::{Deserialize, Serialize};

/// Consensus configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Should Zebra sync using checkpoints?
    ///
    /// Setting this option to true enables post-Canopy checkpoints.
    /// (Zebra always checkpoints on Canopy activation.)
    pub checkpoint_sync: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            checkpoint_sync: false,
        }
    }
}
