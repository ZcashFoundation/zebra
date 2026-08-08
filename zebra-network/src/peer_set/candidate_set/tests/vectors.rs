//! Fixed test vectors for candidate peer selection.

use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use tokio::time::{self, Instant};
use tracing::Span;

use zebra_chain::{parameters::Network::*, serialization::DateTime32};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::{
    address_book_updater::{AddressBookUpdater, MIN_CHANNEL_SIZE},
    constants::{DEFAULT_MAX_CONNS_PER_IP, GET_ADDR_FANOUT, MIN_PEER_GET_ADDR_INTERVAL},
    types::{MetaAddr, PeerServices},
    AddressBook, Request, Response,
};

use super::super::{crawl_once, crawler_services, validate_addrs};

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

/// Test that offset is not applied if the most recent `last_seen` time is equal to the limit.
#[test]
fn doesnt_offsets_if_most_recent_last_seen_times_is_exactly_the_limit() {
    let last_seen_limit = DateTime32::now();
    let last_seen_limit_chrono = last_seen_limit.to_chrono();

    let input_peers = mock_gossiped_peers(vec![
        last_seen_limit_chrono,
        last_seen_limit_chrono - Duration::minutes(3),
        last_seen_limit_chrono - Duration::hours(1),
    ]);

    let validated_peers: Vec<_> = validate_addrs(input_peers.clone(), last_seen_limit).collect();

    let expected_peers = input_peers;

    assert_eq!(validated_peers, expected_peers);
}

/// Rejects all addresses if underflow occurs when applying the offset.
#[test]
fn rejects_all_addresses_if_applying_offset_causes_an_underflow() {
    let last_seen_limit = DateTime32::now();

    let input_peers = mock_gossiped_peers(vec![
        DateTime32::from(u32::MIN).to_chrono(),
        last_seen_limit.to_chrono(),
        DateTime32::from(u32::MAX).to_chrono(),
    ]);

    let mut validated_peers = validate_addrs(input_peers, last_seen_limit);

    assert!(validated_peers.next().is_none());
}

/// Test that crawls are rate limited.
#[test]
fn candidate_set_updates_are_rate_limited() {
    // Run the test for enough time for `update` to actually run three times
    const INTERVALS_TO_RUN: u32 = 3;
    // How many times should `update` be called in each rate limit interval
    const POLL_FREQUENCY_FACTOR: u32 = 3;

    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );
    let mut peer_service = MockService::build().for_unit_tests();
    let (
        _address_book,
        _bans_receiver,
        _change_sender,
        address_book_service,
        _address_metrics,
        _updater_guard,
    ) = AddressBookUpdater::spawn_with_address_book(address_book, MIN_CHANNEL_SIZE);
    let (_next_peer_service, mut crawl_service) =
        crawler_services(address_book_service, peer_service.clone());

    runtime.block_on(async move {
        time::pause();

        let time_limit = Instant::now()
            + INTERVALS_TO_RUN * MIN_PEER_GET_ADDR_INTERVAL
            + StdDuration::from_secs(1);
        let mut next_allowed_request_time = Instant::now();

        while Instant::now() <= time_limit {
            crawl_once(&mut crawl_service, None)
                .await
                .expect("crawls should not fail");

            if Instant::now() >= next_allowed_request_time {
                verify_fanned_out_requests(&mut peer_service).await;

                next_allowed_request_time = Instant::now() + MIN_PEER_GET_ADDR_INTERVAL;
            } else {
                peer_service.expect_no_requests().await;
            }

            time::advance(MIN_PEER_GET_ADDR_INTERVAL / POLL_FREQUENCY_FACTOR).await;
        }
    });
}

/// Test that a crawl after an initial crawl is rate limited.
#[test]
fn candidate_set_update_after_update_initial_is_rate_limited() {
    let (runtime, _init_guard) = zebra_test::init_async();
    let _guard = runtime.enter();

    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );
    let mut peer_service = MockService::build().for_unit_tests();
    let (
        _address_book,
        _bans_receiver,
        _change_sender,
        address_book_service,
        _address_metrics,
        _updater_guard,
    ) = AddressBookUpdater::spawn_with_address_book(address_book, MIN_CHANNEL_SIZE);
    let (_next_peer_service, mut crawl_service) =
        crawler_services(address_book_service, peer_service.clone());

    runtime.block_on(async move {
        time::pause();

        // Crawl with an initial fanout limit first
        crawl_once(&mut crawl_service, Some(GET_ADDR_FANOUT))
            .await
            .expect("crawls should not fail");

        verify_fanned_out_requests(&mut peer_service).await;

        // The following two crawls should be skipped
        crawl_once(&mut crawl_service, None)
            .await
            .expect("crawls should not fail");
        time::advance(MIN_PEER_GET_ADDR_INTERVAL / 2).await;
        crawl_once(&mut crawl_service, None)
            .await
            .expect("crawls should not fail");

        peer_service.expect_no_requests().await;

        // After waiting for at least the minimum interval the crawl should run
        time::advance(MIN_PEER_GET_ADDR_INTERVAL).await;
        crawl_once(&mut crawl_service, None)
            .await
            .expect("crawls should not fail");

        verify_fanned_out_requests(&mut peer_service).await;
    });
}

// Utility functions

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
                SocketAddr::new(IpAddr::from([192, 168, 1, index as u8]), 20_000).into(),
                PeerServices::NODE_NETWORK,
                last_seen,
            )
        })
        .collect()
}

/// Verify that a batch of fanned out requests are sent by the candidate set.
///
/// # Panics
///
/// This will panic (causing the test to fail) if more or less requests are received than the
/// expected [`GET_ADDR_FANOUT`] amount.
async fn verify_fanned_out_requests(
    peer_service: &mut MockService<Request, Response, PanicAssertion>,
) {
    for _ in 0..GET_ADDR_FANOUT {
        peer_service
            .expect_request_that(|request| matches!(request, Request::Peers))
            .await
            .respond(Response::Peers(vec![]));
    }

    peer_service.expect_no_requests().await;
}
