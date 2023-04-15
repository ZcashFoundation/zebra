//! Configuration for semantic verification which is run in parallel.

use serde::{Deserialize, Serialize};

/// Configuration for parallel semantic verification:
/// <https://zebra.zfnd.org/dev/rfcs/0002-parallel-verification.html#definitions>
///
/// Automatically downloads the Zcash Sprout and Sapling parameters to the default directory:
/// - Linux: `$HOME/.zcash-params`
/// - macOS: `$HOME/Library/Application Support/ZcashParams`
/// - Windows: `%APPDATA%\ZcashParams` or `C:\Users\%USERNAME%\AppData\ZcashParams`
///
/// # Security
///
/// If you are running Zebra with elevated permissions ("root"), create the
/// parameters directory before running Zebra, and make sure the Zebra user
/// account has exclusive access to that directory, and other users can't modify
/// its parent directories.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Should Zebra make sure that it follows the consensus chain while syncing?
    /// This is a developer-only option.
    ///
    /// # Security
    ///
    /// Disabling this option leaves your node vulnerable to some kinds of chain-based attacks.
    /// Zebra regularly updates its checkpoints to ensure nodes are following the best chain.
    ///
    /// # Details
    ///
    /// This option is `true` by default, because it prevents some kinds of chain attacks.
    ///
    /// Disabling this option makes Zebra start full validation earlier.
    /// It is slower and less secure.
    ///
    /// Zebra requires some checkpoints to simplify validation of legacy network upgrades.
    /// Required checkpoints are always active, even when this option is `false`.
    ///
    /// # Deprecation
    ///
    /// For security reasons, this option might be deprecated or ignored in a future Zebra
    /// release.
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
