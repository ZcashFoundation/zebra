//! Tests for the peer book actor's request handling.

use std::{collections::HashSet, net::SocketAddr, str::FromStr};

use chrono::Utc;
use futures::future;
use tower::ServiceExt;
use tracing::Span;

use zebra_chain::{parameters::Network::Mainnet, serialization::DateTime32};

use crate::{
    address_book_peers::AddressBookPeers,
    address_book_updater::{AddressBookUpdater, MIN_CHANNEL_SIZE},
    constants::DEFAULT_MAX_CONNS_PER_IP,
    peer_book::{PeerBookRequest, PeerBookResponse},
    protocol::types::PeerServices,
    types::MetaAddr,
    AddressBook,
};

/// The number of candidate peers used in the concurrency test.
const TEST_PEER_COUNT: usize = 8;

/// Test that the peer book actor serves each request variant,
/// with the matching response variant.
#[tokio::test]
async fn peer_book_actor_serves_all_request_variants() {
    let _init_guard = zebra_test::init();

    let local_listener = SocketAddr::from_str("0.0.0.0:0").unwrap();
    let address_book = AddressBook::new(
        local_listener,
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );

    let handles =
        AddressBookUpdater::spawn_with_book(address_book, local_listener, MIN_CHANNEL_SIZE);

    let gossiped_addr = |address_number: usize| {
        SocketAddr::from_str(&format!("127.1.1.{address_number}:1")).unwrap()
    };

    let gossiped_meta_addr = |address_number: usize| {
        MetaAddr::new_gossiped_meta_addr(
            gossiped_addr(address_number).into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )
    };

    // A change event inserts a new gossiped peer. Changes are fire-and-forget,
    // and the actor applies its message channel in order, so a later call
    // observes the applied change.
    handles
        .change_sender
        .send(
            gossiped_meta_addr(0)
                .new_gossiped_change()
                .expect("gossiped peers always have services set"),
        )
        .await
        .expect("the peer book actor is running");

    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::TestPeerEntry(gossiped_addr(0).into()))
        .await
        .expect("actor should be running");
    match response {
        PeerBookResponse::PeerEntry(Some(entry)) => assert_eq!(
            entry.addr,
            gossiped_addr(0).into(),
            "the stored entry should be the peer that was just gossiped",
        ),
        other => panic!("a new gossiped change should update the peer book: {other:?}"),
    }

    // A `GossipedAddrs` request inserts a batch of gossiped peers.
    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::GossipedAddrs {
            addrs: vec![gossiped_meta_addr(1), gossiped_meta_addr(2)],
        })
        .await
        .expect("actor should be running");
    assert!(matches!(response, PeerBookResponse::Done));

    // All the gossiped peers are ready for a connection attempt.
    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::ReadyCandidateCount)
        .await
        .expect("actor should be running");
    assert!(
        matches!(response, PeerBookResponse::ReadyCandidateCount(3)),
        "all gossiped peers should be ready for a connection attempt: {response:?}",
    );

    // Recently gossiped peers are cacheable.
    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::CacheSnapshot)
        .await
        .expect("actor should be running");
    match response {
        PeerBookResponse::Addrs(peers) => assert_eq!(peers.len(), 3),
        other => panic!("unexpected response variant: {other:?}"),
    }

    // Gossiped peers have never responded, so they are not recently live.
    assert_eq!(
        handles.reader.recently_live_peers(Utc::now()),
        vec![],
        "gossiped peers should not be recently live",
    );

    // A `SelectCandidate` request returns one of the gossiped peers.
    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::SelectCandidate)
        .await
        .expect("actor should be running");
    match response {
        PeerBookResponse::Candidate(Some(peer)) => {
            let gossiped: Vec<_> = (0..3).map(|number| gossiped_addr(number).into()).collect();
            assert!(
                gossiped.contains(&peer.addr),
                "the reconnection candidate should be one of the gossiped peers: {peer:?}",
            );
        }
        other => panic!("gossiped peers should be available for reconnection: {other:?}"),
    }
}

/// Test that concurrent `SelectCandidate` requests never return the same peer.
///
/// The actor picks the candidate and marks it as `AttemptPending` in the same
/// actor turn, so each request marks its candidate before the next request
/// chooses one.
#[tokio::test]
async fn concurrent_select_candidate_requests_return_distinct_peers() {
    let _init_guard = zebra_test::init();

    let local_listener = SocketAddr::from_str("0.0.0.0:0").unwrap();
    let mut address_book = AddressBook::new(
        local_listener,
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );

    // Add never-attempted gossiped peers, which are always ready for a
    // connection attempt. Use distinct IP addresses, so the peers aren't
    // limited by the maximum connections per IP.
    for address_number in 0..TEST_PEER_COUNT {
        let addr = SocketAddr::from_str(&format!("127.1.1.{address_number}:1")).unwrap();
        let change = MetaAddr::new_gossiped_meta_addr(
            addr.into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )
        .new_gossiped_change()
        .expect("gossiped peers always have services set");

        address_book.update(change);
    }

    let handles =
        AddressBookUpdater::spawn_with_book(address_book, local_listener, MIN_CHANNEL_SIZE);

    // Make all the requests concurrently, using independent handles.
    let requests = (0..TEST_PEER_COUNT).map(|_| {
        handles
            .handle
            .clone()
            .oneshot(PeerBookRequest::SelectCandidate)
    });

    let responses = future::join_all(requests).await;

    let unique_peers: HashSet<_> = responses
        .into_iter()
        .map(|response| {
            match response.expect("the peer book actor should serve concurrent requests") {
                PeerBookResponse::Candidate(Some(peer)) => peer.addr,
                PeerBookResponse::Candidate(None) => {
                    panic!("every request should return a candidate peer")
                }
                other => panic!("unexpected response variant: {other:?}"),
            }
        })
        .collect();

    assert_eq!(
        unique_peers.len(),
        TEST_PEER_COUNT,
        "concurrent SelectCandidate requests must never return the same peer",
    );
}
