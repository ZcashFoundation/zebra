use serde::{Deserialize, Serialize};

/// Consensus configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Should Zebra sync using post-Canopy checkpoints?
    ///
    /// This option is `true` by default, and allows for faster chain synchronization.
    ///
    /// Disabling this option forces Zebra to only use checkpoints until Canopy activation, which
    /// helps with debugging by forcing Zebra to validate more blocks.
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
            checkpoint_sync: true,
            debug_skip_parameter_preload: false,
        }
    }
}
