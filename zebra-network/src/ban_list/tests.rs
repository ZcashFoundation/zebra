//! Fixed test vectors for the ban list.
//!
//! Tests that depend on bans lapsing run under a paused tokio clock and advance
//! it explicitly, so they can check the exact ban boundary without waiting.

use std::{
    net::{IpAddr, Ipv4Addr},
    time::Duration,
};

use tokio::time::advance;

use crate::{
    ban_list::BanList,
    constants::{BAN_DURATION, MAX_BANNED_IPS},
};

/// Banning an IPv6 address bans its whole `/64`, and banning an IPv4 address
/// bans only that address.
#[test]
fn bans_apply_to_the_peer_group() {
    let _init_guard = zebra_test::init();

    let mut bans = BanList::default();

    let banned_v6: IpAddr = "2001:db8::1".parse().unwrap();
    bans.ban(banned_v6);

    assert!(bans.is_banned(banned_v6), "the banned address is banned");
    assert!(
        bans.is_banned("2001:db8::2".parse().unwrap()),
        "another address in the banned /64 is banned"
    );
    assert!(
        !bans.is_banned("2001:db9::1".parse().unwrap()),
        "an address in a different /64 is not banned"
    );

    let banned_v4: IpAddr = "192.0.2.10".parse().unwrap();
    bans.ban(banned_v4);

    assert!(
        bans.is_banned(banned_v4),
        "the banned IPv4 address is banned"
    );
    assert!(
        !bans.is_banned("192.0.2.11".parse().unwrap()),
        "a neighbouring IPv4 address in the same /24 is not banned"
    );
}

/// An IPv4-mapped IPv6 address is banned by its plain IPv4 ban, and vice versa,
/// so a peer cannot dodge a ban by switching spellings on a dual-stack listener.
#[test]
fn bans_cover_both_ipv4_spellings() {
    let _init_guard = zebra_test::init();

    let plain: IpAddr = "192.0.2.10".parse().unwrap();
    let mapped: IpAddr = "::ffff:192.0.2.10".parse().unwrap();

    let mut bans = BanList::default();
    bans.ban(plain);
    assert!(
        bans.is_banned(mapped),
        "the IPv4-mapped spelling should be covered by the plain IPv4 ban"
    );

    let mut bans = BanList::default();
    bans.ban(mapped);
    assert!(
        bans.is_banned(plain),
        "the plain spelling should be covered by the IPv4-mapped ban"
    );
}

/// Bans are enforced right up to `BAN_DURATION`, lapse at exactly
/// `BAN_DURATION`, and a lapsed ban is pruned when another ban is applied.
#[tokio::test(start_paused = true)]
async fn bans_lapse_and_are_pruned() {
    let _init_guard = zebra_test::init();

    let mut bans = BanList::default();
    let lapsed: IpAddr = "2001:db8::1".parse().unwrap();
    bans.ban(lapsed);

    advance(BAN_DURATION - Duration::from_nanos(1)).await;
    assert!(
        bans.is_banned(lapsed),
        "a ban should be enforced right up to BAN_DURATION"
    );

    advance(Duration::from_nanos(1)).await;
    assert!(
        !bans.is_banned(lapsed),
        "a ban should lapse at exactly BAN_DURATION"
    );
    assert_eq!(
        bans.len(),
        1,
        "the lapsed entry is still present until pruned"
    );

    // Applying another ban prunes the lapsed one.
    let active: IpAddr = "2001:db8:1::1".parse().unwrap();
    bans.ban(active);
    assert!(bans.is_banned(active), "the new ban is enforced");
    assert_eq!(bans.len(), 1, "the lapsed ban should have been pruned");
    assert!(!bans.is_banned(lapsed));
}

/// Re-banning a group refreshes its ban time, so the ban runs for another full
/// `BAN_DURATION` from the re-ban rather than lapsing on its original schedule.
#[tokio::test(start_paused = true)]
async fn re_banning_refreshes_the_ban() {
    let _init_guard = zebra_test::init();

    let mut bans = BanList::default();
    let banned: IpAddr = "2001:db8::1".parse().unwrap();
    bans.ban(banned);

    // One minute before the ban lapses, re-ban from a sibling address in the
    // same /64.
    let remaining = Duration::from_secs(60);
    advance(BAN_DURATION - remaining).await;
    bans.ban("2001:db8::2".parse().unwrap());

    assert_eq!(bans.len(), 1, "the sibling shares the group's entry");
    assert!(bans.is_banned(banned), "the refreshed ban is enforced");
    assert!(
        bans.is_banned("2001:db8::3".parse().unwrap()),
        "every address in the group stays banned"
    );

    // Go one nanosecond past the original ban's deadline, rather than stopping
    // exactly on it: the original ban would have lapsed by now, but the refresh
    // keeps it.
    let time_after_first_lapse = Duration::from_nanos(1);
    advance(remaining + time_after_first_lapse).await;
    assert!(
        bans.is_banned(banned),
        "a refreshed ban must outlive the original ban's deadline"
    );

    // The refreshed ban lapses a full BAN_DURATION after the re-ban.
    advance(BAN_DURATION - remaining - time_after_first_lapse - Duration::from_nanos(1)).await;
    assert!(
        bans.is_banned(banned),
        "the refreshed ban should be enforced right up to its new deadline"
    );
    advance(Duration::from_nanos(1) + time_after_first_lapse).await;
    assert!(
        !bans.is_banned(banned),
        "the refreshed ban should lapse BAN_DURATION after the re-ban"
    );
}

/// A snapshot taken before a ban lapses stops enforcing it once it does, so
/// holders of a snapshot don't need to be sent a new one.
#[tokio::test(start_paused = true)]
async fn snapshots_stop_enforcing_lapsed_bans() {
    let _init_guard = zebra_test::init();

    let mut bans = BanList::default();
    let banned: IpAddr = "2001:db8::1".parse().unwrap();
    bans.ban(banned);

    let snapshot = bans.clone();
    assert!(
        snapshot.is_banned(banned),
        "the snapshot enforces the active ban"
    );

    advance(BAN_DURATION).await;
    assert!(
        !snapshot.is_banned(banned),
        "a snapshot must not enforce a ban that has lapsed"
    );
    assert_eq!(
        snapshot.len(),
        1,
        "the snapshot still holds the entry: it checks the age on each query \
         rather than being pruned"
    );
}

/// When the list is full, applying a ban evicts the oldest ban, and a refreshed
/// ban counts as new rather than keeping its original age.
#[tokio::test(start_paused = true)]
async fn a_full_ban_list_evicts_the_oldest_ban() {
    let _init_guard = zebra_test::init();

    let mut bans = BanList::default();

    let time_between_bans = Duration::from_nanos(1) + Duration::from_nanos(1);

    let oldest: IpAddr = "192.0.2.1".parse().unwrap();
    bans.ban(oldest);

    // Banned second, so it would be evicted second if it kept its original age.
    let refreshed: IpAddr = "192.0.2.2".parse().unwrap();
    advance(time_between_bans).await;
    bans.ban(refreshed);

    // Fill the remaining slots with distinct IPv4 groups.
    advance(time_between_bans).await;
    let filler = (0..MAX_BANNED_IPS - 2)
        .map(|i| u32::try_from(i).expect("MAX_BANNED_IPS fits in a u32"))
        .map(|i| IpAddr::from(Ipv4Addr::from(u32::from(Ipv4Addr::new(10, 0, 0, 0)) + i)));
    for ip in filler {
        bans.ban(ip);
    }
    assert_eq!(
        bans.len(),
        MAX_BANNED_IPS,
        "the list should be exactly full"
    );

    // Refreshing a ban moves it to the newest position.
    advance(time_between_bans).await;
    bans.ban(refreshed);
    assert_eq!(
        bans.len(),
        MAX_BANNED_IPS,
        "refreshing must not add an entry"
    );

    // One more ban overflows the list and evicts the oldest ban.
    advance(time_between_bans).await;
    let newest: IpAddr = "192.0.2.3".parse().unwrap();
    bans.ban(newest);

    assert_eq!(
        bans.len(),
        MAX_BANNED_IPS,
        "the list must stay at the limit"
    );
    assert!(!bans.is_banned(oldest), "the oldest ban should be evicted");
    assert!(
        bans.is_banned(refreshed),
        "a refreshed ban must not be evicted as if it kept its original age"
    );
    assert!(bans.is_banned(newest), "the new ban is enforced");
}

/// An empty ban list bans nothing.
#[test]
fn empty_ban_list_bans_nothing() {
    let _init_guard = zebra_test::init();

    let bans = BanList::default();

    assert!(bans.is_empty());
    assert_eq!(bans.len(), 0);
    assert!(!bans.is_banned("192.0.2.1".parse().unwrap()));
    assert!(!bans.is_banned("2001:db8::1".parse().unwrap()));
}
