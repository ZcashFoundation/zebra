//! Fixed test vectors for the version 2 address segment.

use std::{
    collections::HashSet,
    net::{Ipv4Addr, SocketAddr},
    time::{Duration, Instant},
};

use tokio::sync::watch;

use zebra_chain::{
    parameters::Network,
    serialization::{DateTime32, Duration32},
};

use crate::{
    meta_addr::MetaAddr,
    protocol::{external::types::PeerServices, v2::announcement::clamp_relayed_time},
    BanList, CacheDir, Config, PeerAddrState, PeerSocketAddr,
};

use super::super::{
    actor::{
        V2Book, GET_ADDR_SNAPSHOT_BASE_INTERVAL, GET_ADDR_SNAPSHOT_JITTER,
        GET_ADDR_SNAPSHOT_MIN_ADDRS, V2_MAX_TRIED,
    },
    buckets::{Group, V2_GOSSIP_BUCKET_SIZE, V2_GOSSIP_CAPACITY, V2_SOURCE_GROUP_SPAN},
    cache::v2_peer_cache_updater_with,
    intake::Rejection,
    load_v2_peer_cache, spawn, Provenance, Via, V2_DNS_RETRY_FAILED_AFTER,
};

/// A routable address for this node's own listener.
const OWN_ADDR: &str = "198.51.100.1:8233";

/// Returns a Mainnet segment that advertises `local_listener`.
fn segment(local_listener: &str) -> V2Book {
    let (stats_tx, _stats_rx) = watch::channel(Default::default());
    V2Book::new(
        &Network::Mainnet,
        local_listener.parse().expect("valid address"),
        false,
        1,
        BanList::default(),
        stats_tx,
    )
}

/// Parses a test address.
fn addr(addr: &str) -> PeerSocketAddr {
    addr.parse().expect("valid address")
}

/// An address in the IPv4 group `203.{group}`.
fn addr_in(group: u8, host: u8) -> PeerSocketAddr {
    SocketAddr::new(Ipv4Addr::new(203, group, 0, host).into(), 8233).into()
}

/// A relayed record for `addr` with `time`.
fn record(addr: PeerSocketAddr, time: DateTime32) -> MetaAddr {
    MetaAddr::new_gossiped_meta_addr(addr, PeerServices::NODE_NETWORK, time)
}

/// The relaying peer of most tests.
fn source() -> PeerSocketAddr {
    addr("192.0.2.1:8233")
}

/// Relays fresh records for `addrs` from [`source`].
fn relay(book: &mut V2Book, addrs: &[PeerSocketAddr]) {
    let now = DateTime32::now();
    let records = addrs.iter().map(|addr| record(*addr, now)).collect();
    book.intake_at(source(), records, Via::GetAddr, now);
}

/// Makes `addr` a tried entry: attempted, then responded.
fn make_tried(book: &mut V2Book, addr: PeerSocketAddr) {
    book.change(MetaAddr::new_reconnect(addr), Instant::now());
    book.change(MetaAddr::new_responded(addr, None), Instant::now());
}

/// `count` relayable addresses, each in its own address group, from the group `203.{first}`.
fn relayed_addrs(first: usize, count: usize) -> Vec<PeerSocketAddr> {
    (first..first + count)
        .map(|group| {
            addr_in(
                u8::try_from(group).expect("tests use fewer than 256 groups"),
                1,
            )
        })
        .collect()
}

/// The connection state of `addr`, if the segment has it.
fn state(book: &mut V2Book, addr: PeerSocketAddr) -> Option<PeerAddrState> {
    book.book.get(addr).map(|entry| entry.last_connection_state)
}

/// The addresses of `entries`.
fn addrs_of(entries: Vec<MetaAddr>) -> HashSet<PeerSocketAddr> {
    entries.into_iter().map(|entry| entry.addr).collect()
}

#[test]
fn clamp_relayed_time_follows_the_draft() {
    let _init_guard = zebra_test::init();

    let now = DateTime32::now();
    let five_days_ago = now.saturating_sub(Duration32::from_days(5));

    assert_eq!(clamp_relayed_time(DateTime32::MIN, now), five_days_ago);
    assert_eq!(
        clamp_relayed_time(now.saturating_add(Duration32::from_minutes(11)), now),
        five_days_ago,
    );
    let soon = now.saturating_add(Duration32::from_minutes(9));
    assert_eq!(clamp_relayed_time(soon, now), soon);
    let old = now.saturating_sub(Duration32::from_days(40));
    assert_eq!(clamp_relayed_time(old, now), old, "old times are kept");
}

#[test]
fn intake_clamps_bad_times_and_drops_stale_records() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let now = DateTime32::now();
    let five_days_ago = now.saturating_sub(Duration32::from_days(5));

    let [zero, future, fresh, stale] = [1, 2, 3, 4].map(|group| addr_in(group, 1));
    let outcomes = book.intake_at(
        source(),
        vec![
            record(zero, DateTime32::MIN),
            record(future, now.saturating_add(Duration32::from_minutes(11))),
            record(fresh, now),
            record(stale, now.saturating_sub(Duration32::from_days(31))),
        ],
        Via::GetAddr,
        now,
    );
    assert_eq!(
        outcomes,
        [
            Some(Rejection::Clamped),
            Some(Rejection::Clamped),
            None,
            Some(Rejection::Stale)
        ],
    );

    for clamped in [zero, future] {
        let entry = book.book.get(clamped).expect("clamped records are kept");
        assert_eq!(entry.untrusted_last_seen(), Some(five_days_ago));
    }
    assert!(book.book.get(fresh).is_some());
    assert!(book.book.get(stale).is_none(), "stale records are dropped");
}

#[test]
fn intake_drops_banned_own_and_unroutable_addresses() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let banned = addr_in(1, 1);
    let mut bans = BanList::default();
    bans.ban(banned.ip());
    book.apply_bans(bans);

    let now = DateTime32::now();
    let not_a_node = MetaAddr::new_gossiped_meta_addr(addr_in(2, 1), PeerServices::empty(), now);
    let outcomes = book.intake_at(
        source(),
        vec![
            record(banned, now),
            record(addr(OWN_ADDR), now),
            record(addr("0.0.0.0:8233"), now),
            record(addr("203.0.113.9:0"), now),
            // The Testnet port, on Mainnet.
            record(addr("203.0.113.9:18232"), now),
            not_a_node,
        ],
        Via::Announcement,
        now,
    );
    assert_eq!(
        outcomes,
        [
            Some(Rejection::Banned),
            Some(Rejection::OwnAddress),
            Some(Rejection::Unroutable),
            Some(Rejection::Unroutable),
            Some(Rejection::Unroutable),
            Some(Rejection::NotANode),
        ],
    );
    assert_eq!(book.book.len(), 0);
    assert_eq!(
        book.buckets.len(),
        0,
        "rejected records take no bucket slot"
    );
}

#[test]
fn intake_does_not_reset_tried_entries() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let tried = addr_in(1, 1);
    relay(&mut book, &[tried]);
    make_tried(&mut book, tried);
    assert_eq!(state(&mut book, tried), Some(PeerAddrState::Responded));
    assert_eq!(book.buckets.len(), 0, "tried entries leave the buckets");

    relay(&mut book, &[tried]);
    assert_eq!(state(&mut book, tried), Some(PeerAddrState::Responded));
    assert_eq!(book.buckets.len(), 0);
}

#[test]
fn a_relay_flood_never_evicts_tried_or_trusted_entries() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let configured = addr("192.0.2.10:8233");
    let dns_seed = addr("192.0.2.11:8233");
    let cached = addr("192.0.2.12:8233");
    book.seed(vec![configured], Provenance::Configured, Instant::now());
    book.seed(vec![dns_seed], Provenance::DnsSeed, Instant::now());
    book.seed(vec![cached], Provenance::Cache, Instant::now());

    // A tried entry, first learned through relay.
    let tried = addr_in(1, 1);
    relay(&mut book, &[tried]);
    make_tried(&mut book, tried);

    // 10,000 relayed records from one source, in many groups, some in the protected entries'
    // groups.
    let now = DateTime32::now();
    let flood: Vec<MetaAddr> = (0..10_000_u32)
        .map(|index| {
            let [_, a, b, c] = index.to_be_bytes();
            let ip = if index % 10 == 0 {
                Ipv4Addr::new(192, 0, b, c.max(20))
            } else {
                Ipv4Addr::new(100, a.wrapping_add(64), b, c)
            };
            record(SocketAddr::new(ip.into(), 8233).into(), now)
        })
        .collect();
    book.intake_at(source(), flood, Via::Announcement, now);

    for protected in [configured, dns_seed, cached, tried] {
        assert!(
            book.book.get(protected).is_some(),
            "{protected:?} must survive the flood",
        );
    }
    let stats = book.stats();
    assert_eq!(stats.trusted, 3);
    assert_eq!(stats.tried, 1);
    assert!(
        stats.relayed <= V2_SOURCE_GROUP_SPAN * V2_GOSSIP_BUCKET_SIZE,
        "one source group fills at most its span of the buckets: {stats:?}",
    );
    assert!(stats.relayed < V2_GOSSIP_CAPACITY);
    assert_eq!(stats.relayed, book.buckets.len());
}

#[test]
fn bans_remove_entries_from_the_book_buckets_and_trusted_entries() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let relayed = addr_in(1, 1);
    let tried = addr_in(2, 1);
    let trusted = addr("192.0.2.10:8233");
    let unbanned = addr_in(3, 1);
    relay(&mut book, &[relayed, tried, unbanned]);
    make_tried(&mut book, tried);
    book.seed(vec![trusted], Provenance::Configured, Instant::now());

    let mut bans = BanList::default();
    for banned in [relayed, tried, trusted] {
        bans.ban(banned.ip());
    }
    book.apply_bans(bans);

    for banned in [relayed, tried, trusted] {
        assert!(book.book.get(banned).is_none(), "{banned:?} is removed");
    }
    assert!(book.trusted.is_empty());
    assert_eq!(book.buckets.len(), 1);
    assert!(book.book.get(unbanned).is_some());

    // Banned addresses do not come back through any message.
    relay(&mut book, &[relayed]);
    book.seed(vec![trusted], Provenance::Configured, Instant::now());
    book.change(MetaAddr::new_reconnect(tried), Instant::now());
    assert_eq!(book.book.len(), 1);
}

#[test]
fn next_candidate_skips_excluded_busy_and_pending_addresses() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let excluded = addr_in(1, 1);
    let busy = addr_in(2, 1);
    let ipv6 = addr("[2001:db8::1]:8233");
    let free = addr_in(3, 1);
    relay(&mut book, &[excluded, busy, ipv6, free]);
    book.ipv4_only = true;

    let exclude: HashSet<_> = [excluded].into();
    let busy_groups: HashSet<_> = [Group::of(busy.ip())].into();

    let candidate = book
        .next_candidate(&exclude, &busy_groups)
        .expect("one candidate is left");
    assert_eq!(candidate.addr, free);
    assert_eq!(
        state(&mut book, free),
        Some(PeerAddrState::NeverAttemptedGossiped),
        "the dialer marks the candidate when it dials it",
    );
    book.change(MetaAddr::new_reconnect(free), Instant::now());

    assert_eq!(
        book.next_candidate(&exclude, &busy_groups),
        None,
        "pending, excluded, busy, and unreachable addresses are not candidates",
    );
    book.ipv4_only = false;
    assert_eq!(
        book.next_candidate(&exclude, &busy_groups)
            .map(|candidate| candidate.addr),
        Some(ipv6),
        "an IPv6 endpoint reaches IPv6 addresses",
    );

    // Banned addresses are never candidates.
    let mut bans = BanList::default();
    bans.ban(excluded.ip());
    book.apply_bans(bans);
    assert_eq!(
        book.next_candidate(&HashSet::new(), &[Group::of(ipv6.ip())].into())
            .map(|candidate| candidate.addr),
        Some(busy),
    );
    book.change(MetaAddr::new_reconnect(busy), Instant::now());
    book.change(MetaAddr::new_reconnect(ipv6), Instant::now());
    assert_eq!(book.next_candidate(&HashSet::new(), &HashSet::new()), None);
}

#[test]
fn stats_count_unreachable_addresses_as_not_dialable() {
    let _init_guard = zebra_test::init();

    let mut book = segment("0.0.0.0:8233");
    let ipv4 = addr_in(1, 1);
    let ipv6 = addr("[2001:db8::1]:8233");
    relay(&mut book, &[ipv4, ipv6]);
    assert_eq!(book.stats().dialable, 2);

    // An IPv4 endpoint never dials the IPv6 entry, so the DNS reseed check must not count it.
    book.ipv4_only = true;
    assert_eq!(book.stats().dialable, 1);
    assert_eq!(book.stats().relayed, 2, "the entry is still kept");
}

/// Returns how often `expected` is the first candidate of `trials` segments made by `make`.
fn first_candidate_share(trials: u32, expected: PeerSocketAddr, make: impl Fn() -> V2Book) -> f64 {
    let chosen = (0..trials)
        .filter(|_| {
            make()
                .next_candidate(&HashSet::new(), &HashSet::new())
                .map(|candidate| candidate.addr)
                == Some(expected)
        })
        .count();
    // The casts are safe: the counts are tiny.
    chosen as f64 / f64::from(trials)
}

/// A relayed record with a `time` just under the clamping limit is chosen no more often than any
/// other relayed record: relayed times do not rank candidates.
#[test]
fn next_candidate_ignores_relayed_times() {
    let _init_guard = zebra_test::init();

    let (old, skewed) = (addr_in(1, 1), addr_in(2, 1));
    let share = first_candidate_share(400, skewed, || {
        let mut book = segment(OWN_ADDR);
        let now = DateTime32::now();
        let records = vec![
            record(old, now.saturating_sub(Duration32::from_days(1))),
            record(skewed, now.saturating_add(Duration32::from_minutes(9))),
        ];
        let outcomes = book.intake_at(source(), records, Via::GetAddr, now);
        assert_eq!(outcomes, [None, None], "both records are kept as relayed");
        book
    });

    // The share is 0.5 with a standard deviation of 0.025, so this never fails by chance.
    assert!(
        (0.3..=0.7).contains(&share),
        "the future-skewed record is chosen first {share} of the time",
    );
}

/// Trusted entries get at least even odds against any number of relayed entries, which carry
/// peer-chosen times and services.
#[test]
fn next_candidate_gives_trusted_entries_even_odds_against_relay() {
    let _init_guard = zebra_test::init();

    let seed = addr("192.0.2.50:8233");
    let share = first_candidate_share(400, seed, || {
        let mut book = segment(OWN_ADDR);
        let now = DateTime32::now();
        let skewed = now.saturating_add(Duration32::from_minutes(9));
        let records = relayed_addrs(1, 20)
            .into_iter()
            .map(|addr| record(addr, skewed))
            .collect();
        book.intake_at(source(), records, Via::GetAddr, now);
        book.seed(vec![seed], Provenance::DnsSeed, Instant::now());
        book
    });

    // The share is 0.5 with a standard deviation of 0.025, so this never fails by chance.
    assert!(
        (0.3..=0.7).contains(&share),
        "the DNS seed is chosen first {share} of the time",
    );
}

/// An entry this node connected to is a candidate again once its reconnection delay has passed
/// after it disconnected, and is chosen ahead of trusted and relayed entries that were never
/// attempted.
#[test]
fn next_candidate_prefers_entries_that_answered() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let answered = addr_in(1, 1);
    let unanswered = addr_in(2, 1);
    relay(&mut book, &[answered, unanswered]);
    relay(&mut book, &relayed_addrs(10, 20));
    book.seed(
        vec![addr("192.0.2.50:8233")],
        Provenance::DnsSeed,
        Instant::now(),
    );
    make_tried(&mut book, answered);
    book.change(MetaAddr::new_shutdown(answered), Instant::now());
    book.change(MetaAddr::new_reconnect(unanswered), Instant::now());
    book.change(MetaAddr::new_errored(unanswered, None), Instant::now());

    let none = HashSet::new();
    let candidate = |book: &V2Book, after: Duration| {
        book.next_candidate_at(
            &none,
            &HashSet::new(),
            Instant::now() + after,
            chrono::Utc::now() + after,
        )
        .map(|candidate| candidate.addr)
    };
    assert_ne!(
        candidate(&book, Duration::ZERO),
        Some(answered),
        "the entry waits for its reconnection delay",
    );
    let after_delay = crate::constants::MIN_PEER_RECONNECTION_DELAY + Duration::from_secs(1);
    for _ in 0..20 {
        assert_eq!(candidate(&book, after_delay), Some(answered));
    }
}

/// Only the entries this node connected to are cached, most recently answered first, so relayed
/// records never become trusted entries after a restart, whatever `time` they claim.
#[test]
fn cacheable_entries_are_the_ones_that_answered() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let (tried, relayed, failed) = (addr_in(1, 1), addr_in(2, 1), addr_in(3, 1));
    let now = DateTime32::now();
    book.intake_at(
        source(),
        vec![
            record(tried, now),
            record(relayed, now.saturating_add(Duration32::from_minutes(9))),
            record(failed, now),
        ],
        Via::GetAddr,
        now,
    );
    let seed = addr("192.0.2.50:8233");
    book.seed(vec![seed], Provenance::DnsSeed, Instant::now());
    make_tried(&mut book, tried);
    book.change(MetaAddr::new_shutdown(tried), Instant::now());
    book.change(MetaAddr::new_reconnect(failed), Instant::now());
    book.change(MetaAddr::new_errored(failed, None), Instant::now());

    assert_eq!(
        addrs_of(book.cacheable(chrono::Utc::now())),
        [tried].into(),
        "relayed, seeded, and unanswered entries are not cached",
    );
}

#[test]
fn get_addr_sample_serves_tried_relayed_and_own_addresses_only() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let configured = addr("192.0.2.10:8233");
    let dns_seed = addr("192.0.2.11:8233");
    book.seed(vec![configured], Provenance::Configured, Instant::now());
    book.seed(vec![dns_seed], Provenance::DnsSeed, Instant::now());
    let relayed = addr_in(1, 1);
    let tried = addr_in(2, 1);
    relay(&mut book, &[relayed, tried]);
    make_tried(&mut book, tried);

    let sample = addrs_of(book.get_addr_sample(tokio::time::Instant::now()));
    assert_eq!(
        sample,
        [relayed, tried, addr(OWN_ADDR)].into(),
        "never-attempted configured and DNS seed entries are not served",
    );

    // An unspecified listener is never served.
    let mut book = segment("0.0.0.0:8233");
    relay(&mut book, &[relayed]);
    let sample = addrs_of(book.get_addr_sample(tokio::time::Instant::now()));
    assert_eq!(sample, [relayed].into());
}

#[test]
fn get_addr_sample_rotates_after_its_interval() {
    let _init_guard = zebra_test::init();

    let mut book = segment("0.0.0.0:8233");
    let start = tokio::time::Instant::now();
    assert!(
        book.get_addr_sample(start).is_empty(),
        "nothing to serve yet"
    );

    let first = relayed_addrs(1, GET_ADDR_SNAPSHOT_MIN_ADDRS);
    relay(&mut book, &first);
    assert_eq!(
        addrs_of(book.get_addr_sample(start)),
        first.iter().copied().collect(),
        "an empty snapshot is never kept"
    );

    let second = addr_in(200, 1);
    relay(&mut book, &[second]);
    assert_eq!(
        addrs_of(book.get_addr_sample(start + GET_ADDR_SNAPSHOT_BASE_INTERVAL / 2)),
        first.iter().copied().collect(),
        "the snapshot is kept for its interval",
    );
    assert_eq!(
        addrs_of(
            book.get_addr_sample(
                start + GET_ADDR_SNAPSHOT_BASE_INTERVAL + GET_ADDR_SNAPSHOT_JITTER
            )
        ),
        first.iter().copied().chain([second]).collect(),
        "a new snapshot is drawn once the interval and its jitter passed",
    );
}

#[test]
fn get_addr_sample_is_drawn_again_while_it_is_small() {
    let _init_guard = zebra_test::init();

    // A node with a routable address always serves it, so its first snapshot after a start is
    // not empty.
    let mut book = segment(OWN_ADDR);
    let own = addr(OWN_ADDR);
    let start = tokio::time::Instant::now();
    assert_eq!(addrs_of(book.get_addr_sample(start)), [own].into());

    let first = addr_in(1, 1);
    relay(&mut book, &[first]);
    assert_eq!(
        addrs_of(book.get_addr_sample(start)),
        [own, first].into(),
        "a snapshot of only this node's own address is not kept",
    );

    let rest = relayed_addrs(2, GET_ADDR_SNAPSHOT_MIN_ADDRS - 1);
    relay(&mut book, &rest);
    let full: HashSet<PeerSocketAddr> = [own, first].into_iter().chain(rest).collect();
    assert_eq!(
        addrs_of(book.get_addr_sample(start)),
        full,
        "a snapshot with too few other addresses is not kept",
    );

    relay(&mut book, &[addr_in(200, 1)]);
    assert_eq!(
        addrs_of(book.get_addr_sample(start)),
        full,
        "a snapshot with enough other addresses is kept for its interval",
    );
}

#[test]
fn seeds_skip_known_entries_unless_they_failed_long_ago() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let seeded = addr("192.0.2.11:8233");
    book.seed(vec![seeded], Provenance::DnsSeed, Instant::now());
    assert_eq!(
        state(&mut book, seeded),
        Some(PeerAddrState::NeverAttemptedGossiped)
    );
    book.change(MetaAddr::new_reconnect(seeded), Instant::now());
    book.change(MetaAddr::new_errored(seeded, None), Instant::now());
    assert_eq!(state(&mut book, seeded), Some(PeerAddrState::Failed));

    // A reseed less than a day after the failure leaves the entry failed, so it is not
    // re-probed.
    book.seed(vec![seeded], Provenance::DnsSeed, Instant::now());
    assert_eq!(state(&mut book, seeded), Some(PeerAddrState::Failed));
    book.seed(
        vec![seeded],
        Provenance::DnsSeed,
        Instant::now() + V2_DNS_RETRY_FAILED_AFTER - Duration::from_secs(60),
    );
    assert_eq!(state(&mut book, seeded), Some(PeerAddrState::Failed));

    // A day later, the reseed makes it a candidate again.
    book.seed(
        vec![seeded],
        Provenance::DnsSeed,
        Instant::now() + V2_DNS_RETRY_FAILED_AFTER + Duration::from_secs(60),
    );
    assert_ne!(state(&mut book, seeded), Some(PeerAddrState::Failed));
    assert_eq!(book.trusted.get(&seeded), Some(&Provenance::DnsSeed));

    // A configured seed of a relayed address makes it trusted, out of the buckets.
    let relayed = addr_in(1, 1);
    relay(&mut book, &[relayed]);
    assert_eq!(book.buckets.len(), 1);
    book.seed(vec![relayed], Provenance::Configured, Instant::now());
    assert_eq!(book.buckets.len(), 0);
    assert_eq!(book.trusted.get(&relayed), Some(&Provenance::Configured));
}

/// The version 2 peer cache round-trips through `<network>.v2-peers`, and leaves the legacy peer
/// cache alone.
#[tokio::test]
async fn v2_peer_cache_round_trips_without_touching_the_legacy_cache() {
    let _init_guard = zebra_test::init();

    let cache_dir = tempfile::tempdir().expect("a temporary directory");
    let config = Config {
        cache_dir: CacheDir::custom_path(cache_dir.path()),
        ..Config::default()
    };
    let legacy_peers: HashSet<PeerSocketAddr> = [addr("127.1.1.1:8233")].into();
    config
        .update_peer_cache(legacy_peers.clone())
        .await
        .expect("the legacy cache is written");

    let (_bans_tx, bans_rx) = watch::channel(BanList::default());
    let unspecified: SocketAddr = "0.0.0.0:8233".parse().expect("valid address");
    let (handle, _stats, _task) = spawn(
        &config,
        &config.network,
        unspecified,
        false,
        bans_rx.clone(),
    );
    let tried: HashSet<PeerSocketAddr> = [addr_in(1, 1), addr_in(2, 1)].into();
    let relayed_only = addr_in(3, 1);
    handle.intake(
        source(),
        tried
            .iter()
            .chain([&relayed_only])
            .map(|addr| record(*addr, DateTime32::now()))
            .collect(),
        Via::GetAddr,
    );
    for addr in &tried {
        handle.change(MetaAddr::new_reconnect(*addr)).await;
        handle.change(MetaAddr::new_responded(*addr, None)).await;
    }

    let updater = tokio::spawn(v2_peer_cache_updater_with(
        config.clone(),
        handle.clone(),
        Duration::from_millis(10),
    ));
    let v2_path = config
        .cache_dir
        .v2_peer_cache_file_path(&config.network)
        .expect("the cache is enabled");
    assert_eq!(
        v2_path.file_name().and_then(|name| name.to_str()),
        Some("mainnet.v2-peers")
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while !v2_path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the v2 peer cache is written");
    updater.abort();

    assert_eq!(
        config.load_peer_cache_at(&v2_path).await.expect("readable"),
        tried,
        "only the peers this node connected to are cached",
    );
    assert_eq!(
        config.load_peer_cache().await.expect("readable"),
        legacy_peers,
        "the legacy peer cache is untouched",
    );

    // A fresh segment loads the cache as trusted entries.
    let (handle, stats, _task) = spawn(&config, &config.network, unspecified, false, bans_rx);
    assert_eq!(load_v2_peer_cache(&config, &handle).await, tried.len());
    // A reply comes after the seed was applied, and the statistics were published before it.
    let _ = handle.cacheable().await;
    assert_eq!(stats.borrow().trusted, tried.len());
}

/// Relayed entries that were dialled and failed leave the buckets, so their number is bounded
/// separately: the entries that failed longest ago are removed.
#[test]
fn failed_tried_entries_are_bounded() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let configured = addr("192.0.2.10:8233");
    book.seed(vec![configured], Provenance::Configured, Instant::now());
    book.change(MetaAddr::new_reconnect(configured), Instant::now());
    book.change(MetaAddr::new_errored(configured, None), Instant::now());

    let now = DateTime32::now();
    for round in 0..4_u8 {
        // Each round's records come from many sources, so most of them find a bucket slot.
        for source_group in 0..100_u8 {
            let source: PeerSocketAddr =
                SocketAddr::new(Ipv4Addr::new(172, source_group, round, 1).into(), 8233).into();
            let records = (0..5_u8)
                .map(|host| {
                    let ip = Ipv4Addr::new(
                        100,
                        round.wrapping_mul(40).wrapping_add(source_group / 3),
                        source_group,
                        host + 1,
                    );
                    record(SocketAddr::new(ip.into(), 8233).into(), now)
                })
                .collect();
            book.intake_at(source, records, Via::GetAddr, now);
        }
        let relayed: Vec<PeerSocketAddr> = book
            .book
            .peers()
            .filter(|peer| peer.last_connection_state == PeerAddrState::NeverAttemptedGossiped)
            .map(|peer| peer.addr)
            .filter(|addr| *addr != configured)
            .collect();
        for addr in relayed {
            book.change(MetaAddr::new_reconnect(addr), Instant::now());
            book.change(MetaAddr::new_errored(addr, None), Instant::now());
        }
        let stats = book.stats();
        assert!(stats.tried <= V2_MAX_TRIED, "round {round}: {stats:?}");
        book.publish_stats();
    }

    assert!(
        book.stats().tried > V2_MAX_TRIED / 2,
        "the test fails enough entries"
    );
    assert!(
        book.book.get(configured).is_some(),
        "trusted entries are never removed for the bound"
    );
}

/// The segment never refreshes the address book metrics, which describe the legacy address book
/// and would make every relayed record walk the whole segment.
#[test]
fn the_segment_never_reports_the_legacy_address_metrics() {
    let _init_guard = zebra_test::init();

    let mut book = segment(OWN_ADDR);
    let metrics = book.book.address_metrics_watcher();
    let tried = addr_in(1, 1);
    relay(&mut book, &relayed_addrs(1, 10));
    make_tried(&mut book, tried);
    book.book.remove(addr_in(2, 1));

    assert_eq!(
        *metrics.borrow(),
        Default::default(),
        "the metrics are never refreshed"
    );
    assert!(!metrics.has_changed().expect("the book is alive"));
}
