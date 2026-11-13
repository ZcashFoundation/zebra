//! Tests for the address relay hub, on a paused clock.

use std::{collections::HashSet, net::SocketAddr, time::Duration};

use tokio::sync::mpsc;

use zebra_chain::{
    parameters::Network,
    serialization::{DateTime32, Duration32},
};

use crate::{meta_addr::MetaAddr, protocol::external::types::PeerServices, PeerSocketAddr};

use super::{
    relay_targets, unix_day, AddrRelay, AddrRelayHandle, Timings, MAX_RELAY_BATCH,
    RELAY_CHANNEL_SIZE,
};

/// The relay secret of the tests' hubs.
const SECRET: [u8; 32] = [7; 32];

/// Longer than any trickle delay, which is capped at ten times its mean.
const TRICKLE_WAIT: Duration = Duration::from_secs(10 * 60);

/// A registered peer's address.
fn peer(index: u8) -> PeerSocketAddr {
    format!("198.51.100.{index}:8233")
        .parse::<SocketAddr>()
        .unwrap()
        .into()
}

/// A relayed address with `time`.
fn gossiped(index: u8, time: DateTime32) -> MetaAddr {
    MetaAddr::new_gossiped_meta_addr(
        format!("203.0.113.{index}:8233")
            .parse::<SocketAddr>()
            .unwrap()
            .into(),
        PeerServices::NODE_NETWORK,
        time,
    )
}

/// A hub with the tests' secret.
fn hub(advertised: Option<&str>) -> AddrRelayHandle {
    AddrRelay::spawn_with(
        advertised.map(|addr| addr.parse().unwrap()),
        Network::Mainnet,
        SECRET,
        Timings::default(),
    )
}

/// Registers `peers` with `hub`, returning each one's receiver.
fn register(hub: &AddrRelayHandle, peers: &[PeerSocketAddr]) -> Vec<mpsc::Receiver<Vec<MetaAddr>>> {
    peers
        .iter()
        .map(|peer| {
            let (tx, rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
            hub.register(*peer, tx);
            rx
        })
        .collect()
}

/// Everything `rx` receives within `wait`.
async fn drain(rx: &mut mpsc::Receiver<Vec<MetaAddr>>, wait: Duration) -> Vec<MetaAddr> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut received = Vec::new();
    while let Ok(Some(batch)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        received.extend(batch);
    }
    received
}

/// Each fresh address goes to exactly two peers other than its source: the
/// ones the day's ranking picks.
#[tokio::test(start_paused = true)]
async fn fresh_small_batches_go_to_exactly_two_other_peers() {
    let _init_guard = zebra_test::init();
    let hub = hub(None);
    let peers: Vec<_> = (1..=6).map(peer).collect();
    let mut receivers = register(&hub, &peers);

    let now = DateTime32::now();
    let batch: Vec<_> = (1..=10).map(|index| gossiped(index, now)).collect();
    hub.offer(peers[0], batch.clone());

    let mut received_by = Vec::new();
    for rx in &mut receivers {
        let addrs: HashSet<_> = drain(rx, TRICKLE_WAIT)
            .await
            .iter()
            .map(MetaAddr::addr)
            .collect();
        received_by.push(addrs);
    }
    assert!(received_by[0].is_empty(), "the source gets nothing back");

    let day = unix_day(DateTime32::now());
    for addr in &batch {
        let recipients: HashSet<_> = peers
            .iter()
            .zip(&received_by)
            .filter(|(_, addrs)| addrs.contains(&addr.addr()))
            .map(|(peer, _)| *peer)
            .collect();
        let expected: HashSet<_> =
            relay_targets(&SECRET, addr.addr(), day, peers[1..].iter().copied())
                .into_iter()
                .collect();
        assert_eq!(recipients.len(), 2, "{addr:?} went to {recipients:?}");
        assert_eq!(recipients, expected);
    }
}

/// The ranking is fixed for a day, changes across days and secrets, and
/// picks two of the candidates.
#[test]
fn relay_targets_are_deterministic_per_day() {
    let candidates: Vec<_> = (1..=8).map(peer).collect();
    let day = unix_day(DateTime32::now());

    let mut changed_across_days = 0;
    let mut changed_across_secrets = 0;
    for index in 1..=50 {
        let addr = gossiped(index, DateTime32::now()).addr();
        let today = relay_targets(&SECRET, addr, day, candidates.iter().copied());
        assert_eq!(today.len(), 2);
        assert!(today.iter().all(|target| candidates.contains(target)));
        assert_eq!(
            today,
            relay_targets(&SECRET, addr, day, candidates.iter().rev().copied()),
            "the order of the candidates does not matter"
        );

        if today != relay_targets(&SECRET, addr, day + 1, candidates.iter().copied()) {
            changed_across_days += 1;
        }
        if today != relay_targets(&[8; 32], addr, day, candidates.iter().copied()) {
            changed_across_secrets += 1;
        }
    }
    assert!(changed_across_days > 0);
    assert!(changed_across_secrets > 0);

    let one = [candidates[0]];
    assert_eq!(
        relay_targets(&SECRET, candidates[1], day, one),
        one.to_vec(),
        "with fewer candidates than the fanout, all of them"
    );
}

/// A large offer is relayed in full, at most ten addresses per trickle, so
/// how many records the source's announcements arrived in together doesn't
/// matter.
#[tokio::test(start_paused = true)]
async fn large_offers_are_sent_ten_per_trickle() {
    let _init_guard = zebra_test::init();
    let hub = hub(None);
    let peers: Vec<_> = (1..=3).map(peer).collect();
    let mut receivers = register(&hub, &peers);

    let now = DateTime32::now();
    let offered: Vec<_> = (1..=25).map(|index| gossiped(index, now)).collect();
    hub.offer(peers[0], offered.clone());
    let expected: HashSet<_> = offered.iter().map(MetaAddr::addr).collect();

    // With two candidates, both get every address.
    for rx in &mut receivers[1..] {
        let mut received = HashSet::new();
        let mut batches = 0;
        while received.len() < expected.len() {
            let batch = tokio::time::timeout(TRICKLE_WAIT, rx.recv())
                .await
                .expect("every queued address is sent within a few trickles")
                .expect("the hub is running");
            assert!(
                !batch.is_empty() && batch.len() <= MAX_RELAY_BATCH,
                "a trickle sends 1 to {MAX_RELAY_BATCH} addresses, not {}",
                batch.len()
            );
            received.extend(batch.iter().map(MetaAddr::addr));
            batches += 1;
        }
        assert_eq!(received, expected);
        assert_eq!(batches, 3, "25 addresses take three trickles");
    }
    assert_eq!(drain(&mut receivers[0], TRICKLE_WAIT).await, Vec::new());
}

/// Records older than ten minutes, and unroutable ones, are not relayed.
#[tokio::test(start_paused = true)]
async fn stale_or_unroutable_addresses_are_not_relayed() {
    let _init_guard = zebra_test::init();
    let hub = hub(None);
    let peers: Vec<_> = (1..=3).map(peer).collect();
    let mut receivers = register(&hub, &peers);

    let now = DateTime32::now();
    let stale = now.saturating_sub(Duration32::from_minutes(11));
    hub.offer(peers[0], vec![gossiped(20, stale)]);
    // A zero time is clamped to five days ago.
    hub.offer(peers[0], vec![gossiped(21, DateTime32::from(0))]);
    // An unroutable address.
    hub.offer(
        peers[0],
        vec![MetaAddr::new_gossiped_meta_addr(
            "0.0.0.0:8233".parse::<SocketAddr>().unwrap().into(),
            PeerServices::NODE_NETWORK,
            now,
        )],
    );

    for rx in &mut receivers {
        assert_eq!(drain(rx, TRICKLE_WAIT).await, Vec::new());
    }
}

/// An address is not sent again to a peer that announced it or was already
/// sent it.
#[tokio::test(start_paused = true)]
async fn known_addresses_are_not_echoed() {
    let _init_guard = zebra_test::init();
    let hub = hub(None);
    let peers: Vec<_> = (1..=3).map(peer).collect();
    let mut receivers = register(&hub, &peers);

    let addr = gossiped(1, DateTime32::now());
    hub.offer(peers[0], vec![addr.clone()]);
    for rx in &mut receivers[1..] {
        let received = drain(rx, TRICKLE_WAIT).await;
        assert_eq!(
            received.iter().map(MetaAddr::addr).collect::<Vec<_>>(),
            vec![addr.addr()]
        );
    }

    // The other two peers announce it back: the first announced it, and each knows it.
    let addr = gossiped(1, DateTime32::now());
    hub.offer(peers[1], vec![addr.clone()]);
    hub.offer(peers[2], vec![addr]);
    for rx in &mut receivers {
        assert_eq!(drain(rx, TRICKLE_WAIT).await, Vec::new());
    }
}

/// This node's address is advertised once soon after a peer registers, then
/// about a day later.
#[tokio::test(start_paused = true)]
async fn self_advertisement_is_sent_soon_after_registering() {
    let _init_guard = zebra_test::init();
    let ours: SocketAddr = "203.0.113.200:8233".parse().unwrap();
    let hub = hub(Some("203.0.113.200:8233"));
    let mut receivers = register(&hub, &[peer(1)]);
    let rx = &mut receivers[0];

    let first = drain(rx, Duration::from_secs(61)).await;
    assert_eq!(first.len(), 1, "one self-advertisement in the first minute");
    assert_eq!(*first[0].addr(), ours);
    let age =
        DateTime32::now().saturating_duration_since(first[0].last_seen().expect("has a time"));
    assert!(age <= Duration32::from_minutes(2), "its time is fresh");

    assert_eq!(
        drain(rx, Duration::from_secs(22 * 60 * 60)).await,
        Vec::new(),
        "nothing more for most of a day"
    );
    let second = drain(rx, Duration::from_secs(4 * 60 * 60)).await;
    assert_eq!(second.len(), 1, "the next one about a day later");
    assert_eq!(*second[0].addr(), ours);
}

/// Without an address, or with one that peers can't connect to, nothing is
/// advertised.
#[tokio::test(start_paused = true)]
async fn no_self_advertisement_without_an_address() {
    let _init_guard = zebra_test::init();
    for advertised in [None, Some("0.0.0.0:8233"), Some("203.0.113.200:0")] {
        let hub = hub(advertised);
        let mut receivers = register(&hub, &[peer(1)]);
        assert_eq!(
            drain(&mut receivers[0], Duration::from_secs(2 * 60 * 60)).await,
            Vec::new(),
            "{advertised:?} is not advertised"
        );
    }
}

/// A deregistered peer gets nothing more: its channel is dropped.
#[tokio::test(start_paused = true)]
async fn deregistered_peers_get_nothing() {
    let _init_guard = zebra_test::init();
    let hub = hub(Some("203.0.113.200:8233"));
    let peers = [peer(1), peer(2)];
    let (tx, mut rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    let registration = hub.register(peers[1], tx);
    let _source_rx = register(&hub, &peers[..1]);
    hub.deregister(registration);
    hub.offer(peers[0], vec![gossiped(1, DateTime32::now())]);

    let received = tokio::time::timeout(TRICKLE_WAIT, rx.recv())
        .await
        .expect("the hub drops the channel promptly");
    assert_eq!(received, None, "no self-advertisement or relayed address");

    // A stale deregistration does not remove a newer registration of the same peer.
    let (tx, mut rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    hub.register(peers[1], tx);
    hub.deregister(registration);
    let first = drain(&mut rx, Duration::from_secs(61)).await;
    assert_eq!(first.len(), 1, "the newer registration is advertised to");
}

/// A handle to a hub that has not started, with room for one message, and
/// the queue that hub would read.
fn stalled_hub() -> (AddrRelayHandle, mpsc::Receiver<super::Message>) {
    let (inbox, receiver) = mpsc::channel(1);
    let handle = AddrRelayHandle {
        inbox,
        next_id: Default::default(),
    };
    (handle, receiver)
}

/// A registration that finds the hub's queue full is delivered once the hub
/// catches up, without the caller waiting, so the connection still gets
/// addresses.
#[tokio::test(start_paused = true)]
async fn registration_survives_a_full_hub_queue() {
    let _init_guard = zebra_test::init();
    let (handle, receiver) = stalled_hub();
    handle.offer(peer(9), Vec::new());

    let (tx, mut rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    handle.register(peer(1), tx);

    // The hub starts late, but well within the wait.
    tokio::time::sleep(super::REGISTER_TIMEOUT / 2).await;
    let hub = super::Hub {
        advertised: Some("203.0.113.200:8233".parse::<SocketAddr>().unwrap().into()),
        ..bare_hub()
    };
    tokio::spawn(hub.run(receiver));

    let first = drain(&mut rx, Duration::from_secs(61)).await;
    assert_eq!(first.len(), 1, "the late registration is advertised to");
}

/// A registration that waits for the hub is given up when its connection
/// ends, or after the registration timeout, so nothing waits forever.
#[tokio::test(start_paused = true)]
async fn waiting_registrations_are_given_up() {
    let _init_guard = zebra_test::init();

    // The connection ends while its registration waits.
    let (handle, mut receiver) = stalled_hub();
    handle.offer(peer(9), Vec::new());
    let (tx, rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    handle.register(peer(1), tx);
    drop(rx);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(matches!(
        receiver.try_recv(),
        Ok(super::Message::Offer { .. })
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        receiver.try_recv().is_err(),
        "an ended connection is not registered"
    );

    // The hub stays behind for longer than the timeout.
    let (handle, mut receiver) = stalled_hub();
    handle.offer(peer(9), Vec::new());
    let (tx, _rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    handle.register(peer(1), tx);
    tokio::time::sleep(super::REGISTER_TIMEOUT + Duration::from_secs(1)).await;
    assert!(matches!(
        receiver.try_recv(),
        Ok(super::Message::Offer { .. })
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        receiver.try_recv().is_err(),
        "a registration that timed out is not delivered later"
    );
}

/// A hub's state, without its task, for checking what it keeps.
fn bare_hub() -> super::Hub {
    super::Hub {
        secret: SECRET,
        advertised: None,
        network: Network::Mainnet,
        timings: Timings::default(),
        peers: Default::default(),
    }
}

/// Records that don't claim to serve the chain are not relayed, as the
/// address book doesn't keep them, and don't suppress a later full-node
/// record for the same address.
#[tokio::test(start_paused = true)]
async fn non_node_addresses_are_not_relayed() {
    let _init_guard = zebra_test::init();
    let hub = hub(None);
    let peers: Vec<_> = (1..=3).map(peer).collect();
    let mut receivers = register(&hub, &peers);

    let node = gossiped(1, DateTime32::now());
    let non_node =
        MetaAddr::new_gossiped_meta_addr(node.addr(), PeerServices::empty(), DateTime32::now());
    hub.offer(peers[0], vec![non_node]);
    for rx in &mut receivers {
        assert_eq!(drain(rx, TRICKLE_WAIT).await, Vec::new());
    }

    hub.offer(peers[0], vec![node.clone()]);
    for rx in &mut receivers[1..] {
        let received = drain(rx, TRICKLE_WAIT).await;
        assert_eq!(
            received.iter().map(MetaAddr::addr).collect::<Vec<_>>(),
            vec![node.addr()]
        );
    }
}

/// An address dropped because a peer's queue is full is not remembered as
/// known to that peer, so it is relayed to it later.
#[test]
fn addresses_dropped_for_a_full_queue_are_not_marked_known() {
    let _init_guard = zebra_test::init();
    let mut hub = bare_hub();
    let now = tokio::time::Instant::now();
    let (source_tx, _source_rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    let (tx, _rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    hub.handle(
        super::Message::Register {
            peer: peer(1),
            id: 1,
            tx: source_tx,
        },
        now,
    );
    hub.handle(
        super::Message::Register {
            peer: peer(2),
            id: 2,
            tx,
        },
        now,
    );

    let time = DateTime32::now();
    let addr = |index: usize| {
        MetaAddr::new_gossiped_meta_addr(
            format!("203.0.{}.{}:8233", index / 250 + 1, index % 250 + 1)
                .parse::<SocketAddr>()
                .unwrap()
                .into(),
            PeerServices::NODE_NETWORK,
            time,
        )
    };
    let offered: Vec<_> = (0..=super::MAX_QUEUED_ADDRS).map(addr).collect();
    let dropped = offered.last().unwrap().clone();
    hub.offer(peer(1), offered, now);

    let dest = &hub.peers[&peer(2)];
    assert_eq!(dest.queue.len(), super::MAX_QUEUED_ADDRS);
    assert!(!dest.known.contains(&dropped.addr()));

    // Once the queue is sent, the dropped address is relayed when offered again.
    hub.peers.get_mut(&peer(2)).unwrap().queue.clear();
    hub.offer(peer(1), vec![dropped.clone()], now);
    let queued: Vec<_> = hub.peers[&peer(2)]
        .queue
        .iter()
        .map(MetaAddr::addr)
        .collect();
    assert_eq!(queued, vec![dropped.addr()]);
}

/// Destinations whose connection ended without deregistering, like an
/// aborted connection task, are dropped when another peer registers, so the
/// hub keeps at most its live registrations and the new one.
#[test]
fn hub_drops_closed_destinations_without_deregistration() {
    let _init_guard = zebra_test::init();
    let mut hub = bare_hub();
    let now = tokio::time::Instant::now();
    for index in 1..=50 {
        let (tx, rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
        drop(rx);
        hub.handle(
            super::Message::Register {
                peer: peer(index),
                id: u64::from(index),
                tx,
            },
            now,
        );
    }

    let (tx, _rx) = mpsc::channel(RELAY_CHANNEL_SIZE);
    hub.handle(
        super::Message::Register {
            peer: peer(200),
            id: 200,
            tx,
        },
        now,
    );
    assert_eq!(hub.peers.len(), 1);
    assert!(hub.peers.contains_key(&peer(200)));
}
