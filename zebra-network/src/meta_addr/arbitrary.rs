//! Randomised test data generation for MetaAddr.

use std::time::Instant;

use proptest::{arbitrary::any, collection::vec, prelude::*};

use zebra_chain::{parameters::Network::*, serialization::DateTime32};

use crate::protocol::external::arbitrary::canonical_peer_addr_strategy;

use super::{MetaAddr, MetaAddrChange, PeerServices, PeerSocketAddr};

/// The largest number of random changes we want to apply to a [`MetaAddr`].
///
/// This should be at least twice the number of [`PeerAddrState`][1]s, so the
/// tests can cover multiple transitions through every state.
///
/// [1]: super::PeerAddrState
#[allow(dead_code)]
pub const MAX_ADDR_CHANGE: usize = 15;

/// The largest number of random addresses we want to add to an [`AddressBook`][2].
///
/// This should be at least the number of [`PeerAddrState`][1]s, so the tests
/// can cover interactions between addresses in different states.
///
/// [1]: super::PeerAddrState
/// [2]: crate::AddressBook
#[allow(dead_code)]
pub const MAX_META_ADDR: usize = 8;

impl MetaAddr {
    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`NeverAttemptedGossiped`][1] state.
    ///
    /// [1]: super::PeerAddrState::NeverAttemptedGossiped
    pub fn gossiped_strategy() -> BoxedStrategy<Self> {
        (
            canonical_peer_addr_strategy(),
            any::<PeerServices>(),
            any::<DateTime32>(),
        )
            .prop_map(|(addr, untrusted_services, untrusted_last_seen)| {
                MetaAddr::new_gossiped_meta_addr(addr, untrusted_services, untrusted_last_seen)
            })
            .boxed()
    }

    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`NeverAttemptedAlternate`][1] state.
    ///
    /// [1]: super::PeerAddrState::NeverAttemptedAlternate
    pub fn alternate_strategy() -> BoxedStrategy<Self> {
        (
            canonical_peer_addr_strategy(),
            any::<PeerServices>(),
            any::<Instant>(),
            any::<DateTime32>(),
        )
            .prop_map(|(socket_addr, untrusted_services, instant_now, local_now)| {
                // instant_now is not actually used for this variant,
                // so we could just provide a default value
                MetaAddr::new_alternate(socket_addr, &untrusted_services)
                    .into_new_meta_addr(instant_now, local_now)
            })
            .boxed()
    }
}

impl MetaAddrChange {
    /// Returns a strategy which generates changes for `addr`.
    ///
    /// `addr` is typically generated by the `canonical_peer_addr` strategy.
    pub fn addr_strategy(addr: PeerSocketAddr) -> BoxedStrategy<Self> {
        any::<MetaAddrChange>()
            .prop_map(move |mut change| {
                change.set_addr(addr);
                change
            })
            .boxed()
    }

    /// Returns a strategy which generates a `MetaAddr`, and a vector of up to
    /// `max_addr_change` changes.
    ///
    /// The address and the changes all have matching `PeerSocketAddr`s.
    pub fn addr_changes_strategy(
        max_addr_change: usize,
    ) -> BoxedStrategy<(MetaAddr, Vec<MetaAddrChange>)> {
        any::<MetaAddr>()
            .prop_flat_map(move |addr| {
                (
                    Just(addr),
                    vec(MetaAddrChange::addr_strategy(addr.addr), 1..max_addr_change),
                )
            })
            .boxed()
    }

    /// Create a strategy that generates [`MetaAddrChange`]s which are ready for
    /// outbound connections.
    ///
    /// Currently, all generated changes are the [`NewAlternate`][1] variant.
    /// TODO: Generate all [`MetaAddrChange`] variants, and give them ready
    /// fields. (After PR #2276 merges.)
    ///
    /// [1]: super::NewAlternate
    pub fn ready_outbound_strategy() -> BoxedStrategy<Self> {
        (
            canonical_peer_addr_strategy(),
            any::<Instant>(),
            any::<DateTime32>(),
        )
            .prop_filter_map(
                "failed MetaAddr::is_valid_for_outbound",
                |(addr, instant_now, local_now)| {
                    // Alternate nodes use the current time, so they're always ready
                    //
                    // TODO: create a "Zebra supported services" constant

                    let change = MetaAddr::new_alternate(addr, &PeerServices::NODE_NETWORK);
                    if change
                        .into_new_meta_addr(instant_now, local_now)
                        .last_known_info_is_valid_for_outbound(Mainnet)
                    {
                        Some(change)
                    } else {
                        None
                    }
                },
            )
            .boxed()
    }
}
