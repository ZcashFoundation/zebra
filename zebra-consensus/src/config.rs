use serde::{Deserialize, Serialize};

/// Consensus configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Should Zebra use its optional checkpoints to sync?
    ///
    /// This option is `true` by default, and allows for faster chain synchronization.
    ///
    /// Zebra requires some checkpoints to validate legacy network upgrades.
    /// But it also ships with optional checkpoints, which can be used instead of full block validation.
    ///
    /// Disabling this option makes Zebra start full validation as soon as possible.
    /// This helps developers debug Zebra, by running full validation on more blocks.
    ///
    /// Future versions of Zebra may change the required and optional checkpoints.
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
