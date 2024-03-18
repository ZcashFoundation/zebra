//! Randomised property tests for the address book.

use std::{net::SocketAddr, time::Instant};

use chrono::Utc;
use proptest::{collection::vec, prelude::*};
use tracing::Span;

use zebra_chain::{parameters::Network::*, serialization::Duration32};

use crate::{
    constants::{
        ADDR_RESPONSE_LIMIT_DENOMINATOR, DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK,
        MAX_ADDRS_IN_MESSAGE, MAX_PEER_ACTIVE_FOR_GOSSIP,
    },
    meta_addr::{arbitrary::MAX_META_ADDR, MetaAddr, MetaAddrChange},
    AddressBook,
};

const TIME_ERROR_MARGIN: Duration32 = Duration32::from_seconds(1);

const MAX_ADDR_CHANGE: usize = 10;

proptest! {
    #[test]
    fn only_recently_reachable_are_gossiped(
        local_listener in any::<SocketAddr>(),
        addresses in vec(any::<MetaAddr>(), 0..MAX_META_ADDR),
    ) {
        let _init_guard = zebra_test::init();
        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(
            local_listener,
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            addresses
        );

        // Only recently reachable are sanitized
        let sanitized = address_book.sanitized(chrono_now);
        let gossiped = address_book.fresh_get_addr_response();

        let expected_num_gossiped = sanitized.len().div_ceil(ADDR_RESPONSE_LIMIT_DENOMINATOR).min(MAX_ADDRS_IN_MESSAGE);
        let num_gossiped = gossiped.len();

        prop_assert_eq!(expected_num_gossiped, num_gossiped);

        for sanitized_address in sanitized {
            let duration_since_last_seen = sanitized_address
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
        let _init_guard = zebra_test::init();
        let instant_now = Instant::now();
        let chrono_now = Utc::now();

        let address_book = AddressBook::new_with_addrs(
            local_listener,
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            MAX_ADDRS_IN_ADDRESS_BOOK,
            Span::none(),
            addresses
        );

        for peer in address_book.reconnection_peers(instant_now, chrono_now) {
            prop_assert!(peer.is_probably_reachable(chrono_now), "peer: {:?}", peer);
        }
    }

    /// Test that the address book limit is respected for multiple peers.
    #[test]
    fn address_book_length_is_limited(
        local_listener in any::<SocketAddr>(),
        addr_changes_lists in vec(
            MetaAddrChange::addr_changes_strategy(MAX_ADDR_CHANGE),
            2..MAX_ADDR_CHANGE
        ),
        addr_limit in 1..=MAX_ADDR_CHANGE,
        pre_fill in any::<bool>(),
    ) {
        let _init_guard = zebra_test::init();

        let initial_addrs = if pre_fill {
            addr_changes_lists
                .iter()
                .map(|(addr, _changes)| addr)
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        // sequentially apply changes for one address, then move on to the next

        let mut address_book = AddressBook::new_with_addrs(
            local_listener,
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            addr_limit,
            Span::none(),
            initial_addrs.clone(),
        );

        for (_addr, changes) in addr_changes_lists.iter() {
            for change in changes {
                address_book.update(*change);

                prop_assert!(
                    address_book.len() <= addr_limit,
                    "sequential test length: {} was greater than limit: {}",
                    address_book.len(), addr_limit,
                );
            }
        }

        // interleave changes for different addresses

        let mut address_book = AddressBook::new_with_addrs(
            local_listener,
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            addr_limit,
            Span::none(),
            initial_addrs,
        );

        for index in 0..MAX_ADDR_CHANGE {
            for (_addr, changes) in addr_changes_lists.iter() {
                if let Some(change) = changes.get(index) {
                    address_book.update(*change);

                    prop_assert!(
                        address_book.len() <= addr_limit,
                        "interleave test length: {} was greater than limit: {}",
                        address_book.len(), addr_limit,
                    );
                }
            }
        }
    }
}
