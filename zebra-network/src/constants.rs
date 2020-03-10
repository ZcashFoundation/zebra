//! Definitions of constants.

use std::time::Duration;

// XXX should these constants be split into protocol also?
use crate::protocol::external::types::*;

/// The timeout for requests made to a remote peer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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
/// This is intended to prevent a peer from learning exactly when we recieved
/// messages from each of our peers.
pub const TIMESTAMP_TRUNCATION_SECONDS: i64 = 30 * 60;

/// The User-Agent string provided by the node.
pub const USER_AGENT: &str = "🦓Zebra v2.0.0-alpha.0🦓";

/// The Zcash network protocol version used on mainnet.
pub const CURRENT_VERSION: Version = Version(170_009);

/// The minimum version supported for peer connections.
pub const MIN_VERSION: Version = Version(170_009);

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
