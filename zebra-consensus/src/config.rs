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
    /// Skip the pre-download of Groth16 parameters if this option is true.
    pub debug_skip_parameter_preload: bool,
}

// we like our default configs to be explicit
#[allow(unknown_lints)]
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            checkpoint_sync: false,
            debug_skip_parameter_preload: false,
        }
    }
}
