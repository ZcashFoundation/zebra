//! Fixed test vectors for the address book.

use std::time::Instant;

use chrono::Utc;
use tracing::Span;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{
    constants::{DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK, MAX_PEER_MISBEHAVIOR_SCORE},
    meta_addr::{MetaAddr, MetaAddrChange},
    protocol::external::types::PeerServices,
    AddressBook,
};

/// Make sure an empty address book is actually empty.
#[test]
fn address_book_empty() {
    let address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        None
    );
    assert_eq!(address_book.len(), 0);
}

/// Helper: build a `MetaAddrChange::NewGossiped` for a given address and
/// last-seen time. Used to seed the address book before triggering a ban so
/// the test exercises the by-IP cleanup loop on real entries.
fn gossiped_change(
    addr: crate::PeerSocketAddr,
    services: PeerServices,
    untrusted_last_seen: DateTime32,
) -> MetaAddrChange {
    MetaAddr::new_gossiped_meta_addr(addr, services, untrusted_last_seen)
        .new_gossiped_change()
        .expect("gossiped MetaAddr should produce a NewGossiped change")
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10580.
///
/// Applying a ban-threshold misbehavior update with
/// `max_connections_per_ip > 1` previously panicked because the ban branch
/// unconditionally unwrapped `most_recent_by_ip`, which is only populated when
/// `max_connections_per_ip == 1`.
#[test]
fn misbehavior_ban_does_not_panic_with_max_connections_per_ip_above_one() {
    let banned_addr: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let other_port_same_ip: crate::PeerSocketAddr = "127.0.0.1:8234".parse().unwrap();
    let unrelated_addr: crate::PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();

    let mut address_book =
        AddressBook::new("0.0.0.0:0".parse().unwrap(), &Mainnet, 2, Span::current());

    // Seed two entries on the soon-to-be-banned IP plus an unrelated entry,
    // so the ban path's `by_addr` cleanup loop has visible work to do.
    address_book.update(gossiped_change(
        banned_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));
    address_book.update(gossiped_change(
        other_port_same_ip,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    ));
    address_book.update(gossiped_change(
        unrelated_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(2)),
    ));

    assert!(address_book.get(banned_addr).is_some());
    assert!(address_book.get(other_port_same_ip).is_some());

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned_addr,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert!(
        address_book.bans().contains_key(&banned_addr.ip()),
        "ban-threshold misbehavior should ban the peer IP"
    );
    assert!(
        address_book.get(banned_addr).is_none(),
        "primary banned address should be removed from the address book"
    );
    assert!(
        address_book.get(unrelated_addr).is_some(),
        "unrelated IP entries should remain after banning a different IP"
    );
}

/// Make sure peers are attempted in priority order.
#[test]
fn address_book_peer_order() {
    let addr1 = "127.0.0.1:1".parse().unwrap();
    let addr2 = "127.0.0.2:2".parse().unwrap();

    let mut meta_addr1 =
        MetaAddr::new_gossiped_meta_addr(addr1, PeerServices::NODE_NETWORK, DateTime32::MIN);
    let mut meta_addr2 = MetaAddr::new_gossiped_meta_addr(
        addr2,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );

    // Regardless of the order of insertion, the most recent address should be chosen first
    let addrs = vec![meta_addr1.clone(), meta_addr2.clone()];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2.clone()),
    );

    // Reverse the order, check that we get the same result
    let addrs = vec![meta_addr2.clone(), meta_addr1.clone()];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2.clone()),
    );

    // Now check that the order depends on the time, not the address
    meta_addr1.addr = addr2;
    meta_addr2.addr = addr1;

    let addrs = vec![meta_addr1.clone(), meta_addr2.clone()];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2.clone()),
    );

    // Reverse the order, check that we get the same result
    let addrs = vec![meta_addr2.clone(), meta_addr1];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );
}

/// Check that `reconnection_peers` skips addresses with IPs for which
/// Zebra already has recently updated outbound peers.
#[test]
fn reconnection_peers_skips_recently_updated_ip() {
    // tests that reconnection_peers() skips addresses where there's a connection at that IP with a recent:
    // - `last_response`
    test_reconnection_peers_skips_recently_updated_ip(true, |addr| {
        MetaAddr::new_responded(addr, None)
    });

    // tests that reconnection_peers() *does not* skip addresses where there's a connection at that IP with a recent:
    // - `last_attempt`
    test_reconnection_peers_skips_recently_updated_ip(false, MetaAddr::new_reconnect);
    // - `last_failure`
    test_reconnection_peers_skips_recently_updated_ip(false, |addr| {
        MetaAddr::new_errored(addr, PeerServices::NODE_NETWORK)
    });
}

fn test_reconnection_peers_skips_recently_updated_ip<
    M: Fn(crate::PeerSocketAddr) -> crate::meta_addr::MetaAddrChange,
>(
    should_skip_ip: bool,
    make_meta_addr_change: M,
) {
    let addr1 = "127.0.0.1:1".parse().unwrap();
    let addr2 = "127.0.0.1:2".parse().unwrap();

    let meta_addr1 = make_meta_addr_change(addr1).into_new_meta_addr(
        Instant::now(),
        Utc::now().try_into().expect("will succeed until 2038"),
    );
    let meta_addr2 = MetaAddr::new_gossiped_meta_addr(
        addr2,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );

    // The second address should be skipped because the first address has a
    // recent `last_response` time and the two addresses have the same IP.
    let addrs = vec![meta_addr1, meta_addr2];
    let address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        addrs,
    );

    let next_reconnection_peer = address_book
        .reconnection_peers(Instant::now(), Utc::now())
        .next();

    if should_skip_ip {
        assert_eq!(next_reconnection_peer, None,);
    } else {
        assert_ne!(next_reconnection_peer, None,);
    }
}

/// Helper: seed a book with two entries on one IP, separated in reconnection
/// order by an entry on a different IP.
///
/// `MetaAddr`'s `Ord` sorts more recently gossiped addresses first, and all
/// three entries are `NeverAttemptedGossiped`, so the last-seen times below
/// place the unrelated IP between the two same-IP entries. Returns the book and
/// the addresses in that order.
fn book_with_non_contiguous_same_ip_entries() -> (
    AddressBook,
    crate::PeerSocketAddr,
    crate::PeerSocketAddr,
    crate::PeerSocketAddr,
) {
    let banned_addr: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let unrelated_addr: crate::PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();
    // An ephemeral-port entry for the same IP, like the one in #11134.
    let zombie_addr: crate::PeerSocketAddr = "127.0.0.1:43562".parse().unwrap();

    // `max_connections_per_ip` is above one, so `reconnection_peers` does not
    // skip the second entry on the banned IP for being a duplicate IP.
    let mut address_book =
        AddressBook::new("0.0.0.0:0".parse().unwrap(), &Mainnet, 2, Span::current());

    address_book.update(gossiped_change(
        banned_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(2)),
    ));
    address_book.update(gossiped_change(
        unrelated_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    ));
    address_book.update(gossiped_change(
        zombie_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));

    // Without this ordering both tests below would pass even before the fix,
    // because a contiguous scan removes contiguous entries correctly.
    assert_eq!(
        address_book.by_addr.descending_keys().collect::<Vec<_>>(),
        vec![&banned_addr, &unrelated_addr, &zombie_addr],
        "test setup: the unrelated IP must sort between the two same-IP entries",
    );

    (address_book, banned_addr, unrelated_addr, zombie_addr)
}

/// Regression test for <https://github.com/ZcashFoundation/zebra/issues/11134>.
///
/// `by_addr` is ordered by reconnection order, not grouped by IP. The ban path
/// used to collect the banned IP's entries with
/// `skip_while(ip != banned).take_while(ip == banned)`, which stops at the first
/// entry for a different IP, so any later entry on the banned IP survived.
#[test]
fn ban_removes_non_contiguous_same_ip_entries() {
    let (mut address_book, banned_addr, unrelated_addr, zombie_addr) =
        book_with_non_contiguous_same_ip_entries();

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned_addr,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert!(
        address_book.bans().contains_key(&banned_addr.ip()),
        "ban-threshold misbehavior should ban the peer IP",
    );
    assert!(
        address_book.get(banned_addr).is_none(),
        "the banned address itself should be removed",
    );
    assert!(
        address_book.get(zombie_addr).is_none(),
        "a same-IP entry that does not sort next to the banned address \
         should also be removed",
    );
    assert!(
        address_book.get(unrelated_addr).is_some(),
        "entries for other IPs should be untouched",
    );
}

/// Regression test for <https://github.com/ZcashFoundation/zebra/issues/11134>.
///
/// A banned IP must never be returned as a reconnection candidate. Otherwise
/// `CandidateSet::next()` keeps selecting it, `update()` rejects the resulting
/// `UpdateAttempt` because the IP is banned, and the entry is never marked
/// `AttemptPending` — so its state never changes and it stays at the front of
/// the reconnection order for the lifetime of the process.
#[test]
fn banned_ip_is_never_a_reconnection_candidate() {
    let (mut address_book, banned_addr, unrelated_addr, _zombie_addr) =
        book_with_non_contiguous_same_ip_entries();

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned_addr,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    let candidates: Vec<_> = address_book
        .reconnection_peers(Instant::now(), Utc::now())
        .collect();

    assert!(
        candidates
            .iter()
            .all(|peer| peer.addr.ip() != banned_addr.ip()),
        "a banned IP must not be a reconnection candidate, but got: {candidates:?}",
    );
    assert_eq!(
        candidates.iter().map(|peer| peer.addr).collect::<Vec<_>>(),
        vec![unrelated_addr],
        "the unrelated IP should still be a candidate",
    );
}
