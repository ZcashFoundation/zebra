//! Tests for the gossiped-address buckets.

use std::collections::HashSet;

use crate::PeerSocketAddr;

use super::{GossipBuckets, GOSSIP_BUCKET_SIZE};

/// An address in the group `group`, distinguished by `host`.
fn addr(group: u8, host: u16) -> PeerSocketAddr {
    let [a, b] = host.to_be_bytes();
    format!("10.{group}.{a}.{b}:8233")
        .parse()
        .expect("valid address")
}

#[test]
fn one_group_is_bounded_at_one_bucket() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();
    let mut book: HashSet<PeerSocketAddr> = HashSet::new();

    // Gossip three buckets' worth of addresses from one /16 group,
    // simulating the book alongside.
    for host in 0..(3 * GOSSIP_BUCKET_SIZE as u16) {
        let new = addr(1, host);
        let victim = buckets.admit(new, |entry| book.contains(&entry));
        if let Some(victim) = victim {
            assert!(book.remove(&victim), "victims are tracked entries");
        }
        book.insert(new);
    }

    assert_eq!(
        book.len(),
        GOSSIP_BUCKET_SIZE,
        "one group is bounded at one bucket of gossiped entries",
    );

    // A second group is admitted independently of the first group's
    // pressure.
    let other = addr(2, 1);
    assert_eq!(buckets.admit(other, |entry| book.contains(&entry)), None);
}

#[test]
fn entries_that_leave_the_gossiped_state_free_their_slots() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();

    // Fill one group's bucket.
    for host in 0..GOSSIP_BUCKET_SIZE as u16 {
        let victim = buckets.admit(addr(1, host), |_| true);
        assert_eq!(victim, None);
    }

    // Every tracked entry left the gossiped state (attempted, responded,
    // or evicted by the book): the next admission needs no victim.
    let victim = buckets.admit(addr(1, 999), |_| false);
    assert_eq!(
        victim, None,
        "healed buckets admit without evicting anything",
    );
}

#[test]
fn readmission_and_bans_are_idempotent() {
    let _init_guard = zebra_test::init();

    let mut buckets = GossipBuckets::new();

    let re_gossiped = addr(1, 1);
    assert_eq!(buckets.admit(re_gossiped, |_| true), None);
    assert_eq!(
        buckets.admit(re_gossiped, |_| true),
        None,
        "re-gossiped addresses keep their slot without eviction",
    );

    // A ban purges the tracked entry; readmission then works.
    buckets.remove_if(|entry| entry == re_gossiped);
    assert_eq!(buckets.admit(re_gossiped, |_| false), None);
}
