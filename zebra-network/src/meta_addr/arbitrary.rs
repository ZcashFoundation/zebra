use std::net::SocketAddr;

use proptest::{arbitrary::any, collection::vec, prelude::*};

use super::{MetaAddr, MetaAddrChange, PeerServices};

use zebra_chain::serialization::{arbitrary::canonical_socket_addr_strategy, DateTime32};

/// The largest number of random changes we want to apply to a [`MetaAddr`].
///
/// This should be at least twice the number of [`PeerAddrState`]s, so the tests
/// can cover multiple transitions through every state.
pub const MAX_ADDR_CHANGE: usize = 15;

/// The largest number of random addresses we want to add to an [`AddressBook`].
///
/// This should be at least the number of [`PeerAddrState`]s, so the tests can
/// cover interactions between addresses in different states.
pub const MAX_META_ADDR: usize = 8;

impl MetaAddr {
    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`PeerAddrState::NeverAttemptedGossiped`] state.
    pub fn gossiped_strategy() -> BoxedStrategy<Self> {
        (
            canonical_socket_addr_strategy(),
            any::<PeerServices>(),
            any::<DateTime32>(),
        )
            .prop_map(|(addr, untrusted_services, untrusted_last_seen)| {
                MetaAddr::new_gossiped_meta_addr(addr, untrusted_services, untrusted_last_seen)
            })
            .boxed()
    }

    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`PeerAddrState::NeverAttemptedAlternate`] state.
    pub fn alternate_strategy() -> BoxedStrategy<Self> {
        (canonical_socket_addr_strategy(), any::<PeerServices>())
            .prop_map(|(socket_addr, untrusted_services)| {
                MetaAddr::new_alternate(&socket_addr, &untrusted_services)
                    .into_new_meta_addr()
                    .expect("unexpected invalid alternate change")
            })
            .boxed()
    }
}

impl MetaAddrChange {
    /// Returns a strategy which generates changes for `socket_addr`.
    ///
    /// `socket_addr` is typically generated by the `canonical_socket_addr`
    /// strategy.
    pub fn addr_strategy(addr: SocketAddr) -> BoxedStrategy<Self> {
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
    /// The address and the changes all have matching `SocketAddr`s.
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
    /// Currently, all generated changes are the [`NewAlternate`] variant.
    /// TODO: Generate all [`MetaAddrChange`] variants, and give them ready fields.
    ///       (After PR #2276 merges.)
    pub fn ready_outbound_strategy() -> BoxedStrategy<Self> {
        canonical_socket_addr_strategy()
            .prop_filter_map("failed MetaAddr::is_valid_for_outbound", |addr| {
                // Alternate nodes use the current time, so they're always ready
                //
                // TODO: create a "Zebra supported services" constant
                let change = MetaAddr::new_alternate(&addr, &PeerServices::NODE_NETWORK);
                if change
                    .into_new_meta_addr()
                    .expect("unexpected invalid alternate change")
                    .last_known_info_is_valid_for_outbound()
                {
                    Some(change)
                } else {
                    None
                }
            })
            .boxed()
    }
}
