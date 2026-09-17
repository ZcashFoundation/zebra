//! Tests for the address book updater service.

use std::{collections::HashSet, net::SocketAddr, str::FromStr};

use futures::future;
use tower::ServiceExt;
use tracing::Span;

use zebra_chain::{parameters::Network::Mainnet, serialization::DateTime32};

use crate::{
    address_book_updater::{
        AddressBookRequest, AddressBookResponse, AddressBookUpdater, MIN_CHANNEL_SIZE,
    },
    constants::DEFAULT_MAX_CONNS_PER_IP,
    protocol::types::PeerServices,
    types::MetaAddr,
    AddressBook,
};

/// The number of candidate peers used in the concurrency test.
const TEST_PEER_COUNT: usize = 8;

/// Test that the address book service serves each request variant,
/// with the matching response variant.
#[tokio::test]
async fn address_book_service_serves_all_request_variants() {
    let _init_guard = zebra_test::init();

    let address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
        &Mainnet,
        DEFAULT_MAX_CONNS_PER_IP,
        Span::none(),
    );

    let (
        _address_book,
        _bans_receiver,
        _change_sender,
        address_book_service,
        _address_metrics,
        _updater_guard,
    ) = AddressBookUpdater::spawn_with_address_book(address_book, MIN_CHANNEL_SIZE);

    let gossiped_addr = |address_number: usize| {
        SocketAddr::from_str(&format!("127.1.1.{address_number}:1")).unwrap()
    };

    let gossiped_change = |address_number: usize| {
        MetaAddr::new_gossiped_meta_addr(
            gossiped_addr(address_number).into(),
            PeerServices::NODE_NETWORK,
            DateTime32::now(),
        )
        .new_gossiped_change()
        .expect("gossiped peers always have services set")
    };

    // A `Change` request inserts a new gossiped peer, and returns the updated entry.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::Change(gossiped_change(0)))
        .await
        .expect("service should be running");
    match response {
        AddressBookResponse::Updated(Some(updated)) => assert_eq!(
            updated.addr,
            gossiped_addr(0).into(),
            "the updated entry should be the peer that was just gossiped",
        ),
        other => panic!("a new gossiped change should update the address book: {other:?}"),
    }

    // An `ExtendGossiped` request inserts a batch of gossiped peers.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::ExtendGossiped(vec![
            gossiped_change(1),
            gossiped_change(2),
        ]))
        .await
        .expect("service should be running");
    assert!(matches!(response, AddressBookResponse::Extended));

    // All the gossiped peers are ready for a connection attempt.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::ReadyPeerCount)
        .await
        .expect("service should be running");
    assert!(
        matches!(response, AddressBookResponse::ReadyPeerCount(3)),
        "all gossiped peers should be ready for a connection attempt: {response:?}",
    );

    // Recently gossiped peers are cacheable.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::CacheablePeers)
        .await
        .expect("service should be running");
    match response {
        AddressBookResponse::Peers(peers) => assert_eq!(peers.len(), 3),
        other => panic!("unexpected response variant: {other:?}"),
    }

    // Gossiped peers have never responded, so they are not recently live.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::RecentlyLivePeers)
        .await
        .expect("service should be running");
    match response {
        AddressBookResponse::Peers(peers) => assert_eq!(peers, vec![]),
        other => panic!("unexpected response variant: {other:?}"),
    }

    // A `NextReconnectPeer` request returns one of the gossiped peers.
    let response = address_book_service
        .clone()
        .oneshot(AddressBookRequest::NextReconnectPeer)
        .await
        .expect("service should be running");
    match response {
        AddressBookResponse::NextReconnectPeer(Some(peer)) => {
            let gossiped: Vec<_> = (0..3).map(|number| gossiped_addr(number).into()).collect();
            assert!(
                gossiped.contains(&peer.addr),
                "the reconnection candidate should be one of the gossiped peers: {peer:?}",
            );
        }
        other => panic!("gossiped peers should be available for reconnection: {other:?}"),
    }
}

/// Test that concurrent `NextReconnectPeer` requests never return the same peer.
///
/// Requests are served from a single ordered queue, so each request marks its
/// candidate as `AttemptPending` before the next request chooses a candidate.
#[tokio::test]
async fn concurrent_next_reconnect_peer_requests_return_distinct_peers() {
    let _init_guard = zebra_test::init();

    let mut address_book = AddressBook::new(
        SocketAddr::from_str("0.0.0.0:0").unwrap(),
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

    let (
        _address_book,
        _bans_receiver,
        _change_sender,
        address_book_service,
        _address_metrics,
        _updater_guard,
    ) = AddressBookUpdater::spawn_with_address_book(address_book, MIN_CHANNEL_SIZE);

    // Make all the requests concurrently, using independent service handles.
    let requests = (0..TEST_PEER_COUNT).map(|_| {
        address_book_service
            .clone()
            .oneshot(AddressBookRequest::NextReconnectPeer)
    });

    let responses = future::join_all(requests).await;

    let unique_peers: HashSet<_> = responses
        .into_iter()
        .map(|response| {
            match response.expect("address book service should serve concurrent requests") {
                AddressBookResponse::NextReconnectPeer(Some(peer)) => peer.addr,
                AddressBookResponse::NextReconnectPeer(None) => {
                    panic!("every request should return a candidate peer")
                }
                other => panic!("unexpected response variant: {other:?}"),
            }
        })
        .collect();

    assert_eq!(
        unique_peers.len(),
        TEST_PEER_COUNT,
        "concurrent NextReconnectPeer requests must never return the same peer",
    );
}
