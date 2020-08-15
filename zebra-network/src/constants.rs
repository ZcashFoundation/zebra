//! Definitions of constants.

use std::time::Duration;

// XXX should these constants be split into protocol also?
use crate::protocol::external::types::*;

use zebra_chain::parameters::NetworkUpgrade;

/// The buffer size for the peer set.
pub const PEERSET_BUFFER_SIZE: usize = 10;

/// The timeout for requests made to a remote peer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The timeout for handshakes when connecting to new peers.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// We expect to receive a message from a live peer at least once in this time duration.
///
/// This is the sum of:
/// - the interval between connection heartbeats
/// - the timeout of a possible pending (already-sent) request
/// - the timeout for a possible queued request
/// - the timeout for the heartbeat request itself
///
/// This avoids explicit synchronization, but relies on the peer
/// connector actually setting up channels and these heartbeats in a
/// specific manner that matches up with this math.
pub const LIVE_PEER_DURATION: Duration = Duration::from_secs(60 + 10 + 10 + 10);

/// Regular interval for sending keepalive `Ping` messages to each
/// connected peer.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Truncate timestamps in outbound address messages to this time interval.
///
/// This is intended to prevent a peer from learning exactly when we received
/// messages from each of our peers.
pub const TIMESTAMP_TRUNCATION_SECONDS: i64 = 30 * 60;

/// The User-Agent string provided by the node.
///
/// This must be a valid [BIP 14] user agent.
///
/// [BIP 14]: https://github.com/bitcoin/bips/blob/master/bip-0014.mediawiki
pub const USER_AGENT: &str = "/🦓Zebra🦓:3.0.0-alpha.0/";

/// The Zcash network protocol version implemented by this crate, and advertised
/// during connection setup.
///
/// The current protocol version is checked by our peers. If it is too old,
/// newer peers will refuse to connect to us.
///
/// The current protocol version typically changes before Mainnet and Testnet
/// network upgrades.
pub const CURRENT_VERSION: Version = Version(170_012);

/// The most recent bilateral consensus upgrade implemented by this crate.
///
/// The minimum network upgrade is used to check the protocol versions of our
/// peers. If their versions are too old, we will disconnect from them.
//
// TODO: replace with NetworkUpgrade::current(network, height).
//       See the detailed comment in handshake.rs, where this constant is used.
pub const MIN_NETWORK_UPGRADE: NetworkUpgrade = NetworkUpgrade::Heartwood;

/// The default RTT estimate for peer responses.
pub const EWMA_DEFAULT_RTT: Duration = Duration::from_secs(1);

/// The decay time for the EWMA response time metric used for load balancing.
pub const EWMA_DECAY_TIME: Duration = Duration::from_secs(60);

/// Magic numbers used to identify different Zcash networks.
pub mod magics {
    use super::*;
    /// The production mainnet.
    pub const MAINNET: Magic = Magic([0x24, 0xe9, 0x27, 0x64]);
    /// The testnet.
    pub const TESTNET: Magic = Magic([0xfa, 0x1a, 0xf9, 0xbf]);
}

#[cfg(test)]
mod tests {

    use super::*;

    /// This assures that the `Duration` value we are computing for
    /// LIVE_PEER_DURATION actually matches the other const values it
    /// relies on.
    #[test]
    fn ensure_live_peer_duration_value_matches_others() {
        let constructed_live_peer_duration =
            HEARTBEAT_INTERVAL + REQUEST_TIMEOUT + REQUEST_TIMEOUT + REQUEST_TIMEOUT;

        assert_eq!(LIVE_PEER_DURATION, constructed_live_peer_duration);
    }
}
