use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, PeerAddrState, PeerServices};

use chrono::{TimeZone, Utc};
use std::net::SocketAddr;

impl Arbitrary for MetaAddr {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<SocketAddr>(),
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
