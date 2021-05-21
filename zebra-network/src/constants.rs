//! Definitions of constants.

use std::time::Duration;

use lazy_static::lazy_static;
use regex::Regex;

// XXX should these constants be split into protocol also?
use crate::protocol::external::types::*;

use zebra_chain::parameters::NetworkUpgrade;

/// The maximum number of [`GetAddr]` requests sent when crawling for new peers.
/// This is also the default fanout limit.
///
/// ## SECURITY
///
/// The fanout should be greater than 2, so that Zebra avoids getting a majority
/// of its initial address book entries from a single peer.
///
/// Zebra regularly crawls for new peers, initiating a new crawl every
/// [`crawl_new_peer_interval`].
///
/// TODO: limit the number of addresses that Zebra uses from a single peer
///       response (#1869)
///
/// [`GetAddr`]: [`crate::protocol::external::message::Message::GetAddr`]
/// [`crawl_new_peer_interval`](crate::config::Config.crawl_new_peer_interval).
///
/// Note: Zebra is currently very sensitive to fanout changes (#2193)
pub const MAX_GET_ADDR_FANOUT: usize = 3;

/// The fraction of live peers used for [`GetAddr`] fanouts.
///
/// `zcashd` rate-limits [`Addr`] responses. To avoid choking the peer set, we
/// only want send requests to a small fraction of peers.
///
/// [`GetAddr`]: [`crate::protocol::external::message::Message::GetAddr`]
/// [`Addr`]: [`crate::protocol::external::message::Message::Addr`]
///
/// Note: Zebra is currently very sensitive to fanout changes (#2193)
pub const GET_ADDR_FANOUT_LIVE_PEERS_DIVISOR: usize = 10;

/// Controls the number of peers used for each ObtainTips and ExtendTips request.
///
/// Note: Zebra is currently very sensitive to fanout changes (#2193)
pub const SYNC_FANOUT: usize = 4;

/// The buffer size for the peer set.
///
/// This should be greater than 1 to avoid sender contention, but also reasonably
/// small, to avoid queueing too many in-flight block downloads. (A large queue
/// of in-flight block downloads can choke a constrained local network
/// connection, or a small peer set on testnet.)
///
/// This should also be greater than [`MAX_GET_ADDR_FANOUT`], to avoid contention
/// during [`CandidateSet::update`].
///
/// We assume that Zebra nodes have at least 10 Mbps bandwidth. Therefore, a
/// maximum-sized block can take up to 2 seconds to download. So the peer set
/// buffer adds up to `PEERSET_BUFFER_SIZE*2` seconds worth of blocks to the
/// queue.
///
/// We want enough buffer to support concurrent fanouts, a sync download,
/// an inbound download, and some extra slots to avoid contention.
///
/// Note: Zebra is currently very sensitive to buffer size changes (#2193)
pub const PEERSET_BUFFER_SIZE: usize = MAX_GET_ADDR_FANOUT + SYNC_FANOUT + 3;

/// The timeout for DNS lookups.
///
/// [6.1.3.3 Efficient Resource Usage] from [RFC 1123: Requirements for Internet Hosts]
/// suggest no less than 5 seconds for resolving timeout.
///
/// [RFC 1123: Requirements for Internet Hosts]: https://tools.ietf.org/rfcmarkup?doc=1123
/// [6.1.3.3  Efficient Resource Usage]: https://tools.ietf.org/rfcmarkup?doc=1123#page-77
pub const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The minimum time between connections to initial or candidate peers.
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by making sure that new peer connections
/// are initiated at least `MIN_PEER_CONNECTION_INTERVAL` apart.
pub const MIN_PEER_CONNECTION_INTERVAL: Duration = Duration::from_millis(100);

/// The timeout for handshakes when connecting to new peers.
///
/// This timeout should remain small, because it helps stop slow peers getting
/// into the peer set. There is a tradeoff for network-constrained nodes and on
/// testnet:
/// - we want to avoid very slow nodes, but
/// - we want to have as many nodes as possible.
///
/// Note: Zebra is currently very sensitive to timing changes (#2193)
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);

/// The timeout for crawler [`GetAddr`] requests.
///
/// This timeout is a tradeoff between:
/// - ignored late responses because the timeout is too low, and
/// - peer set congestion from [`GetAddr`] requests dropped by peers.
///
/// Test changes to this timeout against `zcashd` peers on [`Testnet`], or
/// another small network.
///
/// [`GetAddr`]: [`crate::protocol::external::message::Message::GetAddr`]
/// [`Testnet`]: [`zebra_chain::parameters::Network::Testnet`]
///
/// Note: Zebra is currently very sensitive to timing changes (#2193)
pub const GET_ADDR_TIMEOUT: Duration = Duration::from_secs(8);

/// The maximum timeout for requests made to a remote peer.
///
/// [`PeerSet`] callers can set lower timeouts using [`tower::timeout`].
///
/// Note: Zebra is currently very sensitive to timing changes (#2193)
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

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
/// ## SECURITY
///
/// Timestamp truncation prevents a peer from learning exactly when we received
/// messages from each of our peers.
pub const TIMESTAMP_TRUNCATION_SECONDS: i64 = 30 * 60;

/// The maximum queue length for the timestamp collector. Further changes will
/// block until the task processes them.
///
/// This parameter provides a performance/memory tradeoff. It doesn't have much
/// impact on latency, because all queued changes are processed each time the
/// collector runs.
pub const TIMESTAMP_WORKER_BUFFER_SIZE: usize = 100;

/// The User-Agent string provided by the node.
///
/// This must be a valid [BIP 14] user agent.
///
/// [BIP 14]: https://github.com/bitcoin/bips/blob/master/bip-0014.mediawiki
// XXX can we generate this from crate metadata?
pub const USER_AGENT: &str = "/🦓Zebra🦓:1.0.0-alpha.8/";

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
// TODO: replace with NetworkUpgrade::current(network, height). (#1334)
pub const MIN_NETWORK_UPGRADE: NetworkUpgrade = NetworkUpgrade::Canopy;

/// The default RTT estimate for peer responses.
///
/// We choose a high value for the default RTT, so that new peers must prove they
/// are fast, before we prefer them to other peers. This is particularly
/// important on testnet, which has a small number of peers, which are often
/// slow.
///
/// Make the default RTT slightly higher than the request timeout.
pub const EWMA_DEFAULT_RTT: Duration = Duration::from_secs(REQUEST_TIMEOUT.as_secs() + 1);

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
    use std::cmp::min;

    /// Make sure that the fanout and buffer sizes are consistent with each other.
    #[test]
    fn ensure_buffers_consistent() {
        zebra_test::init();

        assert!(
            PEERSET_BUFFER_SIZE >= min(MAX_GET_ADDR_FANOUT, SYNC_FANOUT),
            "Zebra's peerset buffer should hold the smallest fanout"
        );
    }

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

        // Specific requests can't have timeouts longer than the maximum timeout
        assert!(HANDSHAKE_TIMEOUT <= REQUEST_TIMEOUT,
                "Handshakes are requests, so their timeout can't be longer than the timeout for all requests.");
        assert!(GET_ADDR_TIMEOUT <= REQUEST_TIMEOUT,
                "GetAddrs are requests, so their timeout can't be longer than the timeout for all requests.");

        // Other requests shouldn't have timeouts shorter than the handshake timeout
        assert!(GET_ADDR_TIMEOUT >= HANDSHAKE_TIMEOUT,
                "GetAddrs require a successful handshake, so their timeout shouldn't be shorter than the handshake timeout.");

        // Basic EWMA checks

        // The RTT check is particularly important on testnet, which has a small
        // number of peers, which are often slow.
        assert!(EWMA_DEFAULT_RTT > REQUEST_TIMEOUT,
                "The default EWMA RTT should be higher than the request timeout, so new peers are required to prove they are fast, before we prefer them to other peers.");

        assert!(EWMA_DECAY_TIME > REQUEST_TIMEOUT,
                "The EWMA decay time should be higher than the request timeout, so timed out peers are penalised by the EWMA.");
    }
}
