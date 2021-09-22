use serde::{Deserialize, Serialize};

/// Consensus configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Should Zebra sync using checkpoints?
    ///
    /// Setting this option to true enables post-Canopy checkpoints.
    /// (Zebra always checkpoints up to and including Canopy activation.)
    ///
    /// Future versions of Zebra may change the mandatory checkpoint
    /// height.
    pub checkpoint_sync: bool,
}

// we like our default configs to be explicit
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            checkpoint_sync: false,
        }
    }
}
