//! Fixed test vectors for the address book.

use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

use chrono::Utc;
use tracing::Span;

use zebra_chain::{
    parameters::Network::*,
    serialization::{DateTime32, Duration32},
};

use crate::{
    constants::{
        BAN_DURATION, DEFAULT_MAX_CONNS_PER_IP, MAX_ADDRS_IN_ADDRESS_BOOK,
        MAX_PEER_MISBEHAVIOR_SCORE,
    },
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

fn assert_address_book_invariants(address_book: &AddressBook) {
    assert!(
        address_book.by_addr.values().is_sorted(),
        "address book values must remain sorted in ascending MetaAddr reconnection-attempt order",
    );

    for (addr, meta_addr) in &address_book.by_addr {
        assert_eq!(
            *addr, meta_addr.addr,
            "address book keys must match their stored MetaAddr addresses",
        );
    }
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
        address_book.bans().is_banned(banned_addr.ip()),
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
    assert_address_book_invariants(&address_book);
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
    assert_address_book_invariants(&address_book);
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
    assert_address_book_invariants(&address_book);
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
    assert_address_book_invariants(&address_book);
    assert_eq!(
        address_book
            .reconnection_peers(Instant::now(), Utc::now())
            .next(),
        Some(meta_addr2),
    );
}

#[test]
fn address_book_mutations_preserve_peer_order() {
    let addr1 = "127.0.0.1:8233".parse().unwrap();
    let addr2 = "127.0.0.2:8233".parse().unwrap();
    let addr3 = "127.0.0.3:8233".parse().unwrap();

    let meta_addr1 =
        MetaAddr::new_gossiped_meta_addr(addr1, PeerServices::NODE_NETWORK, DateTime32::MIN);
    let meta_addr2 = MetaAddr::new_gossiped_meta_addr(
        addr2,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );
    let meta_addr3 = MetaAddr::new_gossiped_meta_addr(
        addr3,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(2)),
    );

    let mut address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        MAX_ADDRS_IN_ADDRESS_BOOK,
        Span::current(),
        [meta_addr2.clone(), meta_addr1.clone(), meta_addr3.clone()],
    );
    assert_address_book_invariants(&address_book);
    assert_eq!(
        address_book.peers().collect::<Vec<_>>(),
        vec![meta_addr3.clone(), meta_addr2.clone(), meta_addr1.clone()],
    );

    let reprioritized_addr1 = MetaAddr::new_gossiped_meta_addr(
        addr1,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(3)),
    );
    assert_eq!(
        address_book.insert_meta_addr(reprioritized_addr1.clone()),
        Some(meta_addr1),
    );
    assert_address_book_invariants(&address_book);
    assert_eq!(
        address_book.peers().collect::<Vec<_>>(),
        vec![
            reprioritized_addr1.clone(),
            meta_addr3.clone(),
            meta_addr2.clone(),
        ],
    );

    assert_eq!(address_book.take(addr1), Some(reprioritized_addr1));
    assert_address_book_invariants(&address_book);
    assert_eq!(
        address_book.peers().collect::<Vec<_>>(),
        vec![meta_addr3, meta_addr2],
    );
}

#[test]
fn address_book_insert_canonicalizes_key_and_value() {
    let canonical_addr = "127.0.0.4:8233".parse().unwrap();
    let noncanonical_addr = "[::ffff:127.0.0.4]:8233".parse().unwrap();
    let canonical_meta_addr = MetaAddr::new_gossiped_meta_addr(
        canonical_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    );
    let mut meta_addr = MetaAddr::new_gossiped_meta_addr(
        canonical_addr,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
    );
    meta_addr.addr = noncanonical_addr;

    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    assert_eq!(
        address_book.insert_meta_addr(canonical_meta_addr.clone()),
        None,
    );
    assert_eq!(
        address_book.insert_meta_addr(meta_addr),
        Some(canonical_meta_addr),
    );
    assert_eq!(address_book.by_addr.len(), 1);
    assert!(!address_book.by_addr.contains_key(&noncanonical_addr));
    assert_eq!(
        address_book
            .by_addr
            .get(&canonical_addr)
            .expect("the canonical address was inserted")
            .addr,
        canonical_addr,
    );
    assert_address_book_invariants(&address_book);
}

#[test]
fn address_book_evicts_lowest_priority_peer() {
    let addr1 = "127.0.0.1:8233".parse().unwrap();
    let addr2 = "127.0.0.2:8233".parse().unwrap();
    let addr3 = "127.0.0.3:8233".parse().unwrap();

    let mut address_book = AddressBook::new_with_addrs(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        2,
        Span::current(),
        [],
    );

    for (addr, last_seen) in [
        (addr1, DateTime32::MIN),
        (
            addr2,
            DateTime32::MIN.saturating_add(Duration32::from_seconds(1)),
        ),
        (
            addr3,
            DateTime32::MIN.saturating_add(Duration32::from_seconds(2)),
        ),
    ] {
        assert!(address_book
            .update(gossiped_change(addr, PeerServices::NODE_NETWORK, last_seen,))
            .is_some(),);
        assert_address_book_invariants(&address_book);
    }

    assert_eq!(
        address_book
            .peers()
            .map(|meta_addr| meta_addr.addr)
            .collect::<Vec<_>>(),
        vec![addr3, addr2],
    );
    assert!(address_book.get(addr1).is_none());
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

/// Regression test for <https://github.com/ZcashFoundation/zebra/issues/11134>.
///
/// `by_addr` is ordered by reconnection order, not grouped by IP, so the ban path's old
/// `skip_while(ip != banned).take_while(ip == banned)` scan stopped at the first entry for a
/// different IP, and any later entry on the banned IP survived. That survivor then stayed at the
/// front of the reconnection order for the lifetime of the process: it was selected as a candidate
/// on every crawl, and `update()` rejected the resulting `UpdateAttempt` because the IP was
/// banned, so its state never changed.
#[test]
fn ban_removes_every_entry_for_the_banned_ip() {
    let banned_addr: crate::PeerSocketAddr = "127.0.0.1:8233".parse().unwrap();
    let unrelated_addr: crate::PeerSocketAddr = "127.0.0.2:8233".parse().unwrap();
    // An ephemeral-port entry for the same IP, like the one in #11134.
    let zombie_addr: crate::PeerSocketAddr = "127.0.0.1:43562".parse().unwrap();

    // `max_connections_per_ip` is above one, so `reconnection_peers` does not skip the second
    // entry on the banned IP for being a duplicate IP.
    let mut address_book =
        AddressBook::new("0.0.0.0:0".parse().unwrap(), &Mainnet, 2, Span::current());

    // `MetaAddr`'s `Ord` sorts more recently gossiped addresses first, so these last seen times
    // place the unrelated IP between the two entries for the banned IP.
    for (addr, last_seen) in [(banned_addr, 2), (unrelated_addr, 1), (zombie_addr, 0)] {
        address_book.update(gossiped_change(
            addr,
            PeerServices::NODE_NETWORK,
            DateTime32::MIN.saturating_add(Duration32::from_seconds(last_seen)),
        ));
    }

    // Without this ordering the test would also pass before the fix, because a contiguous scan
    // removes contiguous entries correctly.
    assert_eq!(
        address_book.by_addr.keys().collect::<Vec<_>>(),
        vec![&banned_addr, &unrelated_addr, &zombie_addr],
        "test setup: the unrelated IP must sort between the two entries for the banned IP",
    );

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned_addr,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert!(
        address_book.bans().is_banned(banned_addr.ip()),
        "ban-threshold misbehavior should ban the peer IP",
    );
    assert_eq!(
        address_book.by_addr.keys().collect::<Vec<_>>(),
        vec![&unrelated_addr],
        "the ban should remove every entry for the banned IP, including the one that does not \
         sort next to the banned address",
    );

    let candidates: Vec<_> = address_book
        .reconnection_peers(Instant::now(), Utc::now())
        .map(|peer| peer.addr)
        .collect();

    assert_eq!(
        candidates,
        vec![unrelated_addr],
        "a banned IP must never be a reconnection candidate",
    );
}

/// A peer that rotates through the addresses of one IPv6 `/64` must not evade
/// a misbehavior ban.
///
/// Bans used to be keyed by the full address, so a peer with a `/64` could
/// misbehave from each of its 2^64 addresses in turn and never be shut out.
#[test]
fn ipv6_rotation_within_one_64_cannot_evade_a_ban() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: "[2001:db8::1]:8233".parse().unwrap(),
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    let group: IpAddr = "2001:db8::".parse().unwrap();
    assert!(
        address_book.bans().is_banned(group),
        "the ban should be keyed on the /64, not the individual address"
    );

    // Every address in the banned /64 is now rejected, including ones that
    // never misbehaved themselves.
    let untouched: crate::PeerSocketAddr = "[2001:db8::dead:beef]:8233".parse().unwrap();
    address_book.update(gossiped_change(
        untouched,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));
    assert!(
        address_book.get(untouched).is_none(),
        "a fresh address in the banned /64 must not be added to the address book"
    );

    // A different /64 is unaffected.
    let other: crate::PeerSocketAddr = "[2001:db8:1::1]:8233".parse().unwrap();
    address_book.update(gossiped_change(
        other,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));
    assert!(
        address_book.get(other).is_some(),
        "an address in a different /64 should still be accepted"
    );
}

/// IPv4 peers are grouped per address, so one misbehaving IPv4 peer must not
/// ban its neighbours.
#[test]
fn ipv4_ban_does_not_affect_neighbouring_addresses() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    let misbehaving: crate::PeerSocketAddr = "192.0.2.10:8233".parse().unwrap();
    let neighbour: crate::PeerSocketAddr = "192.0.2.11:8233".parse().unwrap();

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: misbehaving,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert!(
        address_book.bans().is_banned(misbehaving.ip()),
        "the misbehaving IPv4 address should be banned"
    );

    address_book.update(gossiped_change(
        neighbour,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));
    assert!(
        address_book.get(neighbour).is_some(),
        "a neighbouring IPv4 address in the same /24 must not be banned"
    );
}

/// Scores are not currently accumulated: a score at
/// `MAX_PEER_MISBEHAVIOR_SCORE` bans the peer group on its own, and a score
/// below it is dropped.
///
/// Every score Zebra produces is `0` or `MAX_PEER_MISBEHAVIOR_SCORE`, so a
/// score below the threshold is a programming error, which the address book
/// logs. It is still checked against the threshold rather than against zero, so
/// this path keeps working if intermediate scores and accumulation are ever
/// added back.
///
/// Change this test if we ever support partial scores.
#[test]
fn only_a_threshold_misbehavior_score_bans_the_peer_group() {
    for (score, should_ban) in [
        (1, false),
        (MAX_PEER_MISBEHAVIOR_SCORE - 1, false),
        (MAX_PEER_MISBEHAVIOR_SCORE, true),
        (MAX_PEER_MISBEHAVIOR_SCORE + 1, true),
    ] {
        let mut address_book = AddressBook::new(
            "0.0.0.0:0".parse().unwrap(),
            &Mainnet,
            DEFAULT_MAX_CONNS_PER_IP,
            Span::current(),
        );

        let misbehaving: crate::PeerSocketAddr = "[2001:db8::1]:8233".parse().unwrap();
        address_book.update(MetaAddrChange::UpdateMisbehavior {
            addr: misbehaving,
            score_increment: score,
        });

        assert_eq!(
            address_book.bans().is_banned(misbehaving.ip()),
            should_ban,
            "a misbehavior score of {score} should ban the peer group: {should_ban}"
        );
        assert_eq!(
            address_book.misbehavior_score(misbehaving),
            if should_ban {
                MAX_PEER_MISBEHAVIOR_SCORE
            } else {
                0
            },
            "a score below the threshold is dropped, not stored: {score}"
        );
    }
}

/// The score reported for an address is its whole peer group's ban state, which
/// is what `getpeerinfo` surfaces as `banscore`.
#[test]
fn misbehavior_score_is_reported_per_group() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    let banned: crate::PeerSocketAddr = "[2001:db8::1]:8233".parse().unwrap();
    let sibling: crate::PeerSocketAddr = "[2001:db8::2]:8233".parse().unwrap();
    let other_group: crate::PeerSocketAddr = "[2001:db8:1::1]:8233".parse().unwrap();

    for addr in [banned, sibling, other_group] {
        assert_eq!(
            address_book.misbehavior_score(addr),
            0,
            "an unbanned group should report no score: {addr:?}"
        );
    }

    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert_eq!(
        address_book.misbehavior_score(banned),
        MAX_PEER_MISBEHAVIOR_SCORE,
        "the banned address should report the ban score"
    );
    assert_eq!(
        address_book.misbehavior_score(sibling),
        MAX_PEER_MISBEHAVIOR_SCORE,
        "a sibling in the same /64 should report the same score"
    );
    assert_eq!(
        address_book.misbehavior_score(other_group),
        0,
        "an address in a different /64 should report no score"
    );
}

/// A ban is enforced right up to `BAN_DURATION`, lapses at exactly
/// `BAN_DURATION`, and the peer group is accepted again once it has.
#[tokio::test(start_paused = true)]
async fn bans_expire_after_the_ban_duration() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    let banned: crate::PeerSocketAddr = "[2001:db8::1]:8233".parse().unwrap();

    // A fresh ban is enforced.
    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });
    assert!(
        address_book.bans().is_banned(banned.ip()),
        "a fresh ban should be enforced"
    );
    address_book.update(gossiped_change(
        banned,
        PeerServices::NODE_NETWORK,
        DateTime32::MIN,
    ));
    assert!(
        address_book.get(banned).is_none(),
        "a banned peer should not be re-added to the address book"
    );

    // The ban is still enforced just before it lapses.
    tokio::time::advance(BAN_DURATION - Duration::from_nanos(1)).await;
    assert!(
        address_book.bans().is_banned(banned.ip()),
        "a ban should be enforced right up to BAN_DURATION"
    );

    // Then it lapses.
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert!(
        !address_book.bans().is_banned(banned.ip()),
        "a ban should lapse at exactly BAN_DURATION"
    );
    assert_eq!(
        address_book.misbehavior_score(banned),
        0,
        "a lapsed ban should no longer be reported as a score"
    );

    // The peer can be learned about again once its ban lapses.
    let now: DateTime32 = Utc::now().try_into().expect("will succeed until 2038");
    address_book.update(gossiped_change(banned, PeerServices::NODE_NETWORK, now));
    assert!(
        address_book.get(banned).is_some(),
        "a peer whose ban has lapsed should be accepted again"
    );
}

/// Applying a new ban prunes lapsed bans, so they don't occupy the
/// `MAX_BANNED_IPS` slots that active bans need.
#[tokio::test(start_paused = true)]
async fn applying_a_ban_prunes_lapsed_bans() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    let lapsed: crate::PeerSocketAddr = "[2001:db8:1::1]:8233".parse().unwrap();
    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: lapsed,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    tokio::time::advance(BAN_DURATION).await;
    assert!(
        !address_book.bans().is_banned(lapsed.ip()),
        "the ban should have lapsed"
    );
    assert_eq!(
        address_book.bans().len(),
        1,
        "the lapsed entry is still present until pruned"
    );

    // Banning another group prunes the lapsed entry.
    let fresh: crate::PeerSocketAddr = "[2001:db8:2::1]:8233".parse().unwrap();
    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: fresh,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    let bans = address_book.bans();
    assert!(
        !bans.is_banned("2001:db8:1::".parse::<IpAddr>().unwrap()),
        "the lapsed ban should have been pruned"
    );
    assert!(
        bans.is_banned("2001:db8:2::".parse::<IpAddr>().unwrap()),
        "the new ban should be present"
    );
    assert_eq!(bans.len(), 1, "only the new ban should remain");
}

/// Once a group's ban lapses, misbehaving again bans the whole group for a full
/// `BAN_DURATION`: the lapsed entry neither blocks nor shortens the new ban.
///
/// Changes from a banned group are rejected, so this is the only way a group
/// can be banned twice through the address book.
#[tokio::test(start_paused = true)]
async fn a_lapsed_group_can_be_banned_again() {
    let mut address_book = AddressBook::new(
        "0.0.0.0:0".parse().unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::current(),
    );

    let banned: crate::PeerSocketAddr = "[2001:db8::1]:8233".parse().unwrap();
    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: banned,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });
    assert!(address_book.bans().is_banned(banned.ip()));

    tokio::time::advance(BAN_DURATION).await;
    assert!(
        !address_book.bans().is_banned(banned.ip()),
        "the first ban should have lapsed"
    );

    // The group misbehaves again from a different address in the same /64.
    let sibling: crate::PeerSocketAddr = "[2001:db8::2]:8233".parse().unwrap();
    address_book.update(MetaAddrChange::UpdateMisbehavior {
        addr: sibling,
        score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
    });

    assert_eq!(
        address_book.bans().len(),
        1,
        "the sibling address must share the banned group's entry"
    );
    assert!(
        address_book.bans().is_banned(banned.ip()),
        "the new ban should cover the whole group"
    );

    // The new ban runs for a full BAN_DURATION from the re-ban.
    tokio::time::advance(BAN_DURATION - Duration::from_nanos(1)).await;
    assert!(
        address_book.bans().is_banned(banned.ip()),
        "the new ban should be enforced right up to its own deadline"
    );
    tokio::time::advance(Duration::from_nanos(1)).await;
    assert!(
        !address_book.bans().is_banned(banned.ip()),
        "the new ban should lapse BAN_DURATION after the re-ban"
    );
}
