//! Definitions of constants.

use std::time::Duration;

// XXX should these constants be split into protocol also?
use crate::protocol::types::*;

/// The timeout for requests made to a remote peer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// We expect to receive a message from a live peer at least once in this time duration.
/// XXX this needs to be synchronized with the ping transmission times.
pub const LIVE_PEER_DURATION: Duration = Duration::from_secs(12);

/// The User-Agent string provided by the node.
pub const USER_AGENT: &'static str = "🦓Zebra v2.0.0-alpha.0🦓";

/// The Zcash network protocol version used on mainnet.
pub const CURRENT_VERSION: Version = Version(170_007);

/// The minimum version supported for peer connections.
pub const MIN_VERSION: Version = Version(170_007);

/// Magic numbers used to identify different Zcash networks.
pub mod magics {
    use super::*;
    /// The production mainnet.
    pub const MAINNET: Magic = Magic([0x24, 0xe9, 0x27, 0x64]);
    /// The testnet.
    pub const TESTNET: Magic = Magic([0xfa, 0x1a, 0xf9, 0xbf]);
}
