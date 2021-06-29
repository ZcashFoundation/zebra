use std::net::SocketAddr;

use proptest::{collection::vec, prelude::*};
use tracing::Span;

use zebra_chain::serialization::Duration32;

use super::super::AddressBook;
use crate::{
    constants::MAX_PEER_ACTIVE_FOR_GOSSIP,
    meta_addr::{arbitrary::MAX_META_ADDR, MetaAddr},
};

const TIME_ERROR_MARGIN: Duration32 = Duration32::from_seconds(1);

proptest! {
    #[test]
    fn only_recently_reachable_are_gossiped(
        local_listener in any::<SocketAddr>(),
        addresses in vec(any::<MetaAddr>(), 0..MAX_META_ADDR),
    ) {
        zebra_test::init();

        let address_book = AddressBook::new_with_addrs(local_listener, Span::none(), addresses);

        for gossiped_address in address_book.sanitized() {
            let duration_since_last_seen = gossiped_address
                .last_seen()
                .expect("Peer that was never seen before is being gossiped")
                .saturating_elapsed()
                .saturating_sub(TIME_ERROR_MARGIN);

            prop_assert!(duration_since_last_seen <= MAX_PEER_ACTIVE_FOR_GOSSIP);
        }
    }
}
