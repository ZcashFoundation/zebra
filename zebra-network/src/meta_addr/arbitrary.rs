use proptest::{arbitrary::any, arbitrary::Arbitrary, prelude::*};

use super::{MetaAddr, PeerAddrState, PeerServices};

use std::{net::SocketAddr, time::SystemTime};

impl Arbitrary for MetaAddr {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<SocketAddr>(),
            any::<PeerServices>(),
            any::<SystemTime>(),
            any::<PeerAddrState>(),
        )
            .prop_map(
                |(addr, services, last_seen, last_connection_state)| MetaAddr {
                    addr,
                    services,
                    // TODO: implement constraints on last_seen as part of the
                    // last_connection_state refactor in #1849
                    last_seen: last_seen.into(),
                    last_connection_state,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
