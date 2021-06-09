use std::time::Instant;

use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, MetaAddrChange, PeerAddrState, PeerServices};

use zebra_chain::serialization::{arbitrary::canonical_socket_addr, DateTime32};

impl MetaAddr {
    /// Returns a strategy which generates arbitrary gossiped `MetaAddr`s.
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

    /// Returns a strategy which generates arbitrary [`MetaAddrChange::NewAlternate`]s.
    pub fn alternate_change_strategy() -> BoxedStrategy<MetaAddrChange> {
        canonical_socket_addr()
            .prop_map(|address| MetaAddr::new_alternate(&address, &PeerServices::NODE_NETWORK))
            .boxed()
    }
}

impl Arbitrary for MetaAddr {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            canonical_socket_addr(),
            any::<PeerServices>(),
            any::<Option<DateTime32>>(),
            any::<Option<DateTime32>>(),
            any::<Option<Instant>>(),
            any::<Option<Instant>>(),
            any::<PeerAddrState>(),
        )
            .prop_map(
                |(
                    addr,
                    services,
                    untrusted_last_seen,
                    last_response,
                    last_attempt,
                    last_failure,
                    last_connection_state,
                )| MetaAddr {
                    addr,
                    services,
                    untrusted_last_seen,
                    last_response,
                    last_attempt,
                    last_failure,
                    last_connection_state,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
