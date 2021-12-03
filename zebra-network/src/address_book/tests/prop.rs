//! Randomised property tests for the address book.

use std::{net::SocketAddr, time::Instant};

use chrono::Utc;
use proptest::{collection::vec, prelude::*};
use tracing::Span;

use zebra_chain::serialization::Duration32;

use crate::{
    constants::MAX_PEER_ACTIVE_FOR_GOSSIP,
    meta_addr::{arbitrary::MAX_META_ADDR, MetaAddr},
    AddressBook,
};

const TIME_ERROR_MARGIN: Duration32 = Duration32::from_seconds(1);

proptest! {
    #[test]
    fn only_recently_reachable_are_gossiped(
        local_listener in any::<SocketAddr>(),
        addresses in vec(any::<MetaAddr>(), 0..MAX_META_ADDR),
    ) {
        zebra_test::init();
        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(local_listener, Span::none(), addresses);

        for gossiped_address in address_book.sanitized(chrono_now) {
            let duration_since_last_seen = gossiped_address
                .last_seen()
                .expect("Peer that was never seen before is being gossiped")
                .saturating_elapsed(chrono_now)
                .saturating_sub(TIME_ERROR_MARGIN);

            prop_assert!(duration_since_last_seen <= MAX_PEER_ACTIVE_FOR_GOSSIP);
        }
    }

    /// Test that only peers that are reachable are listed for reconnection attempts.
    #[test]
    fn only_reachable_addresses_are_attempted(
        local_listener in any::<SocketAddr>(),
        addresses in vec(any::<MetaAddr>(), 0..MAX_META_ADDR),
    ) {
        zebra_test::init();
        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(local_listener, Span::none(), addresses);

        for peer in address_book.reconnection_peers(instant_now, chrono_now) {
            prop_assert!(peer.is_probably_reachable(chrono_now), "peer: {:?}", peer);
        }
    }
}
