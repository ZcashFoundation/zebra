//! Definitions of constants.

use std::time::Duration;

use lazy_static::lazy_static;
use regex::Regex;

// XXX should these constants be split into protocol also?
use crate::protocol::external::types::*;

use zebra_chain::parameters::NetworkUpgrade;

/// The buffer size for the peer set.
///
/// This should be greater than 1 to avoid sender contention, but also reasonably
/// small, to avoid queueing too many in-flight block downloads. (A large queue
/// of in-flight block downloads can choke a constrained local network
/// connection, or a small peer set on testnet.)
///
/// We assume that Zebra nodes have at least 10 Mbps bandwidth. Therefore, a
/// maximum-sized block can take up to 2 seconds to download. So the peer set
/// buffer adds up to 6 seconds worth of blocks to the queue.
pub const PEERSET_BUFFER_SIZE: usize = 3;

/// The timeout for requests made to a remote peer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The timeout for handshakes when connecting to new peers.
///
/// This timeout should remain small, because it helps stop slow peers getting
/// into the peer set. This is particularly important for network-constrained
/// nodes, and on testnet.
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
pub const LIVE_PEER_DURATION: Duration = Duration::from_secs(60 + 20 + 20 + 20);

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
// XXX can we generate this from crate metadata?
pub const USER_AGENT: &str = "/🦓Zebra🦓:1.0.0-alpha.3/";

/// The Zcash network protocol version implemented by this crate, and advertised
/// during connection setup.
///
/// The current protocol version is checked by our peers. If it is too old,
/// newer peers will refuse to connect to us.
///
/// The current protocol version typically changes before Mainnet and Testnet
/// network upgrades.
pub const CURRENT_VERSION: Version = Version(170_013);

/// The most recent bilateral consensus upgrade implemented by this crate.
///
/// The minimum network upgrade is used to check the protocol versions of our
/// peers. If their versions are too old, we will disconnect from them.
//
// TODO: replace with NetworkUpgrade::current(network, height).
//       See the detailed comment in handshake.rs, where this constant is used.
pub const MIN_NETWORK_UPGRADE: NetworkUpgrade = NetworkUpgrade::Canopy;

/// The default RTT estimate for peer responses.
///
/// We choose a high value for the default RTT, so that new peers must prove they
/// are fast, before we prefer them to other peers. This is particularly
/// important on testnet, which has a small number of peers, which are often
/// slow.
///
/// Make the default RTT one second higher than the response timeout.
pub const EWMA_DEFAULT_RTT: Duration = Duration::from_secs(20 + 1);

/// The decay time for the EWMA response time metric used for load balancing.
///
/// This should be much larger than the `SYNC_RESTART_TIMEOUT`, so we choose
/// better peers when we restart the sync.
pub const EWMA_DECAY_TIME: Duration = Duration::from_secs(200);

lazy_static! {
    /// OS-specific error when the port attempting to be opened is already in use.
    pub static ref PORT_IN_USE_ERROR: Regex = if cfg!(unix) {
        #[allow(clippy::trivial_regex)]
        Regex::new("already in use")
    } else {
        Regex::new("(access a socket in a way forbidden by its access permissions)|(Only one usage of each socket address)")
    }.expect("regex is valid");
}

/// The timeout for DNS lookups.
///
/// [6.1.3.3 Efficient Resource Usage] from [RFC 1123: Requirements for Internet Hosts]
/// suggest no less than 5 seconds for resolving timeout.
///
/// [RFC 1123: Requirements for Internet Hosts] https://tools.ietf.org/rfcmarkup?doc=1123
/// [6.1.3.3  Efficient Resource Usage] https://tools.ietf.org/rfcmarkup?doc=1123#page-77
pub const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

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
        zebra_test::init();

        let constructed_live_peer_duration =
            HEARTBEAT_INTERVAL + REQUEST_TIMEOUT + REQUEST_TIMEOUT + REQUEST_TIMEOUT;

        assert_eq!(LIVE_PEER_DURATION, constructed_live_peer_duration);
    }

    /// Make sure that the timeout values are consistent with each other.
    #[test]
    fn ensure_timeouts_consistent() {
        zebra_test::init();

        assert!(HANDSHAKE_TIMEOUT <= REQUEST_TIMEOUT,
                "Handshakes are requests, so the handshake timeout can't be longer than the timeout for all requests.");
        // This check is particularly important on testnet, which has a small
        // number of peers, which are often slow.
        assert!(EWMA_DEFAULT_RTT > REQUEST_TIMEOUT,
                "The default EWMA RTT should be higher than the request timeout, so new peers are required to prove they are fast, before we prefer them to other peers.");

        assert!(EWMA_DECAY_TIME > REQUEST_TIMEOUT,
                "The EWMA decay time should be higher than the request timeout, so timed out peers are penalised by the EWMA.");
    }
}
