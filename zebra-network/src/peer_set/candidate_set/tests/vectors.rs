use std::{
    convert::TryInto,
    net::{IpAddr, SocketAddr},
};

use chrono::{DateTime, Duration, Utc};

use zebra_chain::serialization::DateTime32;

use super::super::validate_addrs;
use crate::types::{MetaAddr, PeerServices};

/// Test that offset is applied when all addresses have `last_seen` times in the future.
#[test]
fn offsets_last_seen_times_in_the_future() {
    let last_seen_limit = DateTime32::now();
    let last_seen_limit_chrono = last_seen_limit.to_chrono();

    let input_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono + Duration::minutes(30),
        last_seen_limit_chrono + Duration::minutes(15),
        last_seen_limit_chrono + Duration::minutes(45),
    ]);

    let validated_peers: Vec<_> = validate_addrs(input_peers, last_seen_limit).collect();

    let expected_offset = Duration::minutes(45);
    let expected_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono + Duration::minutes(30) - expected_offset,
        last_seen_limit_chrono + Duration::minutes(15) - expected_offset,
        last_seen_limit_chrono + Duration::minutes(45) - expected_offset,
    ]);

    assert_eq!(validated_peers, expected_peers);
}

/// Test that offset is not applied if all addresses have `last_seen` times that are in the past.
#[test]
fn doesnt_offset_last_seen_times_in_the_past() {
    let last_seen_limit = DateTime32::now();
    let last_seen_limit_chrono = last_seen_limit.to_chrono();

    let input_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono - Duration::minutes(30),
        last_seen_limit_chrono - Duration::minutes(45),
        last_seen_limit_chrono - Duration::days(1),
    ]);

    let validated_peers: Vec<_> = validate_addrs(input_peers.clone(), last_seen_limit).collect();

    let expected_peers = input_peers;

    assert_eq!(validated_peers, expected_peers);
}

/// Test that offset is applied to all the addresses if at least one has a `last_seen` time in the
/// future.
///
/// Times that are in the past should be changed as well.
#[test]
fn offsets_all_last_seen_times_if_one_is_in_the_future() {
    let last_seen_limit = DateTime32::now();
    let last_seen_limit_chrono = last_seen_limit.to_chrono();

    let input_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono + Duration::minutes(55),
        last_seen_limit_chrono - Duration::days(3),
        last_seen_limit_chrono - Duration::hours(2),
    ]);

    let validated_peers: Vec<_> = validate_addrs(input_peers, last_seen_limit).collect();

    let expected_offset = Duration::minutes(55);
    let expected_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono + Duration::minutes(55) - expected_offset,
        last_seen_limit_chrono - Duration::days(3) - expected_offset,
        last_seen_limit_chrono - Duration::hours(2) - expected_offset,
    ]);

    assert_eq!(validated_peers, expected_peers);
}

/// Create a mock list of gossiped [`MetaAddr`]s with the specified `last_seen_times`.
///
/// The IP address and port of the generated ports should not matter for the test.
fn mock_gossiped_peers(last_seen_times: impl IntoIterator<Item = DateTime<Utc>>) -> Vec<MetaAddr> {
    last_seen_times
        .into_iter()
        .enumerate()
        .map(|(index, last_seen_chrono)| {
            let last_seen = last_seen_chrono
                .try_into()
                .expect("`last_seen` time doesn't fit in a `DateTime32`");

            MetaAddr::new_gossiped_meta_addr(
                SocketAddr::new(IpAddr::from([192, 168, 1, index as u8]), 20_000),
                PeerServices::NODE_NETWORK,
                last_seen,
            )
        })
        .collect()
}
