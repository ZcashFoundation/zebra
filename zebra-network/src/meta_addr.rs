//! An address-with-metadata type used in Bitcoin networking.

use std::{
    cmp::{Ord, Ordering},
    io::{Read, Write},
    net::SocketAddr,
    time::Instant,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use chrono::Duration;
use zebra_chain::serialization::{
    DateTime32, ReadZcashExt, SerializationError, TrustedPreallocate, WriteZcashExt,
    ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
};

use crate::protocol::{external::MAX_PROTOCOL_MESSAGE_LEN, types::PeerServices};

use MetaAddrChange::*;
use PeerAddrState::*;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
#[cfg(any(test, feature = "proptest-impl"))]
use zebra_chain::serialization::arbitrary::canonical_socket_addr;
#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;

#[cfg(test)]
mod tests;

/// Peer connection state, based on our interactions with the peer.
///
/// Zebra also tracks how recently a peer has sent us messages, and derives peer
/// liveness based on the current time. This derived state is tracked using
/// [`AddressBook::maybe_connected_peers`] and
/// [`AddressBook::reconnection_peers`].
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
    /// gossip, but we haven't attempted to connect to it yet.
    NeverAttemptedGossiped,

    /// The peer's address has just been received as part of a `Version` message,
    /// so we might already be connected to this peer.
    ///
    /// Alternate addresses are attempted after gossiped addresses.
    NeverAttemptedAlternate,

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
            NeverAttemptedGossiped | NeverAttemptedAlternate => true,
            AttemptPending | Responded | Failed => false,
        }
    }
}

// non-test code should explicitly specify the peer address state
#[cfg(test)]
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
    fn cmp(&self, other: &Self) -> Ordering {
        use Ordering::*;
        match (self, other) {
            (Responded, Responded)
            | (Failed, Failed)
            | (NeverAttemptedGossiped, NeverAttemptedGossiped)
            | (NeverAttemptedAlternate, NeverAttemptedAlternate)
            | (AttemptPending, AttemptPending) => Equal,
            // We reconnect to `Responded` peers that have stopped sending messages,
            // then `NeverAttempted` peers, then `Failed` peers
            (Responded, _) => Less,
            (_, Responded) => Greater,
            (NeverAttemptedGossiped, _) => Less,
            (_, NeverAttemptedGossiped) => Greater,
            (NeverAttemptedAlternate, _) => Less,
            (_, NeverAttemptedAlternate) => Greater,
            (Failed, _) => Less,
            (_, Failed) => Greater,
            // AttemptPending is covered by the other cases
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
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct MetaAddr {
    /// The peer's address.
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "canonical_socket_addr()")
    )]
    pub addr: SocketAddr,

    /// The services advertised by the peer.
    ///
    /// The exact meaning depends on `last_connection_state`:
    ///   - `Responded`: the services advertised by this peer, the last time we
    ///      performed a handshake with it
    ///   - `NeverAttempted`: the unverified services provided by the remote peer
    ///     that sent us this address
    ///   - `Failed` or `AttemptPending`: unverified services via another peer,
    ///      or services advertised in a previous handshake
    ///
    /// ## Security
    ///
    /// `services` from `NeverAttempted` peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    //
    // TODO: make services private and optional
    //       split gossiped and handshake services?
    pub services: PeerServices,

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

    /// The outcome of our most recent communication attempt with this peer.
    pub last_connection_state: PeerAddrState,
    //
    // TODO: move the time and services fields into PeerAddrState?
    //       then some fields could be required in some states
}

/// A change to an existing `MetaAddr`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum MetaAddrChange {
    /// Creates a new gossiped `MetaAddr`.
    NewGossiped {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
        untrusted_services: PeerServices,
        untrusted_last_seen: DateTime32,
    },

    /// Creates new alternate `MetaAddr`.
    ///
    /// Based on the canonical peer address in `Version` messages.
    NewAlternate {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
        untrusted_services: PeerServices,
    },

    /// Creates new local listener `MetaAddr`.
    NewLocal {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
    },

    /// Updates an existing `MetaAddr` when an outbound connection attempt
    /// starts.
    UpdateAttempt {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
    },

    /// Updates an existing `MetaAddr` when a peer responds with a message.
    UpdateResponded {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
        services: PeerServices,
    },

    /// Updates an existing `MetaAddr` when a peer fails.
    UpdateFailed {
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "canonical_socket_addr()")
        )]
        addr: SocketAddr,
        services: Option<PeerServices>,
    },
}

// TODO: remove this use in a follow-up PR
use chrono::{DateTime, Utc};

impl MetaAddr {
    /// Returns the maximum time among all the time fields.
    ///
    /// This function exists to replicate an old Zebra bug.
    /// TODO: remove this function in a follow-up PR
    pub(crate) fn get_last_seen(&self) -> DateTime<Utc> {
        let latest_seen = self
            .untrusted_last_seen
            .max(self.last_response)
            .map(DateTime32::to_chrono);

        // At this point it's pretty obvious why we want to get rid of this code
        let latest_try = self
            .last_attempt
            .max(self.last_failure)
            .map(|latest_try| Instant::now().checked_duration_since(latest_try))
            .flatten()
            .map(Duration::from_std)
            .map(Result::ok)
            .flatten()
            .map(|before_now| Utc::now() - before_now);

        latest_seen.or(latest_try).unwrap_or(chrono::MIN_DATETIME)
    }

    /// Returns a new `MetaAddr`, based on the deserialized fields from a
    /// gossiped peer [`Addr`][crate::protocol::external::Message::Addr] message.
    pub fn new_gossiped_meta_addr(
        addr: SocketAddr,
        untrusted_services: PeerServices,
        untrusted_last_seen: DateTime32,
    ) -> MetaAddr {
        MetaAddr {
            addr,
            services: untrusted_services,
            untrusted_last_seen: Some(untrusted_last_seen),
            last_response: None,
            last_attempt: None,
            last_failure: None,
            last_connection_state: NeverAttemptedGossiped,
        }
    }

    /// Returns a [`MetaAddrChange::NewGossiped`], based on a gossiped peer
    /// `MetaAddr`.
    pub fn new_gossiped_change(self) -> MetaAddrChange {
        NewGossiped {
            addr: self.addr,
            untrusted_services: self.services,
            untrusted_last_seen: self
                .untrusted_last_seen
                .expect("unexpected missing last seen"),
        }
    }

    /// Returns a [`MetaAddrChange::UpdateResponded`] for a peer that has just
    /// sent us a message.
    ///
    /// # Security
    ///
    /// This address must be the remote address from an outbound connection,
    /// and the services must be the services from that peer's handshake.
    ///
    /// Otherwise:
    /// - malicious peers could interfere with other peers' `AddressBook` state,
    ///   or
    /// - Zebra could advertise unreachable addresses to its own peers.
    pub fn new_responded(addr: &SocketAddr, services: &PeerServices) -> MetaAddrChange {
        UpdateResponded {
            addr: *addr,
            services: *services,
        }
    }

    /// Returns a [`MetaAddrChange::UpdateConnectionAttempt`] for a peer that we
    /// want to make an outbound connection to.
    pub fn new_reconnect(addr: &SocketAddr) -> MetaAddrChange {
        UpdateAttempt { addr: *addr }
    }

    /// Returns a [`MetaAddrChange::NewAlternate`] for a peer's alternate address,
    /// received via a `Version` message.
    pub fn new_alternate(addr: &SocketAddr, untrusted_services: &PeerServices) -> MetaAddrChange {
        NewAlternate {
            addr: *addr,
            untrusted_services: *untrusted_services,
        }
    }

    /// Returns a [`MetaAddrChange::NewLocalListener`] for our own listener address.
    pub fn new_local_listener(addr: &SocketAddr) -> MetaAddrChange {
        NewLocal { addr: *addr }
    }

    /// Returns a [`MetaAddrChange::UpdateFailed`] for a peer that has just had
    /// an error.
    pub fn new_errored(
        addr: &SocketAddr,
        services: impl Into<Option<PeerServices>>,
    ) -> MetaAddrChange {
        UpdateFailed {
            addr: *addr,
            services: services.into(),
        }
    }

    /// Create a new `MetaAddr` for a peer that has just shut down.
    pub fn new_shutdown(
        addr: &SocketAddr,
        services: impl Into<Option<PeerServices>>,
    ) -> MetaAddrChange {
        // TODO: if the peer shut down in the Responded state, preserve that
        // state. All other states should be treated as (timeout) errors.
        MetaAddr::new_errored(addr, services.into())
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

    /// Is this address a directly connected client?
    pub fn is_direct_client(&self) -> bool {
        match self.last_connection_state {
            Responded => !self.services.contains(PeerServices::NODE_NETWORK),
            NeverAttemptedGossiped | NeverAttemptedAlternate | Failed | AttemptPending => false,
        }
    }

    /// Is this address valid for outbound connections?
    pub fn is_valid_for_outbound(&self) -> bool {
        self.services.contains(PeerServices::NODE_NETWORK)
            && !self.addr.ip().is_unspecified()
            && self.addr.port() != 0
    }

    /// Return a sanitized version of this `MetaAddr`, for sending to a remote peer.
    ///
    /// Returns `None` if this `MetaAddr` should not be sent to remote peers.
    pub fn sanitize(&self) -> Option<MetaAddr> {
        let interval = crate::constants::TIMESTAMP_TRUNCATION_SECONDS;
        let ts = self.last_seen()?.timestamp();
        // This can't underflow, because `0 <= rem_euclid < ts`
        let last_seen = ts - ts.rem_euclid(interval);
        let last_seen = DateTime32::from(last_seen);
        Some(MetaAddr {
            addr: self.addr,
            // deserialization also sanitizes services to known flags
            services: self.services & PeerServices::all(),
            // only put the last seen time in the untrusted field,
            // this matches deserialization, and avoids leaking internal state
            untrusted_last_seen: Some(last_seen),
            last_response: None,
            // these fields aren't sent to the remote peer, but sanitize them anyway
            last_attempt: None,
            last_failure: None,
            last_connection_state: NeverAttemptedGossiped,
        })
    }
}

impl MetaAddrChange {
    /// Return the address for this change.
    pub fn addr(&self) -> SocketAddr {
        match self {
            NewGossiped { addr, .. }
            | NewAlternate { addr, .. }
            | NewLocal { addr, .. }
            | UpdateAttempt { addr }
            | UpdateResponded { addr, .. }
            | UpdateFailed { addr, .. } => *addr,
        }
    }

    #[cfg(any(test, feature = "proptest-impl"))]
    /// Set the address for this change to `new_addr`.
    ///
    /// This method should only be used in tests.
    pub fn set_addr(&mut self, new_addr: SocketAddr) {
        match self {
            NewGossiped { addr, .. }
            | NewAlternate { addr, .. }
            | NewLocal { addr, .. }
            | UpdateAttempt { addr }
            | UpdateResponded { addr, .. }
            | UpdateFailed { addr, .. } => *addr = new_addr,
        }
    }

    /// Return the untrusted services for this change, if available.
    pub fn untrusted_services(&self) -> Option<PeerServices> {
        match self {
            NewGossiped {
                untrusted_services, ..
            } => Some(*untrusted_services),
            NewAlternate {
                untrusted_services, ..
            } => Some(*untrusted_services),
            // TODO: create a "services implemented by Zebra" constant
            NewLocal { .. } => Some(PeerServices::NODE_NETWORK),
            UpdateAttempt { .. } => None,
            UpdateResponded { services, .. } => Some(*services),
            UpdateFailed { services, .. } => *services,
        }
    }

    /// Return the untrusted last seen time for this change, if available.
    pub fn untrusted_last_seen(&self) -> Option<DateTime32> {
        match self {
            NewGossiped {
                untrusted_last_seen,
                ..
            } => Some(*untrusted_last_seen),
            NewAlternate { .. } => None,
            NewLocal { .. } => None,
            UpdateAttempt { .. } => None,
            UpdateResponded { .. } => None,
            UpdateFailed { .. } => None,
        }
    }

    /// Return the last attempt for this change, if available.
    pub fn last_attempt(&self) -> Option<Instant> {
        match self {
            NewGossiped { .. } => None,
            NewAlternate { .. } => None,
            NewLocal { .. } => None,
            UpdateAttempt { .. } => Some(Instant::now()),
            UpdateResponded { .. } => None,
            UpdateFailed { .. } => None,
        }
    }

    /// Return the last response for this change, if available.
    pub fn last_response(&self) -> Option<DateTime32> {
        match self {
            NewGossiped { .. } => None,
            NewAlternate { .. } => None,
            NewLocal { .. } => None,
            UpdateAttempt { .. } => None,
            UpdateResponded { .. } => Some(DateTime32::now()),
            UpdateFailed { .. } => None,
        }
    }

    /// Return the last attempt for this change, if available.
    pub fn last_failure(&self) -> Option<Instant> {
        match self {
            NewGossiped { .. } => None,
            NewAlternate { .. } => None,
            NewLocal { .. } => None,
            UpdateAttempt { .. } => None,
            UpdateResponded { .. } => None,
            UpdateFailed { .. } => Some(Instant::now()),
        }
    }

    /// Return the peer connection state for this change.
    pub fn peer_addr_state(&self) -> PeerAddrState {
        match self {
            NewGossiped { .. } => NeverAttemptedGossiped,
            NewAlternate { .. } => NeverAttemptedAlternate,
            // local listeners get sanitized, so the exact value doesn't matter
            NewLocal { .. } => NeverAttemptedGossiped,
            UpdateAttempt { .. } => AttemptPending,
            UpdateResponded { .. } => Responded,
            UpdateFailed { .. } => Failed,
        }
    }

    /// If this change can create a new `MetaAddr`, return that address.
    pub fn into_new_meta_addr(self) -> Option<MetaAddr> {
        match self {
            NewGossiped { .. } | NewAlternate { .. } | NewLocal { .. } => Some(MetaAddr {
                addr: self.addr(),
                // TODO: make services optional when we add a DNS seeder change/state
                services: self
                    .untrusted_services()
                    .expect("unexpected missing services"),
                untrusted_last_seen: self.untrusted_last_seen(),
                last_response: None,
                last_attempt: None,
                last_failure: None,
                last_connection_state: self.peer_addr_state(),
            }),
            UpdateAttempt { .. } | UpdateResponded { .. } | UpdateFailed { .. } => None,
        }
    }

    /// Apply this change to a previous `MetaAddr` from the address book,
    /// producing a new or updated `MetaAddr`.
    ///
    /// If the change isn't valid for the `previous` address, returns `None`.
    pub fn apply_to_meta_addr(&self, previous: impl Into<Option<MetaAddr>>) -> Option<MetaAddr> {
        if let Some(previous) = previous.into() {
            assert_eq!(previous.addr, self.addr(), "unexpected addr mismatch");

            let previous_has_been_attempted = !previous.last_connection_state.is_never_attempted();
            let change_to_never_attempted = self
                .into_new_meta_addr()
                .map(|meta_addr| meta_addr.last_connection_state.is_never_attempted())
                .unwrap_or(false);

            if change_to_never_attempted {
                if previous_has_been_attempted {
                    // Security: ignore never attempted changes once we have made an attempt
                    None
                } else {
                    // never attempted to never attempted update: preserve original values
                    Some(MetaAddr {
                        addr: self.addr(),
                        // TODO: or(self.untrusted_services()) when services become optional
                        services: previous.services,
                        // Security: only update the last seen time if it is missing
                        untrusted_last_seen: previous
                            .untrusted_last_seen
                            .or_else(|| self.untrusted_last_seen()),
                        last_response: None,
                        last_attempt: None,
                        last_failure: None,
                        last_connection_state: self.peer_addr_state(),
                    })
                }
            } else {
                // any to attempt, responded, or failed: prefer newer values
                Some(MetaAddr {
                    addr: self.addr(),
                    services: self.untrusted_services().unwrap_or(previous.services),
                    // we don't modify the last seen field at all
                    untrusted_last_seen: previous.untrusted_last_seen,
                    last_response: self.last_response().or(previous.last_response),
                    last_attempt: self.last_attempt().or(previous.last_attempt),
                    last_failure: self.last_failure().or(previous.last_failure),
                    last_connection_state: self.peer_addr_state(),
                })
            }
        } else {
            // no previous: create a new entry
            self.into_new_meta_addr()
        }
    }
}

impl Ord for MetaAddr {
    /// `MetaAddr`s are sorted in approximate reconnection attempt order, but
    /// with `Responded` peers sorted first as a group.
    ///
    /// This order should not be used for reconnection attempts: use
    /// [`AddressBook::reconnection_peers`] instead.
    ///
    /// See [`CandidateSet`] for more details.
    fn cmp(&self, other: &Self) -> Ordering {
        use std::net::IpAddr::{V4, V6};
        use Ordering::*;

        let oldest_first = self.get_last_seen().cmp(&other.get_last_seen());
        let newest_first = oldest_first.reverse();

        let connection_state = self.last_connection_state.cmp(&other.last_connection_state);
        let reconnection_time = match self.last_connection_state {
            Responded => oldest_first,
            NeverAttemptedGossiped => newest_first,
            NeverAttemptedAlternate => newest_first,
            Failed => oldest_first,
            AttemptPending => oldest_first,
        };
        let ip_numeric = match (self.addr.ip(), other.addr.ip()) {
            (V4(a), V4(b)) => a.octets().cmp(&b.octets()),
            (V6(a), V6(b)) => a.octets().cmp(&b.octets()),
            (V4(_), V6(_)) => Less,
            (V6(_), V4(_)) => Greater,
        };

        connection_state
            .then(reconnection_time)
            // The remainder is meaningless as an ordering, but required so that we
            // have a total order on `MetaAddr` values: self and other must compare
            // as Equal iff they are equal.
            .then(ip_numeric)
            .then(self.addr.port().cmp(&other.addr.port()))
            .then(self.services.bits().cmp(&other.services.bits()))
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

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        self.last_seen()
            .expect("unexpected MetaAddr with missing last seen time: MetaAddrs should be sanitized before serialization")
            .zcash_serialize(&mut writer)?;
        writer.write_u64::<LittleEndian>(self.services.bits())?;
        writer.write_socket_addr(self.addr)?;
        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        let untrusted_last_seen = (&mut reader).zcash_deserialize_into()?;
        let untrusted_services =
            PeerServices::from_bits_truncate(reader.read_u64::<LittleEndian>()?);
        let addr = reader.read_socket_addr()?;

        Ok(MetaAddr::new_gossiped_meta_addr(
            addr,
            untrusted_services,
            untrusted_last_seen,
        ))
    }
}

/// A serialized meta addr has a 4 byte time, 8 byte services, 16 byte IP addr, and 2 byte port
const META_ADDR_SIZE: usize = 4 + 8 + 16 + 2;

impl TrustedPreallocate for MetaAddr {
    fn max_allocation() -> u64 {
        // Since a maximal serialized Vec<MetAddr> uses at least three bytes for its length (2MB  messages / 30B MetaAddr implies the maximal length is much greater than 253)
        // the max allocation can never exceed (MAX_PROTOCOL_MESSAGE_LEN - 3) / META_ADDR_SIZE
        ((MAX_PROTOCOL_MESSAGE_LEN - 3) / META_ADDR_SIZE) as u64
    }
}
