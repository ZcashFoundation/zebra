//! An address-with-metadata type used in Bitcoin networking.
//!
//! In Zebra, [`MetaAddr`]s also track Zebra-specific peer state.

use std::{
    cmp::{Ord, Ordering},
    convert::TryInto,
    io::{Read, Write},
    net::SocketAddr,
};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use chrono::{DateTime, TimeZone, Utc};

use zebra_chain::serialization::{
    ReadZcashExt, SerializationError, TrustedPreallocate, WriteZcashExt, ZcashDeserialize,
    ZcashSerialize,
};

use crate::protocol::{external::MAX_PROTOCOL_MESSAGE_LEN, types::PeerServices};

use MetaAddrChange::*;
use PeerAddrState::*;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::option;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use zebra_chain::serialization::arbitrary::{datetime_full, datetime_u32};

#[cfg(test)]
mod tests;

/// Peer connection state, based on our interactions with the peer.
///
/// Zebra also tracks how recently we've had different kinds of interactions with
/// each peer, and derives peer usage based on the current time. This derived state
/// is tracked using [`AddressBook::recently_used_peers`] and
/// [`AddressBook::candidate_peers`].
///
/// To avoid depending on untrusted or default data, Zebra tracks the required
/// and optional data in each state. State updates are applied using
/// [`MetaAddrChange`]s.
///
/// See the [`CandidateSet`] for a detailed peer state diagram.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum PeerAddrState {
    /// The peer's address is a seed address.
    ///
    /// Seed addresses can come from:
    /// - hard-coded IP addresses in the config,
    /// - DNS names that resolve to a single peer, or
    /// - DNS seeders, which resolve to a dynamic list of peers.
    ///
    /// Zebra attempts to connect to all seed peers on startup. As part of these
    /// connection attempts, the initial peer connector updates all seed peers to
    /// [`AttemptPending`].
    ///
    /// If any seed addresses remain in the address book, they are attempted after
    /// disconnected [`Responded`] peers.
    NeverAttemptedSeed,

    /// The peer's address has just been fetched via peer gossip, but we haven't
    /// attempted to connect to it yet.
    ///
    /// Gossiped addresses are attempted after seed addresses.
    NeverAttemptedGossiped {
        /// The time that another node claims to have connected to this peer.
        /// See [`get_untrusted_last_seen`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_u32()")
        )]
        untrusted_last_seen: DateTime<Utc>,
        /// The services another node claims this peer advertises.
        /// See [`get_services`] for details.
        untrusted_services: PeerServices,
    },

    /// The peer's address has just been received as part of a [`Version`] message,
    /// so we might already be connected to this peer.
    ///
    /// Alternate addresses are attempted after gossiped addresses.
    NeverAttemptedAlternate {
        /// The last time another node gave us this address as their canonical
        /// address.
        /// See [`get_untrusted_last_seen`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        untrusted_last_seen: DateTime<Utc>,
        /// The services another node gave us along with their canonical address.
        /// See [`get_services`] for details.
        untrusted_services: PeerServices,
    },

    /// We are about to start a connection attempt to this peer.
    ///
    /// Pending addresses are retried last.
    AttemptPending {
        /// The last time we made an outbound attempt to this peer.
        /// See [`get_last_attempt`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        last_attempt: DateTime<Utc>,
        /// The last time we made a successful outbound connection to this peer.
        /// See [`get_last_success`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        last_success: Option<DateTime<Utc>>,
        /// The last time an outbound connection to this peer failed.
        /// See [`get_last_failed`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        last_failed: Option<DateTime<Utc>>,
        /// The last time another node claimed this peer was valid.
        /// See [`get_untrusted_last_seen`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        untrusted_last_seen: Option<DateTime<Utc>>,
        /// The services another node claims this peer advertises.
        /// See [`get_services`] for details.
        untrusted_services: Option<PeerServices>,
    },

    /// The peer has sent us a valid message.
    ///
    /// Peers remain in this state, even if they stop responding to requests.
    /// (Peer liveness is derived from the [`last_success`] time, and the current
    /// time.)
    ///
    /// Disconnected responded peers are retried first.
    Responded {
        /// The last time we made an outbound attempt to this peer.
        /// See [`get_last_attempt`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        last_attempt: DateTime<Utc>,
        /// The last time we made a successful outbound connection to this peer.
        /// See [`get_last_success`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        last_success: DateTime<Utc>,
        /// The last time an outbound connection to this peer failed.
        /// See [`get_last_failed`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        last_failed: Option<DateTime<Utc>>,
        /// The last time another node claimed this peer was valid.
        /// See [`get_untrusted_last_seen`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        untrusted_last_seen: Option<DateTime<Utc>>,
        /// The services advertised by this directly connected peer.
        /// See [`get_services`] for details.
        services: PeerServices,
    },

    /// The peer's TCP connection failed, or the peer sent us an unexpected
    /// Zcash protocol message, so we failed the connection.
    ///
    /// Failed peers are retried after disconnected [`Responded`] and never
    /// attempted peers.
    Failed {
        /// The last time we made an outbound attempt to this peer.
        /// See [`get_last_attempt`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        last_attempt: DateTime<Utc>,
        /// The last time we made a successful outbound connection to this peer.
        /// See [`get_last_success`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        last_success: Option<DateTime<Utc>>,
        /// The last time an outbound connection to this peer failed.
        /// See [`get_last_failed`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_full()")
        )]
        last_failed: DateTime<Utc>,
        /// The last time another node claimed this peer was valid.
        /// See [`get_untrusted_last_seen`] for details.
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "option::of(datetime_full())")
        )]
        untrusted_last_seen: Option<DateTime<Utc>>,
        /// The services claimed by another peer or advertised by this peer.
        /// See [`get_services`] for details.
        //
        // TODO: do we need to distinguish direct and untrusted services?
        untrusted_services: Option<PeerServices>,
    },
}

impl PeerAddrState {
    /// The last time we attempted to make a direct outbound connection to the
    /// address of this peer.
    ///
    /// Only updated by the [`AttemptPending`] state.
    /// Also present in [`Responded`] and [`Failed`].
    pub fn get_last_attempt(&self) -> Option<DateTime<Utc>> {
        match self {
            NeverAttemptedSeed => None,
            NeverAttemptedGossiped { .. } => None,
            NeverAttemptedAlternate { .. } => None,
            AttemptPending { last_attempt, .. } => Some(*last_attempt),
            Responded { last_attempt, .. } => Some(*last_attempt),
            Failed { last_attempt, .. } => Some(*last_attempt),
        }
    }

    /// The last time we successfully made a direct outbound connection to the
    /// address of this peer.
    ///
    /// Only updated by the [`Responded`] state.
    /// Also optionally present in [`Failed`] and [`AttemptPending`].
    pub fn get_last_success(&self) -> Option<DateTime<Utc>> {
        match self {
            NeverAttemptedSeed => None,
            NeverAttemptedGossiped { .. } => None,
            NeverAttemptedAlternate { .. } => None,
            AttemptPending { last_success, .. } => *last_success,
            Responded { last_success, .. } => Some(*last_success),
            Failed { last_success, .. } => *last_success,
        }
    }

    /// The last time a direct outbound connection to the address of this peer
    /// failed.
    ///
    /// Only updated by the [`Failed`] state.
    /// Also optionally present in [`AttemptPending`] and [`Responded`].
    pub fn get_last_failed(&self) -> Option<DateTime<Utc>> {
        match self {
            NeverAttemptedSeed => None,
            NeverAttemptedGossiped { .. } => None,
            NeverAttemptedAlternate { .. } => None,
            AttemptPending { last_failed, .. } => *last_failed,
            Responded { last_failed, .. } => *last_failed,
            Failed { last_failed, .. } => Some(*last_failed),
        }
    }

    /// The last time another peer successfully connected to this peer.
    ///
    /// Only updated by the [`NeverAttemptedGossiped`] and
    /// [`NeverAttemptedAlternate`] states.
    ///
    /// Optionally present in the [`AttemptPending`], [`Responded`], and [`Failed`]
    /// states.
    pub fn get_untrusted_last_seen(&self) -> Option<DateTime<Utc>> {
        match self {
            NeverAttemptedSeed => None,
            NeverAttemptedGossiped {
                untrusted_last_seen,
                ..
            } => Some(*untrusted_last_seen),
            NeverAttemptedAlternate {
                untrusted_last_seen,
                ..
            } => Some(*untrusted_last_seen),
            AttemptPending {
                untrusted_last_seen,
                ..
            } => *untrusted_last_seen,
            Responded {
                untrusted_last_seen,
                ..
            } => *untrusted_last_seen,
            Failed {
                untrusted_last_seen,
                ..
            } => *untrusted_last_seen,
        }
    }

    /// Only updated by the [`NeverAttemptedGossiped`] and
    /// [`NeverAttemptedAlternate`] states.
    ///
    /// Optionally present in the [`AttemptPending`], [`Responded`], and [`Failed`]
    /// states.
    pub fn get_untrusted_services(&self) -> Option<PeerServices> {
        match self {
            NeverAttemptedSeed => None,
            NeverAttemptedGossiped {
                untrusted_services, ..
            } => Some(*untrusted_services),
            NeverAttemptedAlternate {
                untrusted_services, ..
            } => Some(*untrusted_services),
            AttemptPending {
                untrusted_services, ..
            } => *untrusted_services,
            Responded { services, .. } => Some(*services),
            Failed {
                untrusted_services, ..
            } => *untrusted_services,
        }
    }
}

// non-test code should explicitly specify the peer address state
#[cfg(test)]
impl Default for PeerAddrState {
    fn default() -> Self {
        NeverAttemptedGossiped {
            untrusted_last_seen: Utc::now(),
            untrusted_services: PeerServices::NODE_NETWORK,
        }
    }
}

impl Ord for PeerAddrState {
    /// [`PeerAddrState`]s are sorted in approximate reconnection attempt
    /// order, ignoring liveness.
    ///
    /// See [`CandidateSet`] and [`MetaAddr::cmp`] for more details.
    fn cmp(&self, other: &Self) -> Ordering {
        use Ordering::*;
        let responded_never_failed_pending = match (self, other) {
            // If the states are the same, use the times to choose an order
            (Responded { .. }, Responded { .. })
            | (Failed { .. }, Failed { .. })
            | (NeverAttemptedGossiped { .. }, NeverAttemptedGossiped { .. })
            | (NeverAttemptedAlternate { .. }, NeverAttemptedAlternate { .. })
            | (NeverAttemptedSeed, NeverAttemptedSeed)
            | (AttemptPending { .. }, AttemptPending { .. }) => Equal,

            // We reconnect to `Responded` peers that have stopped sending messages,
            // then `NeverAttempted...` peers, then `Failed` peers
            (Responded { .. }, _) => Less,
            (_, Responded { .. }) => Greater,
            (NeverAttemptedSeed, _) => Less,
            (_, NeverAttemptedSeed) => Greater,
            (NeverAttemptedGossiped { .. }, _) => Less,
            (_, NeverAttemptedGossiped { .. }) => Greater,
            (NeverAttemptedAlternate { .. }, _) => Less,
            (_, NeverAttemptedAlternate { .. }) => Greater,
            (Failed { .. }, _) => Less,
            (_, Failed { .. }) => Greater,
            // AttemptPending is covered by the other cases
        };

        // Prioritise successful peers:
        // - try the most recent successful peers first
        // - try the oldest failed peers before re-trying the same peer
        // - re-try the oldest attempted peers first
        // - try the most recent last seen times first (untrusted - from remote peers)
        // - use the services as a tie-breaker
        //
        // `None` is earlier than any `Some(time)`, which means:
        // - peers that have never succeeded or with no last seen times sort last
        // - peers that have never failed or attempted sort first
        //
        // # Security
        //
        // Ignore untrusted times if we have any local times.
        let recent_successful = self
            .get_last_success()
            .cmp(&other.get_last_success())
            .reverse();
        let older_failed = self.get_last_failed().cmp(&other.get_last_failed());
        let older_attempted = self.get_last_attempt().cmp(&other.get_last_attempt());
        let recent_untrusted_last_seen = self
            .get_untrusted_last_seen()
            .cmp(&other.get_untrusted_last_seen())
            .reverse();
        let services_tie_breaker = self
            .get_untrusted_services()
            .cmp(&other.get_untrusted_services());

        responded_never_failed_pending
            .then(recent_successful)
            .then(older_failed)
            .then(older_attempted)
            .then(recent_untrusted_last_seen)
            .then(services_tie_breaker)
    }
}

impl PartialOrd for PeerAddrState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for PeerAddrState {
    fn eq(&self, other: &Self) -> bool {
        use Ordering::*;
        self.cmp(other) == Equal
    }
}

impl Eq for PeerAddrState {}

/// A change to a [`MetaAddr`] in an [`AddressBook`].
///
/// Most [`PeerAddrState`]s have a corresponding [`Change`]:
/// - `New...` changes create a new address book entry, or add fields to an
///   existing `NeverAttempted...` address book entry.
/// - `Update...` changes update the state, and add or update fields in an
///   existing address book entry.
/// The [`UpdateShutdown`] preserves the [`Responded`] state, but changes all
/// other states to [`Failed`].
///
/// See the [`CandidateSet`] for a detailed peer state diagram.
#[derive(Copy, Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum MetaAddrChange {
    /// A new seed peer [`MetaAddr`].
    ///
    /// The initial peers config provides the address.
    /// Gossiped or alternate changes can provide missing services.
    /// (But they can't update an existing service value.)
    /// Existing services can be updated by future handshakes with this peer.
    ///
    /// Sets the untrusted last seen time and services to [`None`].
    NewSeed { addr: SocketAddr },

    /// A new gossiped peer [`MetaAddr`].
    ///
    /// [`Addr`] messages provide an address, services, and a last seen time.
    /// The services can be updated by future handshakes with this peer.
    /// The untrusted last seen time is overridden by any last success time.
    NewGossiped {
        addr: SocketAddr,
        untrusted_services: PeerServices,
        #[cfg_attr(
            any(test, feature = "proptest-impl"),
            proptest(strategy = "datetime_u32()")
        )]
        untrusted_last_seen: DateTime<Utc>,
    },

    /// A new alternate peer [`MetaAddr`].
    ///
    /// [`Version`] messages provide the canonical address and its services.
    /// The services can be updated by future handshakes with this peer.
    ///
    /// Sets the untrusted last seen time to the current time.
    /// (Becase a connected peer just gave us this address.)
    NewAlternate {
        addr: SocketAddr,
        untrusted_services: PeerServices,
    },

    /// We have started a connection attempt to this peer.
    ///
    /// Sets the last attempt time to the current time.
    UpdateAttempt { addr: SocketAddr },

    /// A peer has sent us a message, after a successful handshake on an outbound
    /// connection.
    ///
    /// The services are updated based on the handshake with this peer.
    ///
    /// Sets the last success time to the current time.
    UpdateResponded {
        addr: SocketAddr,
        services: PeerServices,
    },

    /// A connection to this peer has failed.
    ///
    /// If the handshake with this peer succeeded, update its services.
    ///
    /// Sets the last failed time to the current time.
    UpdateFailed {
        addr: SocketAddr,
        services: Option<PeerServices>,
    },

    /// A connection to this peer has shut down.
    ///
    /// If the handshake with this peer succeeded, update its services.
    ///
    /// If the peer is in the [`Responded`] state, do nothing.
    /// Otherwise, mark the peer as [`Failed`], and set the last failed time to the
    /// current time.
    UpdateShutdown {
        addr: SocketAddr,
        services: Option<PeerServices>,
    },
}

impl MetaAddrChange {
    /// Return the address for the change.
    pub fn get_addr(&self) -> SocketAddr {
        match self {
            NewSeed { addr, .. } => *addr,
            NewGossiped { addr, .. } => *addr,
            NewAlternate { addr, .. } => *addr,
            UpdateAttempt { addr } => *addr,
            UpdateResponded { addr, .. } => *addr,
            UpdateFailed { addr, .. } => *addr,
            UpdateShutdown { addr, .. } => *addr,
        }
    }

    /// Return the services for the change, if available.
    pub fn get_untrusted_services(&self) -> Option<PeerServices> {
        match self {
            NewSeed { .. } => None,
            NewGossiped {
                untrusted_services, ..
            } => Some(*untrusted_services),
            NewAlternate {
                untrusted_services, ..
            } => Some(*untrusted_services),
            UpdateAttempt { .. } => None,
            UpdateResponded { services, .. } => Some(*services),
            UpdateFailed { services, .. } => *services,
            UpdateShutdown { services, .. } => *services,
        }
    }

    /// Return the untrusted last seen time for the change, if available.
    pub fn get_untrusted_last_seen(&self) -> Option<DateTime<Utc>> {
        match self {
            NewGossiped {
                untrusted_last_seen,
                ..
            } => Some(*untrusted_last_seen),
            NewSeed { .. }
            | NewAlternate { .. }
            | UpdateAttempt { .. }
            | UpdateResponded { .. }
            | UpdateFailed { .. }
            | UpdateShutdown { .. } => None,
        }
    }

    /// Is this address valid for outbound connections?
    ///
    /// `book_entry` is the entry for [`self.get_addr`] in the address book.
    ///
    /// Assmes that missing fields or entries are valid.
    pub fn is_valid_for_outbound(&self, book_entry: Option<MetaAddr>) -> bool {
        // Use the latest valid info we have
        self.into_meta_addr(book_entry)
            .or(book_entry)
            .map(|meta_addr| meta_addr.is_valid_for_outbound())
            .unwrap_or(true)
    }

    /// Is this address a directly connected client?
    ///
    /// `book_entry` is the entry for [`self.get_addr`] in the address book.
    ///
    /// Assmes that missing fields or entries are not clients.
    pub fn is_direct_client(&self, book_entry: Option<MetaAddr>) -> bool {
        // Use the latest valid info we have
        self.into_meta_addr(book_entry)
            .or(book_entry)
            .map(|meta_addr| meta_addr.is_direct_client())
            .unwrap_or(false)
    }

    /// Apply this change to `old_entry`, returning an updated entry.
    ///
    /// [`None`] means "no update".
    ///
    /// Ignores out-of-order updates:
    /// - the same address can arrive from multiple sources,
    /// - messages are sent by multiple tasks, and
    /// - multiple tasks read the address book,
    /// so messaging ordering is not guaranteed.
    /// (But the address book should eventually be consistent.)
    pub fn into_meta_addr(self, old_entry: Option<MetaAddr>) -> Option<MetaAddr> {
        let has_been_attempted = old_entry
            .map(|old| old.has_been_attempted())
            .unwrap_or(false);

        let old_last_attempt = old_entry.map(|old| old.get_last_attempt()).flatten();
        let old_last_success = old_entry.map(|old| old.get_last_success()).flatten();
        let old_last_failed = old_entry.map(|old| old.get_last_failed()).flatten();
        let old_untrusted_last_seen = old_entry.map(|old| old.get_untrusted_last_seen()).flatten();

        let old_untrusted_services = old_entry.map(|old| old.get_untrusted_services()).flatten();

        match self {
            // New seed, not in address book:
            NewSeed { addr } if old_entry.is_none() => {
                Some(MetaAddr::new(addr, NeverAttemptedSeed))
            }

            // New gossiped or alternate, not attempted:
            // Update state and services, but ignore updates to untrusted last seen
            NewGossiped {
                addr,
                untrusted_services,
                untrusted_last_seen,
            } if !has_been_attempted => Some(MetaAddr::new(
                addr,
                NeverAttemptedGossiped {
                    untrusted_services,
                    // Keep the first time we got
                    untrusted_last_seen: old_untrusted_last_seen.unwrap_or(untrusted_last_seen),
                },
            )),
            NewAlternate {
                addr,
                untrusted_services,
            } if !has_been_attempted => Some(MetaAddr::new(
                addr,
                NeverAttemptedAlternate {
                    untrusted_services,
                    // Keep the first time we got
                    untrusted_last_seen: old_untrusted_last_seen.unwrap_or_else(Utc::now),
                },
            )),

            // New entry, but already existing (seed) or attempted (others):
            // Skip the update entirely
            NewSeed { .. } | NewGossiped { .. } | NewAlternate { .. } => None,

            // Attempt:
            // Update last_attempt
            UpdateAttempt { addr } => Some(MetaAddr::new(
                addr,
                AttemptPending {
                    last_attempt: Utc::now(),
                    last_success: old_last_success,
                    last_failed: old_last_failed,
                    untrusted_last_seen: old_untrusted_last_seen,
                    untrusted_services: old_untrusted_services,
                },
            )),

            // Responded:
            // Update last_success and services
            UpdateResponded { addr, services } => Some(MetaAddr::new(
                addr,
                Responded {
                    // When the attempt message arrives, it will replace this default.
                    // (But it's a reasonable default anyway.)
                    last_attempt: old_last_attempt.unwrap_or_else(Utc::now),
                    last_success: Utc::now(),
                    last_failed: old_last_failed,
                    untrusted_last_seen: old_untrusted_last_seen,
                    services,
                },
            )),

            // Failed:
            // Update last_failed and services if present
            UpdateFailed { addr, services } => Some(MetaAddr::new(
                addr,
                Failed {
                    // When the attempt message arrives, it will replace this default.
                    // (But it's a reasonable default anyway.)
                    last_attempt: old_last_attempt.unwrap_or_else(Utc::now),
                    last_success: old_last_success,
                    last_failed: Utc::now(),
                    untrusted_last_seen: old_untrusted_last_seen,
                    // replace old services with new services if present
                    untrusted_services: services.or(old_untrusted_services),
                },
            )),

            // Shutdown:
            UpdateShutdown {
                addr,
                services: new_services,
            } => {
                match old_entry.map(|old| old.last_connection_state) {
                    // Responded:
                    // Keep as responded, update services if present
                    Some(Responded {
                        last_attempt,
                        last_success,
                        last_failed,
                        untrusted_last_seen,
                        services: old_services,
                    }) => Some(MetaAddr::new(
                        addr,
                        Responded {
                            last_attempt,
                            last_success,
                            last_failed,
                            untrusted_last_seen,
                            // this could be redundant, but it handles multiple
                            // connections and update reordering better
                            services: new_services.unwrap_or(old_services),
                        },
                    )),

                    // Attempted or Failed:
                    // Change to Failed
                    // Update last_failed and services if present
                    Some(AttemptPending { .. })
                    | Some(Failed { .. })
                    // Unexpected states: change to Failed anyway
                    | Some(NeverAttemptedSeed)
                    | Some(NeverAttemptedGossiped { .. })
                    | Some(NeverAttemptedAlternate { .. })
                    | None => Some(MetaAddr::new(
                        addr,
                        Failed {
                            // When the attempt message arrives, it will replace this default.
                            // (But it's a reasonable default anyway.)
                            last_attempt: old_last_attempt.unwrap_or_else(Utc::now),
                            last_success: old_last_success,
                            last_failed: Utc::now(),
                            untrusted_last_seen: old_untrusted_last_seen,
                            untrusted_services: new_services.or(old_untrusted_services),
                        },
                    )),
                }
            }
        }
    }
}

/// An address with metadata on its advertised services and last-seen time.
///
/// [Bitcoin reference](https://en.bitcoin.it/wiki/Protocol_documentation#Network_address)
#[derive(Copy, Clone, Debug)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct MetaAddr {
    /// The peer's address.
    ///
    /// The exact meaning depends on [`last_connection_state`]:
    ///   - [`Responded`]: the address we used to make a direct outbound connection
    ///      to this peer
    ///   - [`NeverAttemptedSeed: an unverified address provided by the seed config
    ///   - [`NeverAttemptedGossiped`]: an unverified address and services provided
    ///      by a remote peer
    ///   - [`NeverAttemptedAlternate`]: a directly connected peer claimed that
    ///      this address was its canonical address in its [`Version`] message,
    ///      (and provided services). But either:
    ///      - the peer made an inbound connection to us, or
    ///      - the address we used to make a direct outbound connection was
    ///        different from the canonical address
    ///   - [`Failed`] or [`AttemptPending`]: an unverified seeder, gossiped or
    ///      alternate address, or an address from a previous direct outbound
    ///      connection
    ///
    /// ## Security
    ///
    /// `addr`s from non-`Responded` peers may be invalid due to outdated
    /// records, or buggy or malicious peers.
    //
    // TODO: make the addr private to MetaAddr and AddressBook
    pub(super) addr: SocketAddr,

    /// The outcome of our most recent direct outbound connection to this peer.
    ///
    /// If we haven't made a direct connection to this peer, contains untrusted
    /// information provided by other peers (or seeds).
    //
    // TODO: make the state private to MetaAddr and AddressBook
    pub(super) last_connection_state: PeerAddrState,
}

impl MetaAddr {
    /// Create a new [`MetaAddr`] from its parts.
    ///
    /// This function should only be used by the [`meta_addr`] and [`address_book`]
    /// modules. Other callers should use a more specific [`MetaAddr`] or
    /// [`MetaAddrChange`] constructor.
    fn new(addr: SocketAddr, last_connection_state: PeerAddrState) -> MetaAddr {
        MetaAddr {
            addr,
            last_connection_state,
        }
    }

    /// Add or update an [`AddressBook`] entry, based on a configured seed
    /// address.
    pub fn new_seed(addr: SocketAddr) -> MetaAddrChange {
        NewSeed { addr }
    }

    /// Create a new gossiped [`MetaAddr`], based on the deserialized fields from
    /// a gossiped peer [`Addr`][crate::protocol::external::Message::Addr] message.
    pub fn new_gossiped_meta_addr(
        addr: SocketAddr,
        untrusted_services: PeerServices,
        untrusted_last_seen: DateTime<Utc>,
    ) -> MetaAddr {
        MetaAddr {
            addr,
            last_connection_state: NeverAttemptedGossiped {
                untrusted_last_seen,
                untrusted_services,
            },
        }
    }

    /// Add or update an [`AddressBook`] entry, based on a gossiped peer [`Addr`]
    /// message.
    ///
    /// Panics unless `meta_addr` is in the [`NeverAttemptedGossiped`] state.
    pub fn new_gossiped_change(meta_addr: MetaAddr) -> MetaAddrChange {
        if let NeverAttemptedGossiped {
            untrusted_last_seen,
            untrusted_services,
        } = meta_addr.last_connection_state
        {
            NewGossiped {
                addr: meta_addr.addr,
                untrusted_last_seen,
                untrusted_services,
            }
        } else {
            panic!(
                "unexpected non-NeverAttemptedGossiped state: {:?}",
                meta_addr
            )
        }
    }

    /// Add or update an [`AddressBook`] entry, based on the canonical address in a
    /// peer's [`Version`] message.
    pub fn new_alternate(addr: SocketAddr, untrusted_services: PeerServices) -> MetaAddrChange {
        NewAlternate {
            addr,
            untrusted_services,
        }
    }

    /// Update an [`AddressBook`] entry when we start connecting to a peer.
    pub fn update_attempt(addr: SocketAddr) -> MetaAddrChange {
        UpdateAttempt { addr }
    }

    /// Update an [`AddressBook`] entry when a peer sends a message after a
    /// successful handshake.
    ///
    /// # Security
    ///
    /// This address must be the remote address from an outbound connection,
    /// and the services must be the services from that peer's handshake.
    ///
    /// Otherwise:
    /// - malicious peers could interfere with other peers' [`AddressBook`] state,
    ///   or
    /// - Zebra could advertise unreachable addresses to its own peers.
    pub fn update_responded(addr: SocketAddr, services: PeerServices) -> MetaAddrChange {
        UpdateResponded { addr, services }
    }

    /// Update an [`AddressBook`] entry when a peer connection fails.
    pub fn update_failed(addr: SocketAddr, services: Option<PeerServices>) -> MetaAddrChange {
        UpdateFailed { addr, services }
    }

    /// Update an [`AddressBook`] entry when a peer connection shuts down.
    pub fn update_shutdown(addr: SocketAddr, services: Option<PeerServices>) -> MetaAddrChange {
        UpdateShutdown { addr, services }
    }

    /// Add or update our local listener address in an [`AddressBook`].
    ///
    /// See [`AddressBook::get_local_listener`] for details.
    pub fn new_local_listener(addr: SocketAddr) -> MetaAddrChange {
        NewAlternate {
            addr,
            // Note: in this unique case, the services are actually trusted
            //
            // TODO: create a "local services" constant
            untrusted_services: PeerServices::NODE_NETWORK,
        }
    }

    /// The services advertised by the peer.
    ///
    /// The exact meaning depends on [`last_connection_state`]:
    ///   - [`Responded`]: the services advertised by this peer, the last time we
    ///      performed a handshake with it
    ///   - [`NeverAttemptedGossiped`]: the unverified services provided by the
    ///      remote peer that sent us this address
    ///   - [`NeverAttemptedAlternate`]: the services provided by the directly
    ///      connected peer that claimed that this address was its canonical
    ///      address
    ///   - [`NeverAttemptedSeed`]: the seed config doesn't have any service info,
    ///      so this field is [`None`]
    ///   - [`Failed`] or [`AttemptPending`]: unverified services via another peer,
    ///      or services advertised in a previous handshake
    ///
    /// ## Security
    ///
    /// `services` from non-`Responded` peers may be invalid due to outdated
    /// records, older peer versions, or buggy or malicious peers.
    /// The last time another peer successfully connected to this peer.
    pub fn get_untrusted_services(&self) -> Option<PeerServices> {
        self.last_connection_state.get_untrusted_services()
    }

    /// The last time we attempted to make a direct outbound connection to the
    /// address of this peer.
    ///
    /// See [`PeerAddrState::get_last_attempt`] for details.
    pub fn get_last_attempt(&self) -> Option<DateTime<Utc>> {
        self.last_connection_state.get_last_attempt()
    }

    /// The last time we successfully made a direct outbound connection to the
    /// address of this peer.
    ///
    /// See [`PeerAddrState::get_last_success`] for details.
    pub fn get_last_success(&self) -> Option<DateTime<Utc>> {
        self.last_connection_state.get_last_success()
    }

    /// The last time a direct outbound connection to the address of this peer
    /// failed.
    ///
    /// See [`PeerAddrState::get_last_failed`] for details.
    pub fn get_last_failed(&self) -> Option<DateTime<Utc>> {
        self.last_connection_state.get_last_failed()
    }

    /// The last time another node claimed this peer was valid.
    ///
    /// The exact meaning depends on [`last_connection_state`]:
    ///   - [`NeverAttemptedSeed`]: the seed config doesn't have any last seen info,
    ///      so this field is [`None`]
    ///   - [`NeverAttemptedGossiped`]: the unverified time provided by the remote
    ///      peer that sent us this address
    ///   - [`NeverAttemptedAlternate`]: the local time we received the [`Version`]
    ///      message containing this address from a peer
    ///   - [`Failed`] and [`AttemptPending`]: these states do not update this field
    ///
    /// ## Security
    ///
    /// last seen times from non-`Responded` peers may be invalid due to
    /// clock skew, or buggy or malicious peers.
    ///
    /// Typically, this field should be ignored, unless the peer is in a
    /// never attempted state.
    pub fn get_untrusted_last_seen(&self) -> Option<DateTime<Utc>> {
        self.last_connection_state.get_untrusted_last_seen()
    }

    /// The last time we successfully made a direct outbound connection to this
    /// peer, or another node claimed this peer was valid.
    ///
    /// Clamped to a [`u32`] number of seconds.
    ///
    /// [`None`] if the address was supplied as a seed.
    ///
    /// ## Security
    ///
    /// last seen times from non-`Responded` peers may be invalid due to
    /// clock skew, or buggy or malicious peers.
    ///
    /// Use [`get_last_success`] if you need a trusted, unclamped value.
    pub fn get_last_success_or_untrusted(&self) -> Option<DateTime<Utc>> {
        // Use the best time we have, if any
        let seconds = self
            .get_last_success()
            .or_else(|| self.get_untrusted_last_seen())?
            .timestamp();

        // Convert to a DateTime
        let seconds = seconds.clamp(u32::MIN.into(), u32::MAX.into());
        let time = Utc
            .timestamp_opt(seconds, 0)
            .single()
            .expect("unexpected invalid time: all u32 values should be valid");

        Some(time)
    }

    /// Has this peer ever been attempted?
    pub fn has_been_attempted(&self) -> bool {
        self.get_last_attempt().is_some()
    }

    /// Is this address a directly connected client?
    ///
    /// If we don't have enough information, assume that it is not a client.
    pub fn is_direct_client(&self) -> bool {
        match self.last_connection_state {
            Responded { services, .. } => !services.contains(PeerServices::NODE_NETWORK),
            NeverAttemptedSeed
            | NeverAttemptedGossiped { .. }
            | NeverAttemptedAlternate { .. }
            | Failed { .. }
            | AttemptPending { .. } => false,
        }
    }

    /// Is this address valid for outbound connections?
    ///
    /// If we don't have enough information, assume that it is valid.
    pub fn is_valid_for_outbound(&self) -> bool {
        self.get_untrusted_services()
            .map(|services| services.contains(PeerServices::NODE_NETWORK))
            .unwrap_or(true)
            && !self.addr.ip().is_unspecified()
            && self.addr.port() != 0
    }

    /// Return a sanitized version of this [`MetaAddr`], for sending to a remote peer.
    ///
    /// If [`None`], this address should not be sent to remote peers.
    pub fn sanitize(&self) -> Option<MetaAddr> {
        // Sanitize the time
        let interval = crate::constants::TIMESTAMP_TRUNCATION_SECONDS;
        let ts = self.get_last_success_or_untrusted()?.timestamp();
        let last_seen_maybe_untrusted = Utc.timestamp(ts - ts.rem_euclid(interval), 0);

        Some(MetaAddr {
            addr: self.addr,
            // the exact state variant isn't sent to the remote peer, but sanitize it anyway
            last_connection_state: NeverAttemptedGossiped {
                untrusted_last_seen: last_seen_maybe_untrusted,
                // services are sanitized during parsing, or set to a fixed value by
                // new_local_listener, so we don't need to sanitize here
                untrusted_services: self.get_untrusted_services()?,
            },
        })
    }
}

impl Ord for MetaAddr {
    /// [`MetaAddr`]s are sorted in approximate reconnection attempt order, but
    /// with [`Responded`] peers sorted first as a group.
    ///
    /// This order should not be used for reconnection attempts: use
    /// [`AddressBook::reconnection_peers`] instead.
    ///
    /// See [`CandidateSet`] for more details.
    fn cmp(&self, other: &Self) -> Ordering {
        use std::net::IpAddr::{V4, V6};
        use Ordering::*;

        let connection_state = self.last_connection_state.cmp(&other.last_connection_state);

        let ip_numeric = match (self.addr.ip(), other.addr.ip()) {
            (V4(a), V4(b)) => a.octets().cmp(&b.octets()),
            (V6(a), V6(b)) => a.octets().cmp(&b.octets()),
            (V4(_), V6(_)) => Less,
            (V6(_), V4(_)) => Greater,
        };

        connection_state
            // The remainder is meaningless as an ordering, but required so that we
            // have a total order on `MetaAddr` values: self and other must compare
            // as Equal iff they are equal.
            .then(ip_numeric)
            .then(self.addr.port().cmp(&other.addr.port()))
    }
}

impl PartialOrd for MetaAddr {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for MetaAddr {
    fn eq(&self, other: &Self) -> bool {
        use Ordering::*;
        self.cmp(other) == Equal
    }
}

impl Eq for MetaAddr {}

impl ZcashSerialize for MetaAddr {
    fn zcash_serialize<W: Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_u32::<LittleEndian>(
            self.get_last_success_or_untrusted()
                .expect("unexpected serialization of MetaAddr with missing fields")
                .timestamp()
                .try_into()
                .expect("time is in range"),
        )?;
        writer.write_u64::<LittleEndian>(
            self.get_untrusted_services()
                .expect("unexpected serialization of MetaAddr with missing fields")
                .bits(),
        )?;
        writer.write_socket_addr(self.addr)?;
        Ok(())
    }
}

impl ZcashDeserialize for MetaAddr {
    fn zcash_deserialize<R: Read>(mut reader: R) -> Result<Self, SerializationError> {
        // This can't panic, because all u32 values are valid `Utc.timestamp`s
        let untrusted_last_seen = Utc.timestamp(reader.read_u32::<LittleEndian>()?.into(), 0);
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
