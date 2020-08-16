//! The minimum difficulty block rule for Zcash.

use zebra_chain::{block, parameters::Network};

/// The testnet block height when minimum difficulty blocks start being
/// accepted.
pub(crate) const TESTNET_MINIMUM_DIFFICULTY_HEIGHT: block::Height = block::Height(299_188);

/// The Zcash Testnet consensus rules were changed to allow
/// minimum-difficulty blocks, shortly after Testnet Sapling activation.
/// See ZIP-205 and ZIP-208 for details.
///
/// This change represents a hard-fork on Testnet, but it doesn't appear on
/// Mainnet, so we handle it as an independent consensus rule change.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum MinimumDifficulty {
    /// Minimum difficulty blocks are rejected.
    ///
    /// Always returned for Mainnet blocks.
    Rejected,
    /// Minimum difficulty blocks are allowed.
    ///
    /// Only allowed for Testnet blocks.
    AllowedOnTestnet,
}

impl MinimumDifficulty {
    /// Returns the current minimum difficulty rule for `network` and `height`.
    pub fn current(network: Network, height: block::Height) -> MinimumDifficulty {
        use MinimumDifficulty::*;
        use Network::*;

        match network {
            Mainnet => Rejected,
            Testnet if (height >= TESTNET_MINIMUM_DIFFICULTY_HEIGHT) => AllowedOnTestnet,
            Testnet => Rejected,
        }
    }
}
