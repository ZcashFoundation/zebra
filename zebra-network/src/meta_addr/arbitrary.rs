use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, PeerAddrState, PeerServices};

use zebra_chain::serialization::arbitrary::{canonical_socket_addr, datetime_u32};

use chrono::{TimeZone, Utc};

impl MetaAddr {
    pub fn gossiped_strategy() -> BoxedStrategy<Self> {
        (
            canonical_socket_addr(),
            any::<PeerServices>(),
            datetime_u32(),
        )
            .prop_map(|(address, services, untrusted_last_seen)| {
                MetaAddr::new_gossiped_meta_addr(address, services, untrusted_last_seen)
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
            any::<u32>(),
            any::<PeerAddrState>(),
        )
            .prop_map(
                |(addr, services, last_seen, last_connection_state)| MetaAddr {
                    addr,
                    services,
                    // This can't panic, because all u32 values are valid `Utc.timestamp`s
                    last_seen: Utc.timestamp(last_seen.into(), 0),
                    last_connection_state,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
