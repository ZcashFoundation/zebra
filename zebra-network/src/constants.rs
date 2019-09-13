//! Definitions of constants.

use crate::types::*;

/// The Zcash network protocol version used on mainnet.
pub const CURRENT_VERSION: Version = Version(170_007);

/// Magic numbers used to identify different Zcash networks.
pub mod magics {
    use super::*;
    /// The production mainnet.
    pub const MAINNET: Magic = Magic(0x6427e924);
    /// The testnet.
    pub const TESTNET: Magic = Magic(0xbff91afa);
}