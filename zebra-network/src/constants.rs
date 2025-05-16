//! Definitions of Zebra network constants, including:
//! - network protocol versions,
//! - network protocol user agents,
//! - peer address limits,
//! - peer connection limits, and
//! - peer connection timeouts.

use std::{collections::HashMap, time::Duration};

use lazy_static::lazy_static;
use regex::Regex;

// TODO: should these constants be split into protocol also?
use crate::protocol::external::types::*;

use zebra_chain::{
    parameters::{
        Network::{self, *},
        NetworkKind,
        NetworkUpgrade::*,
    },
    serialization::Duration32,
};

/// A multiplier used to calculate the inbound connection limit for the peer set,
///
/// When it starts up, Zebra opens [`Config.peerset_initial_target_size`]
/// outbound connections.
///
/// Then it opens additional outbound connections as needed for network requests,
/// and accepts inbound connections initiated by other peers.
///
/// The inbound and outbound connection limits are calculated from:
///
/// The inbound limit is:
/// `Config.peerset_initial_target_size * INBOUND_PEER_LIMIT_MULTIPLIER`.
/// (This is similar to `zcashd`'s default inbound limit.)
///
/// The outbound limit is:
/// `Config.peerset_initial_target_size * OUTBOUND_PEER_LIMIT_MULTIPLIER`.
/// (This is a bit larger than `zcashd`'s default outbound limit.)
///
/// # Security
///
/// Each connection requires one inbound slot and one outbound slot, on two different peers.
/// But some peers only make outbound connections, because they are behind a firewall,
/// or their lister port address is misconfigured.
///
/// Zebra allows extra inbound connection slots,
/// to prevent accidental connection slot exhaustion.
/// (`zcashd` also allows a large number of extra inbound slots.)
///
/// ## Security Tradeoff
///
/// Since the inbound peer limit is higher than the outbound peer limit,
/// Zebra can be connected to a majority of peers
/// that it has *not* chosen from its [`crate::AddressBook`].
///
/// Inbound peer connections are initiated by the remote peer,
/// so inbound peer selection is not controlled by the local node.
/// This means that an attacker can easily become a majority of a node's peers.
///
/// However, connection exhaustion is a higher priority.
pub const INBOUND_PEER_LIMIT_MULTIPLIER: usize = 5;

/// A multiplier used to calculate the outbound connection limit for the peer set,
///
/// See [`INBOUND_PEER_LIMIT_MULTIPLIER`] for details.
pub const OUTBOUND_PEER_LIMIT_MULTIPLIER: usize = 3;

/// The default maximum number of peer connections Zebra will keep for a given IP address
/// before it drops any additional peer connections with that IP.
///
/// This will be used as `Config.max_connections_per_ip` if no valid value is provided.
///
/// Note: Zebra will currently avoid initiating outbound connections where it
///       has recently had a successful handshake with any address
///       on that IP. Zebra will not initiate more than 1 outbound connection
///       to an IP based on the default configuration, but it will accept more inbound
///       connections to an IP.
pub const DEFAULT_MAX_CONNS_PER_IP: usize = 1;

/// The default peerset target size.
///
/// This will be used as `Config.peerset_initial_target_size` if no valid value is provided.
pub const DEFAULT_PEERSET_INITIAL_TARGET_SIZE: usize = 25;

/// The maximum number of peers we will add to the address book after each `getaddr` request.
pub const PEER_ADDR_RESPONSE_LIMIT: usize =
    DEFAULT_PEERSET_INITIAL_TARGET_SIZE * OUTBOUND_PEER_LIMIT_MULTIPLIER / 2;

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

/// The timeout for sending a message to a remote peer,
/// and receiving a response from a remote peer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The timeout for connections and handshakes when connecting to new peers.
///
/// Outbound TCP connections must complete within this timeout,
/// then the handshake messages get an additional `HANDSHAKE_TIMEOUT` to complete.
/// (Inbound TCP accepts can't have a timeout, because they are handled by the OS.)
///
/// This timeout should remain small, because it helps stop slow peers getting
/// into the peer set. This is particularly important for network-constrained
/// nodes, and on testnet.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

/// The maximum time difference for two address book changes to be considered concurrent.
///
/// This prevents simultaneous or nearby important changes or connection progress
/// being overridden by less important changes.
///
/// This timeout should be less than:
/// - the [peer reconnection delay](MIN_PEER_RECONNECTION_DELAY), and
/// - the [peer keepalive/heartbeat interval](HEARTBEAT_INTERVAL).
///
/// But more than:
/// - the amount of time between connection events and address book updates,
///   even under heavy load (in tests, we have observed delays up to 500ms),
/// - the delay between an outbound connection failing,
///   and the [CandidateSet](crate::peer_set::CandidateSet) registering the failure, and
/// - the delay between the application closing a connection,
///   and any remaining positive changes from the peer.
pub const CONCURRENT_ADDRESS_CHANGE_PERIOD: Duration = Duration::from_secs(5);

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
pub const MIN_PEER_RECONNECTION_DELAY: Duration = Duration::from_secs(59 + 20 + 20 + 20);

/// Zebra rotates its peer inventory registry every time this interval elapses.
///
/// After 2 of these intervals, Zebra's local available and missing inventory entries expire.
pub const INVENTORY_ROTATION_INTERVAL: Duration = Duration::from_secs(53);

/// The default peer address crawler interval.
///
/// This should be at least [`HANDSHAKE_TIMEOUT`] lower than all other crawler
/// intervals.
///
/// This makes the following sequence of events more likely:
/// 1. a peer address crawl,
/// 2. new peer connections,
/// 3. peer requests from other crawlers.
///
/// Using a prime number makes sure that peer address crawls
/// don't synchronise with other crawls.
pub const DEFAULT_CRAWL_NEW_PEER_INTERVAL: Duration = Duration::from_secs(61);

/// The peer address disk cache update interval.
///
/// This should be longer than [`DEFAULT_CRAWL_NEW_PEER_INTERVAL`],
/// but shorter than [`MAX_PEER_ACTIVE_FOR_GOSSIP`].
///
/// We use a short interval so Zebra instances which are restarted frequently
/// still have useful caches.
pub const PEER_DISK_CACHE_UPDATE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The maximum number of addresses in the peer disk cache.
///
/// This is chosen to be less than the number of active peers,
/// and approximately the same as the number of seed peers returned by DNS.
/// It is a tradeoff between fingerprinting attacks, DNS pollution risk, and cache pollution risk.
pub const MAX_PEER_DISK_CACHE_SIZE: usize = 75;

/// The maximum duration since a peer was last seen to consider it reachable.
///
/// This is used to prevent Zebra from gossiping addresses that are likely unreachable. Peers that
/// have last been seen more than this duration ago will not be gossiped.
///
/// This is determined as a tradeoff between network health and network view leakage. From the
/// [Bitcoin protocol documentation](https://en.bitcoin.it/wiki/Protocol_documentation#getaddr):
///
/// "The typical presumption is that a node is likely to be active if it has been sending a message
/// within the last three hours."
pub const MAX_PEER_ACTIVE_FOR_GOSSIP: Duration32 = Duration32::from_hours(3);

/// The maximum duration since a peer was last seen to consider reconnecting to it.
///
/// Peers that haven't been seen for more than three days and that had its last connection attempt
/// fail are considered to be offline and Zebra will stop trying to connect to them.
///
/// This is to ensure that Zebra can't have a denial-of-service as a consequence of having too many
/// offline peers that it constantly and uselessly retries to connect to.
pub const MAX_RECENT_PEER_AGE: Duration32 = Duration32::from_days(3);

/// Regular interval for sending keepalive `Ping` messages to each
/// connected peer.
///
/// Using a prime number makes sure that heartbeats don't synchronise with crawls.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(59);

/// The minimum time between outbound peer connections, implemented by
/// [`CandidateSet::next`][crate::peer_set::CandidateSet::next].
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by making sure that new outbound peer
/// connections are only initiated after this minimum time has elapsed.
///
/// It also enforces a minimum per-peer reconnection interval, and filters failed outbound peers.
pub const MIN_OUTBOUND_PEER_CONNECTION_INTERVAL: Duration = Duration::from_millis(100);

/// The minimum time between _successful_ inbound peer connections, implemented by
/// `peer_set::initialize::accept_inbound_connections`.
///
/// To support multiple peers connecting simultaneously, this is less than the
/// [`HANDSHAKE_TIMEOUT`].
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by limiting the inbound connection rate.
/// After a _successful_ inbound connection, new inbound peer connections are only accepted,
/// and our side of the handshake initiated, after this minimum time has elapsed.
///
/// The inbound interval is much longer than the outbound interval, because Zebra does not
/// control the selection or reconnections of inbound peers.
pub const MIN_INBOUND_PEER_CONNECTION_INTERVAL: Duration = Duration::from_secs(1);

/// The minimum time between _failed_ inbound peer connections, implemented by
/// `peer_set::initialize::accept_inbound_connections`.
///
/// This is a tradeoff between:
/// - the memory, CPU, and network usage of each new connection attempt, and
/// - denying service to honest peers due to an attack which makes many inbound connections.
///
/// Attacks that reach this limit should be managed using a firewall or intrusion prevention system.
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by limiting the inbound connection rate.
/// After a _failed_ inbound connection, new inbound peer connections are only accepted,
/// and our side of the handshake initiated, after this minimum time has elapsed.
pub const MIN_INBOUND_PEER_FAILED_CONNECTION_INTERVAL: Duration = Duration::from_millis(10);

/// The minimum time between successive calls to
/// [`CandidateSet::update`][crate::peer_set::CandidateSet::update].
///
/// Using a prime number makes sure that peer address crawls don't synchronise with other crawls.
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by making sure that requests for more
/// peer addresses are sent at least [`MIN_PEER_GET_ADDR_INTERVAL`] apart.
pub const MIN_PEER_GET_ADDR_INTERVAL: Duration = Duration::from_secs(31);

/// The combined timeout for all the requests in
/// [`CandidateSet::update`][crate::peer_set::CandidateSet::update].
///
/// `zcashd` doesn't respond to most `getaddr` requests,
/// so this timeout needs to be short.
pub const PEER_GET_ADDR_TIMEOUT: Duration = Duration::from_secs(8);

/// The number of GetAddr requests sent when crawling for new peers.
///
/// # Security
///
/// The fanout should be greater than 2, so that Zebra avoids getting a majority
/// of its initial address book entries from a single peer.
///
/// Zebra regularly crawls for new peers, initiating a new crawl every
/// [`crawl_new_peer_interval`](crate::config::Config.crawl_new_peer_interval).
///
/// TODO: Restore the fanout to 3, once fanouts are limited to the number of ready peers (#2214)
///
/// In #3110, we changed the fanout to 1, to make sure we actually use cached address responses.
/// With a fanout of 3, we were dropping a lot of responses, because the overall crawl timed out.
pub const GET_ADDR_FANOUT: usize = 1;

/// The maximum number of addresses allowed in an `addr` or `addrv2` message.
///
/// `addr`:
/// > The number of IP address entries up to a maximum of 1,000.
///
/// <https://developer.bitcoin.org/reference/p2p_networking.html#addr>
///
/// `addrv2`:
/// > One message can contain up to 1,000 addresses.
/// > Clients MUST reject messages with more addresses.
///
/// <https://zips.z.cash/zip-0155#specification>
pub const MAX_ADDRS_IN_MESSAGE: usize = 1000;

/// The fraction of addresses Zebra sends in response to a `Peers` request.
///
/// Each response contains approximately:
/// `address_book.len() / ADDR_RESPONSE_LIMIT_DENOMINATOR`
/// addresses, selected at random from the address book.
///
/// # Security
///
/// This limit makes sure that Zebra does not reveal its entire address book
/// in a single `Peers` response.
pub const ADDR_RESPONSE_LIMIT_DENOMINATOR: usize = 4;

/// The maximum number of addresses Zebra will keep in its address book.
///
/// This is a tradeoff between:
/// - revealing the whole address book in a few requests,
/// - sending the maximum number of peer addresses, and
/// - making sure the limit code actually gets run.
pub const MAX_ADDRS_IN_ADDRESS_BOOK: usize =
    MAX_ADDRS_IN_MESSAGE * (ADDR_RESPONSE_LIMIT_DENOMINATOR + 1);

/// Truncate timestamps in outbound address messages to this time interval.
///
/// ## SECURITY
///
/// Timestamp truncation prevents a peer from learning exactly when we received
/// messages from each of our peers.
pub const TIMESTAMP_TRUNCATION_SECONDS: u32 = 30 * 60;

/// The Zcash network protocol version implemented by this crate, and advertised
/// during connection setup.
///
/// The current protocol version is checked by our peers. If it is too old,
/// newer peers will disconnect from us.
///
/// The current protocol version typically changes before Mainnet and Testnet
/// network upgrades.
///
/// This version of Zebra draws the current network protocol version from
/// [ZIP-253](https://zips.z.cash/zip-0253).
// TODO: Update this constant to the correct value after NU6.1 & NU7 activation,
// pub const CURRENT_NETWORK_PROTOCOL_VERSION: Version = Version(170_140); // NU6.1
// pub const CURRENT_NETWORK_PROTOCOL_VERSION: Version = Version(170_160); // NU7
pub const CURRENT_NETWORK_PROTOCOL_VERSION: Version = Version(170_120);

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
pub const EWMA_DECAY_TIME_NANOS: f64 = 200.0 * NANOS_PER_SECOND;

/// The number of nanoseconds in one second.
const NANOS_PER_SECOND: f64 = 1_000_000_000.0;

/// The duration it takes for the drop probability of an overloaded connection to
/// reach [`MIN_OVERLOAD_DROP_PROBABILITY`].
///
/// Peer connections that receive multiple overloads have a higher probability of being dropped.
///
/// The probability of a connection being dropped gradually decreases during this interval
/// until it reaches the default drop probability ([`MIN_OVERLOAD_DROP_PROBABILITY`]).
///
/// Increasing this number increases the rate at which connections are dropped.
pub const OVERLOAD_PROTECTION_INTERVAL: Duration = MIN_INBOUND_PEER_CONNECTION_INTERVAL;

/// The minimum probability of dropping a peer connection when it receives an
/// [`Overloaded`](crate::PeerError::Overloaded) error.
pub const MIN_OVERLOAD_DROP_PROBABILITY: f32 = 0.05;

/// The maximum probability of dropping a peer connection when it receives an
/// [`Overloaded`](crate::PeerError::Overloaded) error.
pub const MAX_OVERLOAD_DROP_PROBABILITY: f32 = 0.5;

/// The minimum interval between logging peer set status updates.
pub const MIN_PEER_SET_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The maximum number of peer misbehavior incidents before a peer is
/// disconnected and banned.
pub const MAX_PEER_MISBEHAVIOR_SCORE: u32 = 100;

/// The maximum number of banned IP addresses to be stored in-memory at any time.
pub const MAX_BANNED_IPS: usize = 20_000;

lazy_static! {
    /// The minimum network protocol version accepted by this crate for each network,
    /// represented as a network upgrade.
    ///
    /// The minimum protocol version is used to check the protocol versions of our
    /// peers during the initial block download. After the initial block download,
    /// we use the current block height to select the minimum network protocol
    /// version.
    ///
    /// If peer versions are too old, we will disconnect from them.
    ///
    /// The minimum network protocol version typically changes after Mainnet and
    /// Testnet network upgrades.
    // TODO: Change `Nu6` to `Nu7` after NU7 activation.
    // TODO: Move the value here to a field on `testnet::Parameters` (#8367)
    pub static ref INITIAL_MIN_NETWORK_PROTOCOL_VERSION: HashMap<NetworkKind, Version> = {
        let mut hash_map = HashMap::new();

        hash_map.insert(NetworkKind::Mainnet, Version::min_specified_for_upgrade(&Mainnet, Nu6));
        hash_map.insert(NetworkKind::Testnet, Version::min_specified_for_upgrade(&Network::new_default_testnet(), Nu6));
        hash_map.insert(NetworkKind::Regtest, Version::min_specified_for_upgrade(&Network::new_regtest(Default::default()), Nu6));

        hash_map
    };

    /// OS-specific error when the port attempting to be opened is already in use.
    pub static ref PORT_IN_USE_ERROR: Regex = if cfg!(unix) {
        #[allow(clippy::trivial_regex)]
        Regex::new(&regex::escape("already in use"))
    } else {
        Regex::new("(access a socket in a way forbidden by its access permissions)|(Only one usage of each socket address)")
    }.expect("regex is valid");
}

/// The timeout for DNS lookups.
///
/// [6.1.3.3 Efficient Resource Usage] from [RFC 1123: Requirements for Internet Hosts]
/// suggest no less than 5 seconds for resolving timeout.
///
/// [RFC 1123: Requirements for Internet Hosts] <https://tools.ietf.org/rfcmarkup?doc=1123>
/// [6.1.3.3  Efficient Resource Usage] <https://tools.ietf.org/rfcmarkup?doc=1123#page-77>
pub const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use zebra_chain::parameters::POST_BLOSSOM_POW_TARGET_SPACING;

    use super::*;

    /// This assures that the `Duration` value we are computing for
    /// [`MIN_PEER_RECONNECTION_DELAY`] actually matches the other const values
    /// it relies on.
    #[test]
    fn ensure_live_peer_duration_value_matches_others() {
        let _init_guard = zebra_test::init();

        let constructed_live_peer_duration =
            HEARTBEAT_INTERVAL + REQUEST_TIMEOUT + REQUEST_TIMEOUT + REQUEST_TIMEOUT;

        assert_eq!(MIN_PEER_RECONNECTION_DELAY, constructed_live_peer_duration);
    }

    /// Make sure that the timeout values are consistent with each other.
    #[test]
    fn ensure_timeouts_consistent() {
        let _init_guard = zebra_test::init();

        assert!(HANDSHAKE_TIMEOUT <= REQUEST_TIMEOUT,
                "Handshakes are requests, so the handshake timeout can't be longer than the timeout for all requests.");
        // This check is particularly important on testnet, which has a small
        // number of peers, which are often slow.
        assert!(EWMA_DEFAULT_RTT > REQUEST_TIMEOUT,
                "The default EWMA RTT should be higher than the request timeout, so new peers are required to prove they are fast, before we prefer them to other peers.");

        let request_timeout_nanos = REQUEST_TIMEOUT.as_secs_f64()
            + f64::from(REQUEST_TIMEOUT.subsec_nanos()) * NANOS_PER_SECOND;

        assert!(EWMA_DECAY_TIME_NANOS > request_timeout_nanos,
                "The EWMA decay time should be higher than the request timeout, so timed out peers are penalised by the EWMA.");

        assert!(
            MIN_PEER_RECONNECTION_DELAY.as_secs() as f32
                / (u32::try_from(MAX_ADDRS_IN_ADDRESS_BOOK).expect("fits in u32")
                    * MIN_OUTBOUND_PEER_CONNECTION_INTERVAL)
                    .as_secs() as f32
                >= 0.2,
            "some peers should get a connection attempt in each connection interval",
        );

        assert!(
            MIN_PEER_RECONNECTION_DELAY.as_secs() as f32
                / (u32::try_from(MAX_ADDRS_IN_ADDRESS_BOOK).expect("fits in u32")
                    * MIN_OUTBOUND_PEER_CONNECTION_INTERVAL)
                    .as_secs() as f32
                <= 2.0,
            "each peer should only have a few connection attempts in each connection interval",
        );
    }

    /// Make sure that peer age limits are consistent with each other.
    #[test]
    fn ensure_peer_age_limits_consistent() {
        let _init_guard = zebra_test::init();

        assert!(
            MAX_PEER_ACTIVE_FOR_GOSSIP <= MAX_RECENT_PEER_AGE,
            "we should only gossip peers we are actually willing to try ourselves"
        );
    }

    /// Make sure the address limits are consistent with each other.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn ensure_address_limits_consistent() {
        // Estimated network address book size in November 2023, after the address book limit was increased.
        // Zebra 1.0.0-beta.2 address book metrics in December 2021 showed 4500 peers.
        const TYPICAL_MAINNET_ADDRESS_BOOK_SIZE: usize = 5_500;

        let _init_guard = zebra_test::init();

        assert!(
            MAX_ADDRS_IN_ADDRESS_BOOK >= GET_ADDR_FANOUT * MAX_ADDRS_IN_MESSAGE,
            "the address book should hold at least a fanout's worth of addresses"
        );

        assert!(
            MAX_ADDRS_IN_ADDRESS_BOOK / ADDR_RESPONSE_LIMIT_DENOMINATOR > MAX_ADDRS_IN_MESSAGE,
            "the address book should hold enough addresses for a full response"
        );

        assert!(
            MAX_ADDRS_IN_ADDRESS_BOOK <= TYPICAL_MAINNET_ADDRESS_BOOK_SIZE,
            "the address book limit should actually be used"
        );
    }

    /// Make sure inventory registry rotation is consistent with the target block interval.
    #[test]
    fn ensure_inventory_rotation_consistent() {
        let _init_guard = zebra_test::init();

        assert!(
            INVENTORY_ROTATION_INTERVAL
                < Duration::from_secs(POST_BLOSSOM_POW_TARGET_SPACING.into()),
            "we should expire inventory every time 1-2 new blocks get generated"
        );
    }
}
