//! Fixed test vectors for the peer book actor.

use std::time::Duration;

use chrono::Utc;
use tower::{Service, ServiceExt};

use zebra_chain::parameters::Network;

use crate::{
    address_book_updater::AddressBookUpdater,
    constants::{CURRENT_NETWORK_PROTOCOL_VERSION, MAX_PEER_MISBEHAVIOR_SCORE},
    meta_addr::{MetaAddr, MetaAddrChange},
    peer_book::{PeerBookRequest, PeerBookResponse},
    protocol::external::types::PeerServices,
    AddressBookPeers, Config, PeerSocketAddr,
};

/// A generous timeout for actor operations in tests.
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The reader's recently-live snapshot follows applied changes, the handle
/// serves calls, and `add_peer` sends changes through the actor.
#[tokio::test]
async fn peer_book_actor_serves_reads_and_calls() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };

    let crate::address_book_updater::PeerBookHandles {
        bans_receiver: _bans_receiver,
        change_sender,
        address_metrics: _address_metrics,
        actor_task: _updater_guard,
        handle: peer_book_handle,
        reader: mut peer_book_reader,
    } = AddressBookUpdater::spawn(&config, "127.0.0.1:8233".parse().expect("valid address"));

    assert!(
        peer_book_reader.recently_live_peers(Utc::now()).is_empty(),
        "a new book has no live peers",
    );

    // A completed outbound handshake makes the peer recently live, and the
    // reader's snapshot follows it.
    let addr: PeerSocketAddr = "203.0.113.7:8233".parse().expect("valid address");
    change_sender
        .send(MetaAddr::new_connected(
            addr,
            &PeerServices::NODE_NETWORK,
            false,
            "/PeerBookTest:1.0.0/".to_string(),
            CURRENT_NETWORK_PROTOCOL_VERSION,
        ))
        .await
        .expect("the actor is running");

    let live_peer_appears = async {
        loop {
            let live = peer_book_reader.recently_live_peers(Utc::now());
            if live.iter().any(|peer| peer.addr() == addr) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(TEST_TIMEOUT, live_peer_appears)
        .await
        .expect("the reader sees the connected peer in time");

    // The handle serves cache snapshots, which include the live outbound
    // peer, and sanitized address responses.
    let mut handle = peer_book_handle.clone();
    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        handle
            .ready()
            .await
            .expect("the call task is running")
            .call(PeerBookRequest::CacheSnapshot),
    )
    .await
    .expect("response arrives in time")
    .expect("the call task answers");
    let PeerBookResponse::Addrs(cacheable) = response else {
        panic!("expected an Addrs response");
    };
    assert!(
        cacheable.iter().any(|peer| peer.addr() == addr),
        "a live outbound peer is cacheable, got: {cacheable:?}",
    );

    let response = tokio::time::timeout(
        TEST_TIMEOUT,
        handle
            .ready()
            .await
            .expect("the call task is running")
            .call(PeerBookRequest::SanitizedAddrs),
    )
    .await
    .expect("response arrives in time")
    .expect("the call task answers");
    let PeerBookResponse::Addrs(_sanitized) = response else {
        panic!("expected an Addrs response");
    };

    // `add_peer` reports that the change was accepted for processing.
    assert!(peer_book_reader.add_peer("203.0.113.8:8233".parse().expect("valid address")));
}

/// Misbehavior changes that ban a peer update the bans watch channel.
#[tokio::test]
async fn peer_book_actor_updates_bans_watch() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };

    let crate::address_book_updater::PeerBookHandles {
        mut bans_receiver,
        change_sender,
        address_metrics: _address_metrics,
        actor_task: _updater_guard,
        handle: _peer_book_handle,
        reader: _peer_book_reader,
    } = AddressBookUpdater::spawn(&config, "127.0.0.1:8233".parse().expect("valid address"));

    let addr: PeerSocketAddr = "203.0.113.9:8233".parse().expect("valid address");
    change_sender
        .send(MetaAddrChange::UpdateMisbehavior {
            addr,
            score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
        })
        .await
        .expect("the actor is running");

    tokio::time::timeout(TEST_TIMEOUT, bans_receiver.changed())
        .await
        .expect("the bans watch updates in time")
        .expect("the actor is running");

    assert!(
        bans_receiver
            .borrow()
            .contains_key(&crate::peer_book::BanKey::from(addr.ip())),
        "the banned peer's address is in the bans watch",
    );
}

/// Regression test for <https://github.com/ZcashFoundation/zebra/issues/11134>.
///
/// `by_addr` is ordered by reconnection order, not grouped by IP, so a ban
/// must remove every entry for the banned IP, even entries that don't sort
/// next to the banned address. A surviving entry would stay at the front of
/// the reconnection order for the lifetime of the process, because the actor
/// filters banned candidates out of every selection.
#[tokio::test]
async fn ban_removes_every_entry_for_the_banned_ip() {
    use tracing::Span;
    use zebra_chain::{
        parameters::Network::Mainnet,
        serialization::{DateTime32, Duration32},
    };

    let _init_guard = zebra_test::init();

    let banned_addr: PeerSocketAddr = "127.0.0.1:8233".parse().expect("valid address");
    let unrelated_addr: PeerSocketAddr = "127.0.0.2:8233".parse().expect("valid address");
    // An ephemeral-port entry for the same IP, like the one in #11134.
    let zombie_addr: PeerSocketAddr = "127.0.0.1:43562".parse().expect("valid address");

    // `max_connections_per_ip` is above one, so candidate selection does not
    // skip the second entry on the banned IP for being a duplicate IP.
    let local_listener = "0.0.0.0:0".parse().expect("valid address");
    let mut address_book = crate::AddressBook::new(local_listener, &Mainnet, 2, Span::current());

    // `MetaAddr`'s `Ord` sorts more recently gossiped addresses first, so these
    // last seen times place the unrelated IP between the two entries for the
    // banned IP, making the removal scan non-contiguous.
    for (addr, last_seen) in [(banned_addr, 2), (unrelated_addr, 1), (zombie_addr, 0)] {
        address_book.update(
            MetaAddr::new_gossiped_meta_addr(
                addr,
                PeerServices::NODE_NETWORK,
                DateTime32::MIN.saturating_add(Duration32::from_seconds(last_seen)),
            )
            .new_gossiped_change()
            .expect("gossiped peers always have services set"),
        );
    }

    let handles = AddressBookUpdater::spawn_with_book(address_book, local_listener, 10);
    let mut bans_receiver = handles.bans_receiver.clone();

    handles
        .change_sender
        .send(MetaAddrChange::UpdateMisbehavior {
            addr: banned_addr,
            score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
        })
        .await
        .expect("the actor is running");

    tokio::time::timeout(TEST_TIMEOUT, bans_receiver.changed())
        .await
        .expect("the bans watch updates in time")
        .expect("the actor is running");
    assert!(
        bans_receiver
            .borrow()
            .contains_key(&crate::peer_book::BanKey::from(banned_addr.ip())),
        "ban-threshold misbehavior should ban the peer IP",
    );

    // The ban removes every entry for the banned IP, including the one that
    // does not sort next to the banned address.
    for addr in [banned_addr, zombie_addr] {
        let response = handles
            .handle
            .clone()
            .oneshot(PeerBookRequest::TestPeerEntry(addr))
            .await
            .expect("the actor is running");
        assert!(
            matches!(response, PeerBookResponse::PeerEntry(None)),
            "the ban should remove the entry for {addr}: {response:?}",
        );
    }

    // The unrelated peer is the only remaining reconnection candidate.
    let response = handles
        .handle
        .clone()
        .oneshot(PeerBookRequest::SelectCandidates { max: 1 })
        .await
        .expect("the actor is running");
    match response {
        PeerBookResponse::Candidates(candidates) => {
            let (peer, _transports) = candidates
                .first()
                .expect("the unrelated peer should be a candidate");
            assert_eq!(
                peer.addr, unrelated_addr,
                "a banned IP must never be a reconnection candidate",
            );
        }
        other => panic!("unexpected response variant: {other:?}"),
    }
}

/// A sub-threshold misbehavior report for an already-banned peer must not
/// re-create its book entry: the ban reset the peer's score, so later
/// reports fall through the ban threshold and used to reach the book.
#[tokio::test]
async fn banned_peer_cannot_reenter_via_further_misbehavior() {
    let _init_guard = zebra_test::init();

    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };

    let handles =
        AddressBookUpdater::spawn(&config, "127.0.0.1:8233".parse().expect("valid address"));
    let mut bans_receiver = handles.bans_receiver.clone();

    let addr: PeerSocketAddr = "203.0.113.10:8233".parse().expect("valid address");
    handles
        .change_sender
        .send(MetaAddrChange::UpdateMisbehavior {
            addr,
            score_increment: MAX_PEER_MISBEHAVIOR_SCORE,
        })
        .await
        .expect("the actor is running");

    tokio::time::timeout(TEST_TIMEOUT, bans_receiver.changed())
        .await
        .expect("the bans watch updates in time")
        .expect("the actor is running");

    // A follow-up report, like one from the misbehavior batcher flushing
    // after the ban fired.
    handles
        .change_sender
        .send(MetaAddrChange::UpdateMisbehavior {
            addr,
            score_increment: 1,
        })
        .await
        .expect("the actor is running");

    let response = handles
        .handle
        .clone()
        .oneshot(crate::peer_book::PeerBookRequest::TestPeerEntry(addr))
        .await
        .expect("the actor is running");
    assert!(
        matches!(response, PeerBookResponse::PeerEntry(None)),
        "a banned peer must not re-enter the book through further misbehavior reports: \
         {response:?}",
    );
}

/// Gossiped addresses from one address group are bounded by the group's
/// secret-keyed bucket, on the production intake path (`GossipedAddrs`).
#[tokio::test]
async fn gossiped_addrs_are_bounded_by_group_buckets() {
    use zebra_chain::serialization::DateTime32;

    use crate::peer_book::buckets::GOSSIP_BUCKET_SIZE;

    let _init_guard = zebra_test::init();

    let config = Config {
        network: Network::Mainnet,
        ..Config::default()
    };

    let handles =
        AddressBookUpdater::spawn(&config, "127.0.0.1:8233".parse().expect("valid address"));

    // Three buckets' worth of addresses, all in one IPv4 /16 group.
    let addrs: Vec<MetaAddr> = (0..3 * GOSSIP_BUCKET_SIZE)
        .map(|index| {
            // Safety: the index is bounded far below u8::MAX * 250.
            let addr = format!("198.51.{}.{}:8233", index / 250, 1 + (index % 250));
            MetaAddr::new_gossiped_meta_addr(
                addr.parse().expect("valid address"),
                crate::protocol::types::PeerServices::NODE_NETWORK,
                DateTime32::now(),
            )
        })
        .collect();

    let response = handles
        .handle
        .clone()
        .oneshot(crate::peer_book::PeerBookRequest::GossipedAddrs { addrs })
        .await
        .expect("the actor is running");
    assert!(matches!(response, PeerBookResponse::Done));

    let response = handles
        .handle
        .clone()
        .oneshot(crate::peer_book::PeerBookRequest::CacheSnapshot)
        .await
        .expect("the actor is running");
    match response {
        PeerBookResponse::Addrs(peers) => assert_eq!(
            peers.len(),
            GOSSIP_BUCKET_SIZE,
            "one address group must be bounded by one bucket's capacity",
        ),
        other => panic!("unexpected response variant: {other:?}"),
    }
}
