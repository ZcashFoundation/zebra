//! Randomised property tests for the address book.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    time::Instant,
};

use chrono::Utc;
use proptest::{collection::vec, prelude::*};
use tracing::Span;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{
    constants::{
        ADDR_RESPONSE_LIMIT_DENOMINATOR, DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK,
        MAX_ADDRS_IN_MESSAGE, MAX_PEER_ACTIVE_FOR_GOSSIP, MAX_PEER_MISBEHAVIOR_SCORE,
    },
    meta_addr::{arbitrary::MAX_META_ADDR, MetaAddr, MetaAddrChange},
    protocol::external::{canonical_peer_addr, connection_limit_key, types::PeerServices},
    AddressBook, AddressBookPeers, PeerSocketAddr,
};

use super::{super::PeerPriority, vectors::assert_address_book_invariants};

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
        assert_address_book_invariants(&address_book);

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
        assert_address_book_invariants(&address_book);

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
                address_book.update(change.clone());
                assert_address_book_invariants(&address_book);

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
                    address_book.update(change.clone());
                    assert_address_book_invariants(&address_book);

                    prop_assert!(
                        address_book.len() <= addr_limit,
                        "interleave test length: {} was greater than limit: {}",
                        address_book.len(), addr_limit,
                    );
                }
            }
        }
    }

    #[test]
    fn priority_key_matches_meta_addr_order(
        mut left in any::<MetaAddr>(),
        mut right in any::<MetaAddr>(),
    ) {
        left.addr = canonical_peer_addr(left.addr);
        right.addr = canonical_peer_addr(right.addr);
        prop_assert_eq!(
            PeerPriority::from(&left).cmp(&PeerPriority::from(&right)),
            left.cmp(&right),
        );

        // Also exercise address preferences and tie-breakers with all stored metadata equal.
        let mut same_priority = left.clone();
        same_priority.addr = right.addr;
        prop_assert_eq!(
            PeerPriority::from(&left).cmp(&PeerPriority::from(&same_priority)),
            left.cmp(&same_priority),
        );
    }

    #[test]
    fn indexed_updates_match_reference_order(
        operations in vec((0usize..8, any::<MetaAddrChange>(), 0u8..10), 1..80),
        addr_limit in 2usize..=6,
        max_connections_per_ip in 1usize..=2,
    ) {
        let _init_guard = zebra_test::init();
        let addresses: [PeerSocketAddr; 8] = [
            "127.0.0.1:8233",
            "127.0.0.1:8234",
            "127.0.0.2:8233",
            "127.0.0.3:8233",
            "[2001:db8:1::1]:8233",
            "[2001:db8:2::1]:8233",
            "[2001:db8::1]:8233",
            "[2001:db8::2]:8234",
        ].map(|addr| addr.parse().unwrap());
        let mut book = AddressBook::new_with_addrs(
            *addresses[0],
            &Mainnet,
            max_connections_per_ip,
            addr_limit,
            Span::none(),
            [],
        );
        let mut reference: Vec<MetaAddr> = Vec::new();
        let mut banned = HashSet::new();
        let mut recent: HashMap<_, MetaAddr> = HashMap::new();
        let now: DateTime32 = Utc::now().try_into().unwrap();
        let seed_changes = addresses.iter().enumerate().map(|(index, addr)| {
            (
                index,
                MetaAddrChange::NewGossiped {
                    addr: *addr,
                    untrusted_services: PeerServices::NODE_NETWORK,
                    untrusted_last_seen: now.saturating_sub(Duration32::from_seconds(
                        u32::try_from(addresses.len() - index).unwrap(),
                    )),
                },
                2,
            )
        });
        // Fill beyond capacity, then ban both stored IPv6 siblings before random operations.
        let initial_ban = std::iter::once((6, MetaAddr::new_initial_peer(addresses[6]), 0));
        for (index, mut change, action) in seed_changes.chain(initial_ban).chain(operations) {
            let addr = addresses[index];
            let group = connection_limit_key(addr.ip());
            change.set_addr(addr);
            if action == 0 {
                change = MetaAddrChange::UpdateMisbehavior {
                    addr,
                    score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
                    reason: "test misbehavior".to_string(),
                };
            }
            if action == 1 {
                let previous = reference.iter().find(|peer| peer.addr == addr).cloned();
                prop_assert_eq!(book.take(addr), previous);
                reference.retain(|peer| peer.addr != addr);
                if recent.get(&group).is_some_and(|peer| peer.addr == addr) {
                    recent.remove(&group);
                }
            } else {
                let updated = book.update(change.clone());
                if banned.contains(&group) {
                    prop_assert!(updated.is_none());
                } else if change.misbehavior_score() >= MAX_PEER_MISBEHAVIOR_SCORE {
                    prop_assert!(updated.is_none());
                    banned.insert(group);
                    reference.retain(|peer| connection_limit_key(peer.addr.ip()) != group);
                    recent.remove(&group);
                } else if let Some(updated) = updated {
                    // Reuse update timestamps, but sort, group and evict independently.
                    if max_connections_per_ip == 1
                        && updated.last_connection_state == crate::PeerAddrState::Responded
                        && recent.get(&group).is_none_or(|peer| {
                            updated.last_response() > peer.last_response()
                        })
                    {
                        recent.insert(group, updated.clone());
                    }
                    reference.retain(|peer| peer.addr != addr);
                    reference.push(updated);
                    reference.sort();
                    if reference.len() > addr_limit {
                        let evicted = reference.pop().unwrap();
                        let group = connection_limit_key(evicted.addr.ip());
                        if recent.get(&group).is_some_and(|peer| peer.addr == evicted.addr) {
                            recent.remove(&group);
                        }
                    }
                }
            }
            assert_address_book_invariants(&book);
            prop_assert_eq!(book.peers().collect::<Vec<_>>(), reference.clone());
            prop_assert_eq!(
                book.peers().rev().collect::<Vec<_>>(),
                reference.iter().rev().cloned().collect::<Vec<_>>(),
            );
            for addr in addresses {
                let expected = reference.iter().find(|peer| peer.addr == addr);
                let actual = book.get(addr);
                prop_assert_eq!(actual.as_ref(), expected);
                prop_assert_eq!(
                    actual.as_ref().and_then(MetaAddr::user_agent),
                    expected.and_then(MetaAddr::user_agent),
                );
                prop_assert_eq!(book.bans.is_banned(addr.ip()), banned.contains(
                    &connection_limit_key(addr.ip())
                ));
            }

            let instant_now = Instant::now();
            let chrono_now = Utc::now();
            let candidates: Vec<_> = reference.iter().filter(|peer| {
                peer.is_ready_for_connection_attempt(instant_now, chrono_now, &Mainnet)
                    && recent.get(&connection_limit_key(peer.addr.ip()))
                        .is_none_or(|peer| !peer.has_connection_recently_responded(chrono_now))
            }).cloned().collect();
            prop_assert_eq!(
                book.reconnection_peers(instant_now, chrono_now).collect::<Vec<_>>(),
                candidates,
            );
            prop_assert_eq!(
                book.cacheable(chrono_now),
                reference.iter().filter(|peer| peer.is_active_for_gossip(chrono_now))
                    .cloned().collect::<Vec<_>>(),
            );
            prop_assert_eq!(
                book.recently_live_peers(chrono_now),
                reference.iter().filter(|peer| peer.was_recently_live(chrono_now))
                    .cloned().collect::<Vec<_>>(),
            );
            let local = book.local_listener_meta_addr(chrono_now);
            let mut expected: Vec<_> = reference.iter()
                .filter(|peer| peer.addr != local.addr)
                .chain(std::iter::once(&local))
                .filter_map(|peer| peer.sanitize(&Mainnet))
                .filter(|peer| peer.is_active_for_gossip(chrono_now))
                .collect();
            expected.sort();
            let mut actual = book.sanitized(chrono_now);
            actual.sort();
            prop_assert_eq!(actual, expected);

            let cloned = book.clone();
            assert_address_book_invariants(&cloned);
            prop_assert_eq!(cloned.peers().collect::<Vec<_>>(), reference.clone());
        }
    }
}
