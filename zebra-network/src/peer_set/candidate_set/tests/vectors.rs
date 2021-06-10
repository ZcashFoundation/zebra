use std::{
    collections::VecDeque,
    convert::TryInto,
    iter,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::Duration as StdDuration,
};

use chrono::{DateTime, Duration, Utc};
use futures::future;
use tokio::{
    runtime::Runtime,
    sync::watch,
    time::{self, Instant},
};
use tower::Service;
use tracing::Span;

use zebra_chain::serialization::DateTime32;

use super::super::{validate_addrs, CandidateSet};
use crate::{
    constants::{GET_ADDR_FANOUT, MIN_PEER_GET_ADDR_INTERVAL},
    types::{MetaAddr, PeerServices},
    AddressBook, Config, Request, Response,
};

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

/// Test that calls to [`CandidateSet::update`] are rate limited.
#[test]
fn candidate_set_updates_are_rate_limited() {
    // Run the test for enough time for `update` to actually run three times
    const INTERVALS_TO_RUN: u32 = 3;
    // How many times should `update` be called in each rate limit interval
    const POLL_FREQUENCY_FACTOR: u32 = 3;

    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = runtime.enter();

    let address_book = AddressBook::new(&Config::default(), Span::none());
    let (peer_service, call_count) = mock_peer_service();
    let mut candidate_set = CandidateSet::new(Arc::new(Mutex::new(address_book)), peer_service);

    runtime.block_on(async move {
        time::pause();

        let time_limit = Instant::now()
            + INTERVALS_TO_RUN * MIN_PEER_GET_ADDR_INTERVAL
            + StdDuration::from_secs(1);

        while Instant::now() <= time_limit {
            candidate_set
                .update()
                .await
                .expect("Call to CandidateSet::update should not fail");

            time::advance(MIN_PEER_GET_ADDR_INTERVAL / POLL_FREQUENCY_FACTOR).await;
        }

        assert_eq!(
            *call_count.borrow(),
            INTERVALS_TO_RUN as usize * GET_ADDR_FANOUT
        );
    });
}

/// Test that a call to [`CandidateSet::update`] after a call to [`CandidateSet::update_inital`] is
/// rate limited.
#[test]
fn candidate_set_update_after_update_initial_is_rate_limited() {
    let runtime = Runtime::new().expect("Failed to create Tokio runtime");
    let _guard = runtime.enter();

    let address_book = AddressBook::new(&Config::default(), Span::none());
    let (peer_service, call_count) = mock_peer_service();
    let mut candidate_set = CandidateSet::new(Arc::new(Mutex::new(address_book)), peer_service);

    runtime.block_on(async move {
        time::pause();

        // Call `update_initial` first
        candidate_set
            .update_initial(GET_ADDR_FANOUT)
            .await
            .expect("Call to CandidateSet::update should not fail");

        assert_eq!(*call_count.borrow(), GET_ADDR_FANOUT);

        // The following two calls to `update` should be skipped
        candidate_set
            .update()
            .await
            .expect("Call to CandidateSet::update should not fail");
        time::advance(MIN_PEER_GET_ADDR_INTERVAL / 2).await;
        candidate_set
            .update()
            .await
            .expect("Call to CandidateSet::update should not fail");

        assert_eq!(*call_count.borrow(), GET_ADDR_FANOUT);

        // After waiting for at least the minimum interval the call to `update` should succeed
        time::advance(MIN_PEER_GET_ADDR_INTERVAL).await;
        candidate_set
            .update()
            .await
            .expect("Call to CandidateSet::update should not fail");

        assert_eq!(*call_count.borrow(), 2 * GET_ADDR_FANOUT);
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
                SocketAddr::new(IpAddr::from([192, 168, 1, index as u8]), 20_000),
                PeerServices::NODE_NETWORK,
                last_seen,
            )
        })
        .collect()
}

/// Create a mock `PeerSet` service that checks that requests to it are rate limited.
///
/// The function also returns a call count watcher, that can be used for checking how many times the
/// service was called.
fn mock_peer_service<E>() -> (
    impl Service<
            Request,
            Response = Response,
            Future = future::Ready<Result<Response, E>>,
            Error = E,
        > + 'static,
    watch::Receiver<usize>,
) {
    let rate_limit_interval = MIN_PEER_GET_ADDR_INTERVAL;

    let mut call_counter = 0;
    let (call_count_sender, call_count_receiver) = watch::channel(call_counter);

    let mut peer_request_tracker: VecDeque<_> =
        iter::repeat(Instant::now()).take(GET_ADDR_FANOUT).collect();

    let service = tower::service_fn(move |request| {
        match request {
            Request::Peers => {
                // Get time from queue that the request is authorized to be sent
                let authorized_request_time = peer_request_tracker
                    .pop_front()
                    .expect("peer_request_tracker should always have GET_ADDR_FANOUT elements");
                // Check that the request was rate limited
                assert!(Instant::now() >= authorized_request_time);
                // Push a new authorization, updated by the rate limit interval
                peer_request_tracker.push_back(Instant::now() + rate_limit_interval);

                // Increment count of calls
                call_counter += 1;
                let _ = call_count_sender.send(call_counter);

                // Return an empty list of peer addresses
                future::ok(Response::Peers(vec![]))
            }
            _ => unreachable!("Received an unexpected internal message: {:?}", request),
        }
    });

    (service, call_count_receiver)
}
