use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, PeerAddrState, PeerServices};

use zebra_chain::serialization::{arbitrary::canonical_socket_addr, DateTime32};

impl MetaAddr {
    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`PeerAddrState::NeverAttemptedGossiped`] state.
    pub fn gossiped_strategy() -> BoxedStrategy<Self> {
        (
            canonical_socket_addr(),
            any::<PeerServices>(),
            any::<DateTime32>(),
        )
            .prop_map(|(address, services, untrusted_last_seen)| {
                MetaAddr::new_gossiped_meta_addr(address, services, untrusted_last_seen)
            })
            .boxed()
    }

    /// Create a strategy that generates [`MetaAddr`]s in the
    /// [`PeerAddrState::NeverAttemptedAlternate`] state.
    pub fn alternate_strategy() -> BoxedStrategy<Self> {
        (canonical_socket_addr(), any::<PeerServices>())
            .prop_map(|(socket_addr, services)| MetaAddr::new_alternate(&socket_addr, &services))
            .boxed()
    }

    /// Create a strategy that generates [`MetaAddr`]s which are ready for outbound connections.
    ///
    /// TODO: Generate all [`PeerAddrState`] variants, and give them ready fields.
    ///       (After PR #2276 merges.)
    pub fn ready_outbound_strategy() -> BoxedStrategy<Self> {
        canonical_socket_addr()
            .prop_filter_map("failed MetaAddr::is_valid_for_outbound", |address| {
                // Alternate nodes use the current time, so they're always ready
                //
                // TODO: create a "Zebra supported services" constant
                let meta_addr = MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK);
                if meta_addr.is_valid_for_outbound() {
                    Some(meta_addr)
                } else {
                    None
                }
            })
            .boxed()
    }
}

impl Arbitrary for MetaAddr {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            canonical_socket_addr(),
            any::<PeerServices>(),
            any::<DateTime32>(),
            any::<PeerAddrState>(),
        )
            .prop_map(
                |(addr, services, last_seen, last_connection_state)| MetaAddr {
                    addr,
                    services,
                    last_seen,
                    last_connection_state,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
