use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, PeerAddrState, PeerServices};

use zebra_chain::serialization::{arbitrary::canonical_socket_addr, DateTime32};

impl MetaAddr {
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

    pub fn alternate_node_strategy() -> BoxedStrategy<Self> {
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
