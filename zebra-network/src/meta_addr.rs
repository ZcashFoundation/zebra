//! An address-with-metadata type used in Bitcoin networking.

use std::{
    cmp::{max, Ordering},
    time::Instant,
};

use chrono::Utc;

use zebra_chain::{parameters::Network, serialization::DateTime32};

use crate::{
    constants,
    peer::{address_is_valid_for_outbound_connections, PeerPreference},
    protocol::{external::canonical_peer_addr, types::PeerServices},
};

use MetaAddrChange::*;
use PeerAddrState::*;

pub mod peer_addr;

pub use peer_addr::PeerSocketAddr;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use crate::protocol::external::arbitrary::canonical_peer_addr_strategy;

#[cfg(any(test, feature = "proptest-impl"))]
pub(crate) mod arbitrary;

#[cfg(test)]
pub(crate) mod tests;

/// Peer connection state, based on our interactions with the peer.
///
/// Zebra also tracks how recently a peer has sent us messages, and derives peer
/// liveness based on the current time. This derived state is tracked using
/// [`maybe_connected_peers`][mcp] and
/// [`reconnection_peers`][rp].
///
/// [mcp]: crate::AddressBook::maybe_connected_peers
/// [rp]: crate::AddressBook::reconnection_peers
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum PeerAddrState {
    /// The peer has sent us a valid message.
    ///
    /// Peers remain in this state, even if they stop responding to requests.
    /// (Peer liveness is derived from the `last_seen` timestamp, and the current
    /// time.)
    Responded,

    /// The peer's address has just been fetched from a DNS seeder, or via peer
    /// gossip, or as part of a `Version` message, or guessed from an inbound remote IP,
    /// but we haven't attempted to connect to it yet.
    NeverAttemptedGossiped,

    /// The peer's TCP connection failed, or the peer sent us an unexpected
    /// Zcash protocol message, so we failed the connection.
    Failed,

    /// We just started a connection attempt to this peer.
    AttemptPending,
}

impl PeerAddrState {
    /// Return true if this state is a "never attempted" state.
    pub fn is_never_attempted(&self) -> bool {
        match self {
            NeverAttemptedGossiped => true,
            AttemptPending | Responded | Failed => false,
        }
    }

    /// Returns the typical connection state machine order of `self` and `other`.
    /// Partially ordered states are sorted in connection attempt order.
    ///
    /// See [`MetaAddrChange::apply_to_meta_addr()`] for more details.
    fn connection_state_order(&self, other: &Self) -> Ordering {
        use Ordering::*;
        match (self, other) {
            _ if self == other => Equal,
            // Peers start in the "never attempted" state,
            // then typically progress towards a "responded" or "failed" state.
            (NeverAttemptedGossiped, _) => Less,
            (_, NeverAttemptedGossiped) => Greater,
            (AttemptPending, _) => Less,
            (_, AttemptPending) => Greater,
            (Responded, _) => Less,
            (_, Responded) => Greater,
            // These patterns are redundant, but Rust doesn't assume that `==` is reflexive,
            // so the first is still required (but unreachable).
            (Failed, _) => Less,
            //(_, Failed) => Greater,
        }
    }
}

// non-test code should explicitly specify the peer address state
#[cfg(test)]
#[allow(clippy::derivable_impls)]
impl Default for PeerAddrState {
    fn default() -> Self {
        NeverAttemptedGossiped
    }
}

impl Ord for PeerAddrState {
    /// `PeerAddrState`s are sorted in approximate reconnection attempt
    /// order, ignoring liveness.
    ///
    /// See [`CandidateSet`] and [`MetaAddr::cmp`] for more details.
    ///
    /// [`CandidateSet`]: super::peer_set::CandidateSet
    fn cmp(&self, other: &Self) -> Ordering {
        use Ordering::*;
        match (self, other) {
            _ if self == other => Equal,
            // We reconnect to `Responded` peers that have stopped sending messages,
            // then `NeverAttempted` peers, then `Failed` peers
            (Responded, _) => Less,
            (_, Responded) => Greater,
            (NeverAttemptedGossiped, _) => Less,
            (_, NeverAttemptedGossiped) => Greater,
            (Failed, _) => Less,
            (_, Failed) => Greater,
            // These patterns are redundant, but Rust doesn't assume that `==` is reflexive,
            // so the first is still required (but unreachable).
            (AttemptPending, _) => Less,
            //(_, AttemptPending) => Greater,
        }
    }
}

impl PartialOrd for PeerAddrState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// An address with metadata on its advertised services and last-seen time.
///
/// This struct can be created from `addr` or `addrv2` messages.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct MetaAddr {
    /// The peer's canonical socket address.
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "canonical_peer_addr_strategy()")
    )]
    //
    // TODO: make addr private, so the constructors can make sure it is a
    // canonical SocketAddr (#2357)
    pub(crate) addr: PeerSocketAddr,

    /// The services advertised by the peer.
    ///
    /// The exact meaning depends on `last_connection_state`:
    ///   - `Responded`: the services advertised by this peer, the last time we
    ///     performed a handshake with it
    ///   - `NeverAttempted`: the unverified services advertised by another peer,
    ///     then gossiped by the peer that sent us this address
    ///   - `Failed` or `AttemptPending`: unverified services via another peer,
    ///     or services advertised in a previous handshake
    ///
    /// ## Security
    ///
    /// `services` from `NeverAttempted` peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    //
    // TODO: make services private
    //       split gossiped and handshake services? (#2324)
    pub(crate) services: Option<PeerServices>,

    /// The unverified "last seen time" gossiped by the remote peer that sent us
    /// this address.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    untrusted_last_seen: Option<DateTime32>,

    /// The last time we received a message from this peer.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    last_response: Option<DateTime32>,

    /// The last time we tried to open an outbound connection to this peer.
    ///
    /// See the [`MetaAddr::last_attempt`] method for details.
    last_attempt: Option<Instant>,

    /// The last time our outbound connection with this peer failed.
    ///
    /// See the [`MetaAddr::last_failure`] method for details.
    last_failure: Option<Instant>,

    /// The misbehavior score for this peer.
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(value = 0))]
    misbehavior_score: u32,

    /// The outcome of our most recent communication attempt with this peer.
    //
    // TODO: move the time and services fields into PeerAddrState?
    //       then some fields could be required in some states
    pub(crate) last_connection_state: PeerAddrState,

    /// Whether this peer address was added to the address book
    /// when the peer made an inbound connection.
    is_inbound: bool,
}

/// A change to an existing `MetaAddr`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum MetaAddrChange {
    // TODO:
    // - split the common `addr` field into an outer struct
    //
    /// Creates a `MetaAddr` for an initial peer.
    NewInitial {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
    },

    /// Creates a new gossiped `MetaAddr`.
    NewGossiped {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
        untrusted_services: PeerServices,
        untrusted_last_seen: DateTime32,
    },

    /// Creates new local listener `MetaAddr`.
    NewLocal {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
    },

    /// Updates an existing `MetaAddr` when an outbound connection attempt
    /// starts.
    UpdateAttempt {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
    },

    /// Updates an existing `MetaAddr` when we've made a successful connection with a peer.
    UpdateConnected {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
        services: PeerServices,
        is_inbound: bool,
    },

    /// Updates an existing `MetaAddr` when a peer responds with a message.
    UpdateResponded {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
    },

    /// Updates an existing `MetaAddr` when a peer fails.
    UpdateFailed {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_peer_addr_strategy()")
        )]
        addr: PeerSocketAddr,
        services: Option<PeerServices>,
    },

    /// Updates an existing `MetaAddr` when a peer misbehaves such as by advertising
    /// semantically invalid blocks or transactions.
    #[cfg_attr(any(test, feature = "proptest-impl"), proptest(skip))]
    UpdateMisbehavior {
        addr: PeerSocketAddr,
        score_increment: u32,
    },
}

impl MetaAddr {
    /// Returns a [`MetaAddrChange::NewInitial`] for a peer that was excluded from
    /// the list of the initial peers.
    pub fn new_initial_peer(addr: PeerSocketAddr) -> MetaAddrChange {
        NewInitial {
            addr: canonical_peer_addr(addr),
        }
    }

    /// Returns a new `MetaAddr`, based on the deserialized fields from a
    /// gossiped peer [`Addr`][crate::protocol::external::Message::Addr] message.
    pub fn new_gossiped_meta_addr(
        addr: PeerSocketAddr,
        untrusted_services: PeerServices,
        untrusted_last_seen: DateTime32,
    ) -> MetaAddr {
        MetaAddr {
            addr: canonical_peer_addr(addr),
            services: Some(untrusted_services),
            untrusted_last_seen: Some(untrusted_last_seen),
            last_response: None,
            last_attempt: None,
            last_failure: None,
            last_connection_state: NeverAttemptedGossiped,
            misbehavior_score: 0,
            is_inbound: false,
        }
    }

    /// Returns a [`MetaAddrChange::NewGossiped`], based on a gossiped peer
    /// [`MetaAddr`].
    ///
    /// Returns [`None`] if the gossiped peer is missing the untrusted services field.
    #[allow(clippy::unwrap_in_result)]
    pub fn new_gossiped_change(self) -> Option<MetaAddrChange> {
        let untrusted_services = self.services?;

        Some(NewGossiped {
            addr: canonical_peer_addr(self.addr),
            untrusted_services,
            untrusted_last_seen: self
                .untrusted_last_seen
                .expect("unexpected missing last seen"),
        })
    }

    /// Returns a [`MetaAddrChange::UpdateConnected`] for a peer that has just successfully
    /// connected.
    ///
    /// # Security
    ///
    /// This address must be the remote address from an outbound connection,
    /// and the services must be the services from that peer's handshake.
    ///
    /// Otherwise:
    /// - malicious peers could interfere with other peers' [`AddressBook`](crate::AddressBook)
    ///   state, or
    /// - Zebra could advertise unreachable addresses to its own peers.
    pub fn new_connected(
        addr: PeerSocketAddr,
        services: &PeerServices,
        is_inbound: bool,
    ) -> MetaAddrChange {
        UpdateConnected {
            addr: canonical_peer_addr(*addr),
            services: *services,
            is_inbound,
        }
    }

    /// Returns a [`MetaAddrChange::UpdateResponded`] for a peer that has just
    /// sent us a message.
    ///
    /// # Security
    ///
    /// This address must be the remote address from an outbound connection.
    ///
    /// Otherwise:
    /// - malicious peers could interfere with other peers' [`AddressBook`](crate::AddressBook)
    ///   state, or
    /// - Zebra could advertise unreachable addresses to its own peers.
    pub fn new_responded(addr: PeerSocketAddr) -> MetaAddrChange {
        UpdateResponded {
            addr: canonical_peer_addr(*addr),
        }
    }

    /// Returns a [`MetaAddrChange::UpdateAttempt`] for a peer that we
    /// want to make an outbound connection to.
    pub fn new_reconnect(addr: PeerSocketAddr) -> MetaAddrChange {
        UpdateAttempt {
            addr: canonical_peer_addr(*addr),
        }
    }

    /// Returns a [`MetaAddrChange::NewLocal`] for our own listener address.
    pub fn new_local_listener_change(addr: impl Into<PeerSocketAddr>) -> MetaAddrChange {
        NewLocal {
            addr: canonical_peer_addr(addr),
        }
    }

    /// Returns a [`MetaAddrChange::UpdateFailed`] for a peer that has just had an error.
    pub fn new_errored(
        addr: PeerSocketAddr,
        services: impl Into<Option<PeerServices>>,
    ) -> MetaAddrChange {
        UpdateFailed {
            addr: canonical_peer_addr(*addr),
            services: services.into(),
        }
    }

    /// Create a new `MetaAddr` for a peer that has just shut down.
    pub fn new_shutdown(addr: PeerSocketAddr) -> MetaAddrChange {
        // TODO: if the peer shut down in the Responded state, preserve that
        // state. All other states should be treated as (timeout) errors.
        MetaAddr::new_errored(addr, None)
    }

    /// Return the address for this `MetaAddr`.
    pub fn addr(&self) -> PeerSocketAddr {
        self.addr
    }

    /// Return the address preference level for this `MetaAddr`.
    pub fn peer_preference(&self) -> Result<PeerPreference, &'static str> {
        PeerPreference::new(self.addr, None)
    }

    /// Returns the time of the last successful interaction with this peer.
    ///
    /// Initially set to the unverified "last seen time" gossiped by the remote
    /// peer that sent us this address.
    ///
    /// If the `last_connection_state` has ever been `Responded`, this field is
    /// set to the last time we processed a message from this peer.
    ///
    /// ## Security
    ///
    /// `last_seen` times from peers that have never `Responded` may be
    /// incorrect due to clock skew, or buggy or malicious peers.
    pub fn last_seen(&self) -> Option<DateTime32> {
        self.last_response.or(self.untrusted_last_seen)
    }

    /// Returns whether the address is from an inbound peer connection
    pub fn is_inbound(&self) -> bool {
        self.is_inbound
    }

    /// Returns the unverified "last seen time" gossiped by the remote peer that
    /// sent us this address.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    //
    // TODO: pub(in crate::address_book) - move meta_addr into address_book
    pub(crate) fn untrusted_last_seen(&self) -> Option<DateTime32> {
        self.untrusted_last_seen
    }

    /// Returns the last time we received a message from this peer.
    ///
    /// See the [`MetaAddr::last_seen`] method for details.
    //
    // TODO: pub(in crate::address_book) - move meta_addr into address_book
    #[allow(dead_code)]
    pub(crate) fn last_response(&self) -> Option<DateTime32> {
        self.last_response
    }

    /// Set the gossiped untrusted last seen time for this peer.
    pub(crate) fn set_untrusted_last_seen(&mut self, untrusted_last_seen: DateTime32) {
        self.untrusted_last_seen = Some(untrusted_last_seen);
    }

    /// Returns the time of our last outbound connection attempt with this peer.
    ///
    /// If the `last_connection_state` has ever been `AttemptPending`, this
    /// field is set to the last time we started an outbound connection attempt
    /// with this peer.
    pub fn last_attempt(&self) -> Option<Instant> {
        self.last_attempt
    }

    /// Returns the time of our last failed outbound connection with this peer.
    ///
    /// If the `last_connection_state` has ever been `Failed`, this field is set
    /// to the last time:
    /// - a connection attempt failed, or
    /// - an open connection encountered a fatal protocol error.
    pub fn last_failure(&self) -> Option<Instant> {
        self.last_failure
    }

    /// Have we had any recently messages from this peer?
    ///
    /// Returns `true` if the peer is likely connected and responsive in the peer
    /// set.
    ///
    /// [`constants::MIN_PEER_RECONNECTION_DELAY`] represents the time interval in which
    /// we should receive at least one message from a peer, or close the
    /// connection. Therefore, if the last-seen timestamp is older than
    /// [`constants::MIN_PEER_RECONNECTION_DELAY`] ago, we know we should have
    /// disconnected from it. Otherwise, we could potentially be connected to it.
    pub fn has_connection_recently_responded(&self, now: chrono::DateTime<Utc>) -> bool {
        if let Some(last_response) = self.last_response {
            // Recent times and future times are considered live
            last_response.saturating_elapsed(now)
                <= constants::MIN_PEER_RECONNECTION_DELAY
                    .try_into()
                    .expect("unexpectedly large constant")
        } else {
            // If there has never been any response, it can't possibly be live
            false
        }
    }

    /// Have we recently attempted an outbound connection to this peer?
    ///
    /// Returns `true` if this peer was recently attempted, or has a connection
    /// attempt in progress.
    pub fn was_connection_recently_attempted(&self, now: Instant) -> bool {
        if let Some(last_attempt) = self.last_attempt {
            // Recent times and future times are considered live.
            // Instants are monotonic, so `now` should always be later than `last_attempt`,
            // except for synthetic data in tests.
            now.saturating_duration_since(last_attempt) <= constants::MIN_PEER_RECONNECTION_DELAY
        } else {
            // If there has never been any attempt, it can't possibly be live
            false
        }
    }

    /// Have we recently had a failed connection to this peer?
    ///
    /// Returns `true` if this peer has recently failed.
    pub fn has_connection_recently_failed(&self, now: Instant) -> bool {
        if let Some(last_failure) = self.last_failure {
            // Recent times and future times are considered live
            now.saturating_duration_since(last_failure) <= constants::MIN_PEER_RECONNECTION_DELAY
        } else {
            // If there has never been any failure, it can't possibly be recent
            false
        }
    }

    /// Returns true if this peer has recently sent us a message.
    pub fn was_recently_live(&self, now: chrono::DateTime<Utc>) -> bool {
        // NeverAttempted, Failed, and AttemptPending peers should never be live
        self.last_connection_state == PeerAddrState::Responded
            && self.has_connection_recently_responded(now)
    }

    /// Has this peer been seen recently?
    ///
    /// Returns `true` if this peer has responded recently or if the peer was gossiped with a
    /// recent reported last seen time.
    ///
    /// [`constants::MAX_PEER_ACTIVE_FOR_GOSSIP`] represents the maximum time since a peer was seen
    /// to still be considered reachable.
    pub fn is_active_for_gossip(&self, now: chrono::DateTime<Utc>) -> bool {
        if let Some(last_seen) = self.last_seen() {
            // Correctness: `last_seen` shouldn't ever be in the future, either because we set the
            // time or because another peer's future time was sanitized when it was added to the
            // address book
            last_seen.saturating_elapsed(now) <= constants::MAX_PEER_ACTIVE_FOR_GOSSIP
        } else {
            // Peer has never responded and does not have a gossiped last seen time
            false
        }
    }

    /// Returns true if any messages were recently sent to or received from this address.
    pub fn was_recently_updated(
        &self,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> bool {
        self.has_connection_recently_responded(chrono_now)
            || self.was_connection_recently_attempted(instant_now)
            || self.has_connection_recently_failed(instant_now)
    }

    /// Is this address ready for a new outbound connection attempt?
    pub fn is_ready_for_connection_attempt(
        &self,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
        network: &Network,
    ) -> bool {
        self.last_known_info_is_valid_for_outbound(network)
            && !self.was_recently_updated(instant_now, chrono_now)
            && self.is_probably_reachable(chrono_now)
    }

    /// Is the [`PeerSocketAddr`] we have for this peer valid for outbound
    /// connections?
    ///
    /// Since the addresses in the address book are unique, this check can be
    /// used to permanently reject entire [`MetaAddr`]s.
    pub fn address_is_valid_for_outbound(&self, network: &Network) -> bool {
        address_is_valid_for_outbound_connections(self.addr, network.clone()).is_ok()
    }

    /// Is the last known information for this peer valid for outbound
    /// connections?
    ///
    /// The last known info might be outdated or untrusted, so this check can
    /// only be used to:
    /// - reject `NeverAttempted...` [`MetaAddrChange`]s, and
    /// - temporarily stop outbound connections to a [`MetaAddr`].
    pub fn last_known_info_is_valid_for_outbound(&self, network: &Network) -> bool {
        let is_node = match self.services {
            Some(services) => services.contains(PeerServices::NODE_NETWORK),
            None => true,
        };

        is_node && self.address_is_valid_for_outbound(network)
    }

    /// Should this peer considered reachable?
    ///
    /// A peer is probably reachable if:
    /// - it has never been attempted, or
    /// - the last connection attempt was successful, or
    /// - the last successful connection was less than 3 days ago.
    ///
    /// # Security
    ///
    /// This is used by [`Self::is_ready_for_connection_attempt`] so that Zebra stops trying to
    /// connect to peers that are likely unreachable.
    ///
    /// The `untrusted_last_seen` time is used as a fallback time if the local node has never
    /// itself seen the peer. If the reported last seen time is a long time ago or `None`, then the local
    /// node will attempt to connect the peer once, and if that attempt fails it won't
    /// try to connect ever again. (The state can't be `Failed` until after the first connection attempt.)
    pub fn is_probably_reachable(&self, now: chrono::DateTime<Utc>) -> bool {
        self.last_connection_state != PeerAddrState::Failed || self.last_seen_is_recent(now)
    }

    /// Was this peer last seen recently?
    ///
    /// Returns `true` if this peer was last seen at most
    /// [`MAX_RECENT_PEER_AGE`][constants::MAX_RECENT_PEER_AGE] ago.
    /// Returns false if the peer is outdated, or it has no last seen time.
    pub fn last_seen_is_recent(&self, now: chrono::DateTime<Utc>) -> bool {
        match self.last_seen() {
            Some(last_seen) => last_seen.saturating_elapsed(now) <= constants::MAX_RECENT_PEER_AGE,
            None => false,
        }
    }

    /// Returns a score of misbehavior encountered in a peer at this address.
    pub fn misbehavior(&self) -> u32 {
        self.misbehavior_score
    }

    /// Return a sanitized version of this `MetaAddr`, for sending to a remote peer.
    ///
    /// Returns `None` if this `MetaAddr` should not be sent to remote peers.
    #[allow(clippy::unwrap_in_result)]
    pub fn sanitize(&self, network: &Network) -> Option<MetaAddr> {
        if !self.last_known_info_is_valid_for_outbound(network) {
            return None;
        }

        // Avoid responding to GetAddr requests with addresses of misbehaving peers.
        if self.misbehavior_score != 0 || self.is_inbound {
            return None;
        }

        // Sanitize time
        let last_seen = self.last_seen()?;
        let remainder = last_seen
            .timestamp()
            .rem_euclid(crate::constants::TIMESTAMP_TRUNCATION_SECONDS);
        let last_seen = last_seen
            .checked_sub(remainder.into())
            .expect("unexpected underflow: rem_euclid is strictly less than timestamp");

        Some(MetaAddr {
            addr: canonical_peer_addr(self.addr),
            // initial peers are sanitized assuming they are `NODE_NETWORK`
            // TODO: split untrusted and direct services
            //       consider sanitizing untrusted services to NODE_NETWORK (#2324)
            services: self.services.or(Some(PeerServices::NODE_NETWORK)),
            // only put the last seen time in the untrusted field,
            // this matches deserialization, and avoids leaking internal state
            untrusted_last_seen: Some(last_seen),
            last_response: None,
            // these fields aren't sent to the remote peer, but sanitize them anyway
            last_attempt: None,
            last_failure: None,
            last_connection_state: NeverAttemptedGossiped,
            misbehavior_score: 0,
            is_inbound: false,
        })
    }
}

#[cfg(test)]
impl MetaAddr {
    /// Forcefully change the time this peer last responded.
    ///
    /// This method is for testing purposes only.
    pub(crate) fn set_last_response(&mut self, last_response: DateTime32) {
        self.last_response = Some(last_response);
    }
}

impl MetaAddrChange {
    /// Return the address for this change.
    pub fn addr(&self) -> PeerSocketAddr {
        match self {
            NewInitial { addr }
            | NewGossiped { addr, .. }
            | NewLocal { addr, .. }
            | UpdateAttempt { addr }
            | UpdateConnected { addr, .. }
            | UpdateResponded { addr, .. }
            | UpdateFailed { addr, .. }
            | UpdateMisbehavior { addr, .. } => *addr,
        }
    }

    #[cfg(any(test, feature = "proptest-impl"))]
    /// Set the address for this change to `new_addr`.
    ///
    /// This method should only be used in tests.
    pub fn set_addr(&mut self, new_addr: PeerSocketAddr) {
        match self {
            NewInitial { addr }
            | NewGossiped { addr, .. }
            | NewLocal { addr, .. }
            | UpdateAttempt { addr }
            | UpdateConnected { addr, .. }
            | UpdateResponded { addr, .. }
            | UpdateFailed { addr, .. }
            | UpdateMisbehavior { addr, .. } => *addr = new_addr,
        }
    }

    /// Return the untrusted services for this change, if available.
    pub fn untrusted_services(&self) -> Option<PeerServices> {
        match self {
            NewInitial { .. } => None,
            // TODO: split untrusted and direct services (#2324)
            NewGossiped {
                untrusted_services, ..
            } => Some(*untrusted_services),
            // TODO: create a "services implemented by Zebra" constant (#2324)
            NewLocal { .. } => Some(PeerServices::NODE_NETWORK),
            UpdateAttempt { .. } => None,
            UpdateConnected { services, .. } => Some(*services),
            UpdateResponded { .. } => None,
            UpdateFailed { services, .. } => *services,
            UpdateMisbehavior { .. } => None,
        }
    }

    /// Return the untrusted last seen time for this change, if available.
    pub fn untrusted_last_seen(&self, now: DateTime32) -> Option<DateTime32> {
        match self {
            NewInitial { .. } => None,
            NewGossiped {
                untrusted_last_seen,
                ..
            } => Some(*untrusted_last_seen),
            // We know that our local listener is available
            NewLocal { .. } => Some(now),
            UpdateAttempt { .. }
            | UpdateConnected { .. }
            | UpdateResponded { .. }
            | UpdateFailed { .. }
            | UpdateMisbehavior { .. } => None,
        }
    }

    // # Concurrency
    //
    // We assign a time to each change when it is applied to the address book by either the
    // address book updater or candidate set tasks. This is the time that the change was received
    // from the updater channel, rather than the time that the message was read from the peer
    // connection.
    //
    // Since the connection tasks run concurrently in an unspecified order, and the address book
    // updater runs in a separate thread, these times are almost always very similar. If Zebra's
    // address book is under load, we should use lower rate-limits for new inbound or outbound
    // connections, disconnections, peer gossip crawls, or peer `UpdateResponded` updates.
    //
    // TODO:
    // - move the time API calls from `impl MetaAddrChange` `last_*()` methods:
    //   - if they impact performance, call them once in the address book updater task,
    //     then apply them to all the waiting changes
    //   - otherwise, move them to the `impl MetaAddrChange` `new_*()` methods,
    //     so they are called in the connection tasks
    //
    /// Return the last attempt for this change, if available.
    pub fn last_attempt(&self, now: Instant) -> Option<Instant> {
        match self {
            NewInitial { .. } | NewGossiped { .. } | NewLocal { .. } => None,
            // Attempt changes are applied before we start the handshake to the
            // peer address. So the attempt time is a lower bound for the actual
            // handshake time.
            UpdateAttempt { .. } => Some(now),
            UpdateConnected { .. }
            | UpdateResponded { .. }
            | UpdateFailed { .. }
            | UpdateMisbehavior { .. } => None,
        }
    }

    /// Return the last response for this change, if available.
    pub fn last_response(&self, now: DateTime32) -> Option<DateTime32> {
        match self {
            NewInitial { .. } | NewGossiped { .. } | NewLocal { .. } | UpdateAttempt { .. } => None,
            // If there is a large delay applying this change, then:
            // - the peer might stay in the `AttemptPending` state for longer,
            // - we might send outdated last seen times to our peers, and
            // - the peer will appear to be live for longer, delaying future
            //   reconnection attempts.
            UpdateConnected { .. } | UpdateResponded { .. } => Some(now),
            UpdateFailed { .. } | UpdateMisbehavior { .. } => None,
        }
    }

    /// Return the last failure for this change, if available.
    pub fn last_failure(&self, now: Instant) -> Option<Instant> {
        match self {
            NewInitial { .. }
            | NewGossiped { .. }
            | NewLocal { .. }
            | UpdateAttempt { .. }
            | UpdateConnected { .. }
            | UpdateResponded { .. } => None,
            // If there is a large delay applying this change, then:
            // - the peer might stay in the `AttemptPending` or `Responded`
            //   states for longer, and
            // - the peer will appear to be used for longer, delaying future
            //   reconnection attempts.
            UpdateFailed { .. } | UpdateMisbehavior { .. } => Some(now),
        }
    }

    /// Return the peer connection state for this change.
    pub fn peer_addr_state(&self) -> PeerAddrState {
        match self {
            NewInitial { .. } => NeverAttemptedGossiped,
            NewGossiped { .. } => NeverAttemptedGossiped,
            // local listeners get sanitized, so the state doesn't matter here
            NewLocal { .. } => NeverAttemptedGossiped,
            UpdateAttempt { .. } => AttemptPending,
            UpdateConnected { .. } | UpdateResponded { .. } | UpdateMisbehavior { .. } => Responded,
            UpdateFailed { .. } => Failed,
        }
    }

    /// Returns the corresponding `MetaAddr` for this change.
    pub fn into_new_meta_addr(self, instant_now: Instant, local_now: DateTime32) -> MetaAddr {
        MetaAddr {
            addr: self.addr(),
            services: self.untrusted_services(),
            untrusted_last_seen: self.untrusted_last_seen(local_now),
            last_response: self.last_response(local_now),
            last_attempt: self.last_attempt(instant_now),
            last_failure: self.last_failure(instant_now),
            last_connection_state: self.peer_addr_state(),
            misbehavior_score: self.misbehavior_score(),
            is_inbound: self.is_inbound(),
        }
    }

    /// Returns the misbehavior score increment for the current change.
    pub fn misbehavior_score(&self) -> u32 {
        match self {
            MetaAddrChange::UpdateMisbehavior {
                score_increment, ..
            } => *score_increment,
            _ => 0,
        }
    }

    /// Returns whether this change was created for a new inbound connection.
    pub fn is_inbound(&self) -> bool {
        if let MetaAddrChange::UpdateConnected { is_inbound, .. } = self {
            *is_inbound
        } else {
            false
        }
    }

    /// Returns the corresponding [`MetaAddr`] for a local listener change.
    ///
    /// This method exists so we don't have to provide an unused [`Instant`] to get a local
    /// listener `MetaAddr`.
    ///
    /// # Panics
    ///
    /// If this change is not a [`MetaAddrChange::NewLocal`].
    pub fn local_listener_into_new_meta_addr(self, local_now: DateTime32) -> MetaAddr {
        assert!(matches!(self, MetaAddrChange::NewLocal { .. }));

        MetaAddr {
            addr: self.addr(),
            services: self.untrusted_services(),
            untrusted_last_seen: self.untrusted_last_seen(local_now),
            last_response: self.last_response(local_now),
            last_attempt: None,
            last_failure: None,
            last_connection_state: self.peer_addr_state(),
            misbehavior_score: self.misbehavior_score(),
            is_inbound: self.is_inbound(),
        }
    }

    /// Apply this change to a previous `MetaAddr` from the address book,
    /// producing a new or updated `MetaAddr`.
    ///
    /// If the change isn't valid for the `previous` address, returns `None`.
    #[allow(clippy::unwrap_in_result)]
    pub fn apply_to_meta_addr(
        &self,
        previous: impl Into<Option<MetaAddr>>,
        instant_now: Instant,
        chrono_now: chrono::DateTime<Utc>,
    ) -> Option<MetaAddr> {
        let local_now: DateTime32 = chrono_now.try_into().expect("will succeed until 2038");

        let Some(previous) = previous.into() else {
            // no previous: create a new entry
            return Some(self.into_new_meta_addr(instant_now, local_now));
        };

        assert_eq!(previous.addr, self.addr(), "unexpected addr mismatch");

        let instant_previous = max(previous.last_attempt, previous.last_failure);
        let local_previous = previous.last_response;

        // Is this change potentially concurrent with the previous change?
        //
        // Since we're using saturating arithmetic, one of each pair of less than comparisons
        // will always be true, because subtraction saturates to zero.
        let change_is_concurrent = instant_previous
            .map(|instant_previous| {
                instant_previous.saturating_duration_since(instant_now)
                    < constants::CONCURRENT_ADDRESS_CHANGE_PERIOD
                    && instant_now.saturating_duration_since(instant_previous)
                        < constants::CONCURRENT_ADDRESS_CHANGE_PERIOD
            })
            .unwrap_or_default()
            || local_previous
                .map(|local_previous| {
                    local_previous.saturating_duration_since(local_now).to_std()
                        < constants::CONCURRENT_ADDRESS_CHANGE_PERIOD
                        && local_now.saturating_duration_since(local_previous).to_std()
                            < constants::CONCURRENT_ADDRESS_CHANGE_PERIOD
                })
                .unwrap_or_default();
        let change_is_out_of_order = instant_previous
            .map(|instant_previous| instant_previous > instant_now)
            .unwrap_or_default()
            || local_previous
                .map(|local_previous| local_previous > local_now)
                .unwrap_or_default();

        // Is this change typically from a connection state that has more progress?
        let connection_has_more_progress = self
            .peer_addr_state()
            .connection_state_order(&previous.last_connection_state)
            == Ordering::Greater;

        let previous_has_been_attempted = !previous.last_connection_state.is_never_attempted();
        let change_to_never_attempted = self.peer_addr_state().is_never_attempted();
        let is_misbehavior_update = self.misbehavior_score() != 0;

        // Invalid changes

        if change_to_never_attempted && previous_has_been_attempted && !is_misbehavior_update {
            // Existing entry has been attempted, change is NeverAttempted
            // - ignore the change
            //
            // # Security
            //
            // Ignore NeverAttempted changes once we have made an attempt,
            // so malicious peers can't keep changing our peer connection order.
            return None;
        }

        if change_is_out_of_order && !change_is_concurrent && !is_misbehavior_update {
            // Change is significantly out of order: ignore it.
            //
            // # Security
            //
            // Ignore changes that arrive out of order, if they are far enough apart.
            // This enforces the peer connection retry interval.
            return None;
        }

        if change_is_concurrent && !connection_has_more_progress && !is_misbehavior_update {
            // Change is close together in time, and it would revert the connection to an earlier
            // state.
            //
            // # Security
            //
            // If the changes might have been concurrent, ignore connection states with less
            // progress.
            //
            // ## Sources of Concurrency
            //
            // If two changes happen close together, the async scheduler can run their change
            // send and apply code in any order. This includes the code that records the time of
            // the change. So even if a failure happens after a response message, the failure time
            // can be recorded before the response time code is run.
            //
            // Some machines and OSes have limited time resolution, so we can't guarantee that
            // two messages on the same connection will always have different times. There are
            // also known bugs impacting monotonic times which make them go backwards or stay
            // equal. For wall clock times, clock skew is an expected event, particularly with
            // network time server updates.
            //
            // Also, the application can fail a connection independently and simultaneously
            // (or slightly before) a positive update from that peer connection. We want the
            // application change to take priority in the address book, because the connection
            // state machine also prioritises failures over any other peer messages.
            //
            // ## Resolution
            //
            // In these cases, we want to apply the failure, then ignore any nearby changes that
            // reset the address book entry to a more appealing state. This prevents peers from
            // sending updates right before failing a connection, in order to make themselves more
            // likely to get a reconnection.
            //
            // The connection state machine order is used so that state transitions which are
            // typically close together are preserved. These transitions are:
            // - NeverAttempted*->AttemptPending->(Responded|Failed)
            // - Responded->Failed
            //
            // State transitions like (Responded|Failed)->AttemptPending only happen after the
            // reconnection timeout, so they will never be considered concurrent.
            return None;
        }

        // Valid changes

        if change_to_never_attempted && !previous_has_been_attempted {
            // Existing entry and change are both NeverAttempted
            // - preserve original values of all fields
            // - but replace None with Some
            //
            // # Security
            //
            // Preserve the original field values for NeverAttempted peers,
            // so malicious peers can't keep changing our peer connection order.
            Some(MetaAddr {
                addr: self.addr(),
                services: previous.services.or_else(|| self.untrusted_services()),
                untrusted_last_seen: previous
                    .untrusted_last_seen
                    .or_else(|| self.untrusted_last_seen(local_now)),
                // The peer has not been attempted, so these fields must be None
                last_response: None,
                last_attempt: None,
                last_failure: None,
                last_connection_state: self.peer_addr_state(),
                misbehavior_score: previous.misbehavior_score + self.misbehavior_score(),
                is_inbound: previous.is_inbound || self.is_inbound(),
            })
        } else {
            // Existing entry and change are both Attempt, Responded, Failed,
            // and the change is later, either in time or in connection progress
            // (this is checked above and returns None early):
            // - update the fields from the change
            Some(MetaAddr {
                addr: self.addr(),
                // Always update optional fields, unless the update is None.
                //
                // We want up-to-date services, even if they have fewer bits
                services: self.untrusted_services().or(previous.services),
                // Only NeverAttempted changes can modify the last seen field
                untrusted_last_seen: previous.untrusted_last_seen,
                // This is a wall clock time, but we already checked that responses are in order.
                // Even if the wall clock time has jumped, we want to use the latest time.
                last_response: self.last_response(local_now).or(previous.last_response),
                // These are monotonic times, we already checked the responses are in order.
                last_attempt: self.last_attempt(instant_now).or(previous.last_attempt),
                last_failure: self.last_failure(instant_now).or(previous.last_failure),
                // Replace the state with the updated state.
                last_connection_state: self.peer_addr_state(),
                misbehavior_score: previous.misbehavior_score + self.misbehavior_score(),
                is_inbound: previous.is_inbound || self.is_inbound(),
            })
        }
    }
}

impl Ord for MetaAddr {
    /// `MetaAddr`s are sorted in approximate reconnection attempt order, but
    /// with `Responded` peers sorted first as a group.
    ///
    /// But this order should not be used for reconnection attempts: use
    /// [`reconnection_peers`] instead.
    ///
    /// See [`CandidateSet`] for more details.
    ///
    /// [`CandidateSet`]: super::peer_set::CandidateSet
    /// [`reconnection_peers`]: crate::AddressBook::reconnection_peers
    fn cmp(&self, other: &Self) -> Ordering {
        use std::net::IpAddr::{V4, V6};
        use Ordering::*;

        // First, try states that are more likely to work
        let more_reliable_state = self.last_connection_state.cmp(&other.last_connection_state);

        // Then, try addresses that are more likely to be valid.
        // Currently, this prefers addresses with canonical Zcash ports.
        let more_likely_valid = self.peer_preference().cmp(&other.peer_preference());

        // # Security and Correctness
        //
        // Prioritise older attempt times, so we try all peers in each state,
        // before re-trying any of them. This avoids repeatedly reconnecting to
        // peers that aren't working.
        //
        // Using the internal attempt time for peer ordering also minimises the
        // amount of information `Addrs` responses leak about Zebra's retry order.

        // If the states are the same, try peers that we haven't tried for a while.
        //
        // Each state change updates a specific time field, and
        // None is less than Some(T),
        // so the resulting ordering for each state is:
        // - Responded: oldest attempts first (attempt times are required and unique)
        // - NeverAttempted...: recent gossiped times first (all other times are None)
        // - Failed: oldest attempts first (attempt times are required and unique)
        // - AttemptPending: oldest attempts first (attempt times are required and unique)
        //
        // We also compare the other local times, because:
        // - seed peers may not have an attempt time, and
        // - updates can be applied to the address book in any order.
        let older_attempt = self.last_attempt.cmp(&other.last_attempt);
        let older_failure = self.last_failure.cmp(&other.last_failure);
        let older_response = self.last_response.cmp(&other.last_response);

        // # Security
        //
        // Compare local times before untrusted gossiped times and services.
        // This gives malicious peers less influence over our peer connection
        // order.

        // If all local times are None, try peers that other peers have seen more recently
        let newer_untrusted_last_seen = self
            .untrusted_last_seen
            .cmp(&other.untrusted_last_seen)
            .reverse();

        // Finally, prefer numerically larger service bit patterns
        //
        // As of June 2021, Zebra only recognises the NODE_NETWORK bit.
        // When making outbound connections, Zebra skips non-nodes.
        // So this comparison will have no impact until Zebra implements
        // more service features.
        //
        // None is less than Some(T), so peers with missing services are chosen last.
        //
        // TODO: order services by usefulness, not bit pattern values (#2324)
        //       Security: split gossiped and direct services
        let larger_services = self.services.cmp(&other.services);

        // The remaining comparisons are meaningless for peer connection priority.
        // But they are required so that we have a total order on `MetaAddr` values:
        // self and other must compare as Equal iff they are equal.

        // As a tie-breaker, compare ip and port numerically
        //
        // Since SocketAddrs are unique in the address book, these comparisons
        // guarantee a total, unique order.
        let ip_tie_breaker = match (self.addr.ip(), other.addr.ip()) {
            (V4(a), V4(b)) => a.octets().cmp(&b.octets()),
            (V6(a), V6(b)) => a.octets().cmp(&b.octets()),
            (V4(_), V6(_)) => Less,
            (V6(_), V4(_)) => Greater,
        };
        let port_tie_breaker = self.addr.port().cmp(&other.addr.port());

        more_reliable_state
            .then(more_likely_valid)
            .then(older_attempt)
            .then(older_failure)
            .then(older_response)
            .then(newer_untrusted_last_seen)
            .then(larger_services)
            .then(ip_tie_breaker)
            .then(port_tie_breaker)
    }
}

impl PartialOrd for MetaAddr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for MetaAddr {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for MetaAddr {}
