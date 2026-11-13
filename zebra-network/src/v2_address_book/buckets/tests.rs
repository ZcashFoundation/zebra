//! Tests for the gossiped-address buckets.

use std::collections::HashSet;

use crate::PeerSocketAddr;

use super::{
    Admission, GossipBuckets, Group, Incumbent, V2_GOSSIP_BUCKET_SIZE, V2_SOURCE_GROUP_SPAN,
};

/// The bucket size, as a host number bound.
// The cast is safe: the bucket size is a small constant.
const BUCKET_SIZE: u16 = V2_GOSSIP_BUCKET_SIZE as u16;

/// An address in the IPv4 group `10.{group}`, distinguished by `host`.
fn addr(group: u8, host: u16) -> PeerSocketAddr {
    let [a, b] = host.to_be_bytes();
    format!("10.{group}.{a}.{b}:8233")
        .parse()
        .expect("valid address")
}

/// An address in the IPv4 group `{first}.{second}`.
fn addr_in(first: u8, second: u8, host: u8) -> PeerSocketAddr {
    format!("{first}.{second}.0.{host}:8233")
        .parse()
        .expect("valid address")
}

/// A relaying peer in the IPv4 group `172.{group}`.
fn source(group: u8) -> PeerSocketAddr {
    format!("172.{group}.0.1:8233")
        .parse()
        .expect("valid address")
}

/// An incumbent that is kept if `gossiped`, and gone otherwise.
fn kept_if(gossiped: bool) -> Incumbent {
    if gossiped {
        Incumbent::Kept
    } else {
        Incumbent::Gone
    }
}

/// An incumbent that is terrible if `gossiped`, and gone otherwise.
fn terrible_if(gossiped: bool) -> Incumbent {
    if gossiped {
        Incumbent::Terrible
    } else {
        Incumbent::Gone
    }
}

#[test]
fn one_group_from_one_source_is_bounded_at_one_bucket() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();
    let mut book: HashSet<PeerSocketAddr> = HashSet::new();

    // Gossip three buckets' worth of addresses from one /16 group, relayed by one source,
    // simulating the book alongside.
    for host in 0..(3 * BUCKET_SIZE) {
        let new = addr(1, host);
        match buckets.admit(new, source(1), |entry| terrible_if(book.contains(&entry))) {
            Admission::Admitted => {}
            Admission::Replaced(victim) => {
                assert!(book.remove(&victim), "victims are tracked entries")
            }
            Admission::Full => panic!("a terrible incumbent always gives up its slot"),
        }
        book.insert(new);
    }

    assert_eq!(
        book.len(),
        V2_GOSSIP_BUCKET_SIZE,
        "one group from one source is bounded at one bucket of gossiped entries",
    );
}

#[test]
fn one_source_group_reaches_at_most_its_span_of_buckets() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();
    let mut book: HashSet<PeerSocketAddr> = HashSet::new();

    // One source relays addresses from every IPv4 /16 group it likes.
    for first in 1..=40 {
        for second in 0..=255 {
            let new = addr_in(first, second, 1);
            match buckets.admit(new, source(1), |entry| terrible_if(book.contains(&entry))) {
                Admission::Admitted => {}
                Admission::Replaced(victim) => {
                    assert!(book.remove(&victim), "victims are tracked entries")
                }
                Admission::Full => panic!("a terrible incumbent always gives up its slot"),
            }
            book.insert(new);
        }
    }

    let used = buckets
        .buckets
        .iter()
        .filter(|bucket| !bucket.is_empty())
        .count();
    assert!(
        used <= V2_SOURCE_GROUP_SPAN,
        "one source group used {used} buckets, more than its span of {V2_SOURCE_GROUP_SPAN}",
    );
    assert!(
        book.len() <= V2_SOURCE_GROUP_SPAN * V2_GOSSIP_BUCKET_SIZE,
        "one source group holds {} entries, more than its span of buckets",
        book.len(),
    );

    // Another source group whose bucket the first one did not reach still finds room, without
    // evicting anything.
    let other = addr_in(41, 0, 1);
    let other_source = (2..=255)
        .map(source)
        .find(|other_source| buckets.buckets[buckets.bucket_index(&other, other_source)].is_empty())
        .expect("most buckets are out of the first source group's reach");
    assert_eq!(
        buckets.admit(other, other_source, |entry| kept_if(book.contains(&entry))),
        Admission::Admitted,
    );
}

#[test]
fn full_buckets_only_evict_tracked_relayed_entries() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();
    let mut relayed: HashSet<PeerSocketAddr> = HashSet::new();

    // Fill one bucket.
    for host in 0..BUCKET_SIZE {
        let new = addr(1, host);
        assert_eq!(
            buckets.admit(new, source(1), |_| Incumbent::Kept),
            Admission::Admitted,
        );
        relayed.insert(new);
    }

    // Half of the entries were tried since: they are no longer gossiped, so they are never
    // victims, however many new addresses arrive.
    let tried: HashSet<PeerSocketAddr> = relayed
        .iter()
        .copied()
        .take(V2_GOSSIP_BUCKET_SIZE / 2)
        .collect();
    relayed.retain(|entry| !tried.contains(entry));

    for host in 1000..1100 {
        let new = addr(1, host);
        match buckets.admit(new, source(1), |entry| {
            terrible_if(relayed.contains(&entry))
        }) {
            Admission::Admitted => {}
            Admission::Replaced(victim) => {
                assert!(!tried.contains(&victim), "tried entries are never victims");
                assert!(
                    relayed.remove(&victim),
                    "victims are tracked relayed entries"
                );
            }
            Admission::Full => panic!("a terrible incumbent always gives up its slot"),
        }
        relayed.insert(new);
    }
}

/// A full bucket keeps its entries: new addresses are dropped, however many arrive, until the
/// incumbent a new address is compared with is terrible.
#[test]
fn full_buckets_keep_incumbents_that_are_not_terrible() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();
    let incumbents: HashSet<PeerSocketAddr> = (0..BUCKET_SIZE).map(|host| addr(1, host)).collect();
    for incumbent in &incumbents {
        assert_eq!(
            buckets.admit(*incumbent, source(1), |_| Incumbent::Kept),
            Admission::Admitted,
        );
    }

    for host in 1000..2000 {
        assert_eq!(
            buckets.admit(addr(1, host), source(1), |_| Incumbent::Kept),
            Admission::Full,
            "a flood never replaces an incumbent that is worth keeping",
        );
    }
    let index = buckets.bucket_index(&addr(1, 0), &source(1));
    assert_eq!(
        buckets.buckets[index]
            .iter()
            .copied()
            .collect::<HashSet<_>>(),
        incumbents,
    );

    // One incumbent became terrible. Only the new addresses that are compared with it get in,
    // and that comparison is fixed by the secret, not by the order of arrival.
    let terrible = addr(1, 3);
    let state = |entry| {
        if entry == terrible {
            Incumbent::Terrible
        } else {
            Incumbent::Kept
        }
    };
    let outcomes: Vec<Admission> = (2000..2100)
        .map(|host| buckets.admit(addr(1, host), source(1), state))
        .collect();
    let replaced: Vec<&Admission> = outcomes
        .iter()
        .filter(|outcome| **outcome != Admission::Full)
        .collect();
    assert_eq!(
        replaced,
        [&Admission::Replaced(terrible)],
        "only the terrible incumbent is replaced, and only once",
    );
}

#[test]
fn the_secret_changes_placement() {
    let _init_guard = zebra_test::init();

    let first = GossipBuckets::with_secret([1; 32]);
    let second = GossipBuckets::with_secret([2; 32]);

    let placements = |buckets: &GossipBuckets| -> Vec<usize> {
        (0..64)
            .map(|group| buckets.bucket_index(&addr(group, 1), &source(1)))
            .collect()
    };
    assert_eq!(
        placements(&first),
        placements(&GossipBuckets::with_secret([1; 32])),
        "placement is deterministic for one secret",
    );
    assert_ne!(
        placements(&first),
        placements(&second),
        "another secret places the same addresses elsewhere",
    );
}

#[test]
fn entries_that_leave_the_gossiped_state_free_their_slots() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();

    // Fill one group's bucket.
    for host in 0..BUCKET_SIZE {
        let admission = buckets.admit(addr(1, host), source(1), |_| Incumbent::Kept);
        assert_eq!(admission, Admission::Admitted);
    }

    // Every tracked entry left the gossiped state (attempted, responded,
    // or evicted by the book): the next admission needs no victim.
    let admission = buckets.admit(addr(1, 999), source(1), |_| Incumbent::Gone);
    assert_eq!(
        admission,
        Admission::Admitted,
        "healed buckets admit without evicting anything",
    );
}

#[test]
fn readmission_removal_and_bans_are_idempotent() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();

    let re_gossiped = addr(1, 1);
    let kept = |_| Incumbent::Kept;
    assert_eq!(
        buckets.admit(re_gossiped, source(1), kept),
        Admission::Admitted
    );
    assert_eq!(
        buckets.admit(re_gossiped, source(1), kept),
        Admission::Admitted,
        "re-gossiped addresses keep their slot without eviction",
    );
    assert_eq!(buckets.len(), 1);

    // A ban purges the tracked entry; readmission then works.
    buckets.remove_if(|entry| entry == re_gossiped);
    assert_eq!(buckets.len(), 0);
    assert_eq!(
        buckets.admit(re_gossiped, source(1), |_| Incumbent::Gone),
        Admission::Admitted,
    );

    // So does a removal once the address is tried.
    buckets.remove(re_gossiped);
    buckets.remove(re_gossiped);
    assert_eq!(buckets.len(), 0);
}

#[test]
fn groups_are_ipv4_16_and_ipv6_32_prefixes() {
    let _init_guard = zebra_test::init();

    let group = |ip: &str| Group::of(ip.parse().expect("valid IP"));

    assert_eq!(group("10.1.2.3"), group("10.1.200.4"));
    assert_ne!(group("10.1.2.3"), group("10.2.2.3"));
    assert_eq!(group("2001:db8:1::1"), group("2001:db8:ffff::2"));
    assert_ne!(group("2001:db8::1"), group("2001:db9::1"));
    assert_ne!(group("32.1.13.184"), group("2001:db8::1"));
}
