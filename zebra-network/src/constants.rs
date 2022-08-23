//! Definitions of Zebra network constants, including:
//! - network protocol versions,
//! - network protocol user agents,
//! - peer address limits,
//! - peer connection limits, and
//! - peer connection timeouts.

use std::{collections::HashMap, time::Duration};

use lazy_static::lazy_static;
use regex::Regex;

// XXX should these constants be split into protocol also?
use crate::protocol::external::types::*;

use zebra_chain::{
    parameters::{
        Network::{self, *},
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

/// The timeout for handshakes when connecting to new peers.
///
/// This timeout should remain small, because it helps stop slow peers getting
/// into the peer set. This is particularly important for network-constrained
/// nodes, and on testnet.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);

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

/// The minimum time between successive calls to
/// [`CandidateSet::next`][crate::peer_set::CandidateSet::next].
///
/// ## Security
///
/// Zebra resists distributed denial of service attacks by making sure that new peer connections
/// are initiated at least [`MIN_PEER_CONNECTION_INTERVAL`] apart.
pub const MIN_PEER_CONNECTION_INTERVAL: Duration = Duration::from_millis(25);

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
pub const ADDR_RESPONSE_LIMIT_DENOMINATOR: usize = 3;

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

/// The User-Agent string provided by the node.
///
/// This must be a valid [BIP 14] user agent.
///
/// [BIP 14]: https://github.com/bitcoin/bips/blob/master/bip-0014.mediawiki
//
// TODO: generate this from crate metadata (#2375)
pub const USER_AGENT: &str = "/Zebra:1.0.0-beta.13/";

/// The Zcash network protocol version implemented by this crate, and advertised
/// during connection setup.
///
/// The current protocol version is checked by our peers. If it is too old,
/// newer peers will disconnect from us.
///
/// The current protocol version typically changes before Mainnet and Testnet
/// network upgrades.
pub const CURRENT_NETWORK_PROTOCOL_VERSION: Version = Version(170_100);

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
    pub static ref INITIAL_MIN_NETWORK_PROTOCOL_VERSION: HashMap<Network, Version> = {
        let mut hash_map = HashMap::new();

        hash_map.insert(Mainnet, Version::min_specified_for_upgrade(Mainnet, Nu5));
        hash_map.insert(Testnet, Version::min_specified_for_upgrade(Testnet, Nu5));

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

    use std::convert::TryFrom;

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
            u32::try_from(MAX_ADDRS_IN_ADDRESS_BOOK).expect("fits in u32")
                * MIN_PEER_CONNECTION_INTERVAL
                < MIN_PEER_RECONNECTION_DELAY,
            "each peer should get at least one connection attempt in each connection interval",
        );

        assert!(
            MIN_PEER_RECONNECTION_DELAY.as_secs()
                / (u32::try_from(MAX_ADDRS_IN_ADDRESS_BOOK).expect("fits in u32")
                    * MIN_PEER_CONNECTION_INTERVAL)
                    .as_secs()
                <= 2,
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
        // Zebra 1.0.0-beta.2 address book metrics in December 2021.
        const TYPICAL_MAINNET_ADDRESS_BOOK_SIZE: usize = 4_500;

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
            MAX_ADDRS_IN_ADDRESS_BOOK < TYPICAL_MAINNET_ADDRESS_BOOK_SIZE,
            "the address book limit should actually be used"
        );
    }

    /// Make sure inventory registry rotation is consistent with the target block interval.
    #[test]
    fn ensure_inventory_rotation_consistent() {
        let _init_guard = zebra_test::init();

        assert!(
            INVENTORY_ROTATION_INTERVAL
                < Duration::from_secs(
                    POST_BLOSSOM_POW_TARGET_SPACING
                        .try_into()
                        .expect("non-negative"),
                ),
            "we should expire inventory every time 1-2 new blocks get generated"
        );
    }
}
