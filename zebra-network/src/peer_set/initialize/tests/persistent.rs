//! Persistent target tests with controlled DNS, connection lifetimes, and no sockets.

use std::{
    collections::HashSet,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use futures::{channel::mpsc as futures_mpsc, SinkExt};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{timeout, Instant},
};
use tower::{service_fn, ServiceExt};
use zebra_chain::parameters::Network;

use crate::{
    address_book_updater::{
        AddressBookChangeSender, AddressBookRequest, AddressBookService, AddressBookUpdater,
    },
    config::CacheDir,
    constants,
    meta_addr::MetaAddr,
    peer::{ClientTestHarness, OutboundConnectorRequest},
    peer_set::{
        crawler_services,
        initialize::{
            crawl_and_dial,
            persistent::{maintain, paced_connector},
            DiscoveredPeer,
        },
        set::MorePeers,
        ActiveConnectionCounter, AdvertPeers, ConnectionTracker,
    },
    BanList, BoxError, Config, PeerSocketAddr, Response,
};

const RETRY: Duration = constants::MIN_PEER_RECONNECTION_DELAY;

struct Attempt {
    addr: PeerSocketAddr,
    tracker: Option<ConnectionTracker>,
    at: Instant,
}

struct Harness {
    task: JoinHandle<Result<(), BoxError>>,
    updater_task: JoinHandle<Result<(), BoxError>>,
    attempts: mpsc::UnboundedReceiver<Attempt>,
    _peers: futures_mpsc::Receiver<DiscoveredPeer>,
    peer_sender: futures_mpsc::Sender<DiscoveredPeer>,
    connected: watch::Sender<AdvertPeers>,
    bans: watch::Sender<BanList>,
    address_service: AddressBookService,
    address_updater: AddressBookChangeSender,
}

impl Harness {
    fn new<R, F>(config: Config, fail_first: usize, resolve: R) -> Self
    where
        R: Fn(String) -> F + Send + Sync + 'static,
        F: Future<Output = HashSet<PeerSocketAddr>> + Send + 'static,
    {
        let (_, _, address_updater, address_service, _, updater_task) =
            AddressBookUpdater::spawn(&config, config.listen_addr);
        let (connected, connected_rx) = watch::channel(AdvertPeers::new());
        let (bans, bans_rx) = watch::channel(BanList::default());
        let (peer_sender, peers) = futures_mpsc::channel(config.peerset_total_connection_limit());
        let (attempt_tx, attempts) = mpsc::unbounded_channel();
        let calls = AtomicUsize::new(0);
        let connector = service_fn(move |request: OutboundConnectorRequest| {
            let failed = calls.fetch_add(1, Ordering::SeqCst) < fail_first;
            let (tracker, result) = if failed {
                (
                    None,
                    Err::<DiscoveredPeer, BoxError>("test connection failed".into()),
                )
            } else {
                let (client, _harness) = ClientTestHarness::build().finish();
                (Some(request.connection_tracker), Ok((request.addr, client)))
            };
            attempt_tx
                .send(Attempt {
                    addr: request.addr,
                    tracker,
                    at: Instant::now(),
                })
                .unwrap();
            async move { result }
        });
        let task = tokio::spawn(maintain(
            config,
            paced_connector(connector),
            peer_sender.clone(),
            address_updater.clone(),
            bans_rx,
            connected_rx,
            resolve,
        ));
        Self {
            task,
            updater_task,
            attempts,
            _peers: peers,
            peer_sender,
            connected,
            bans,
            address_service,
            address_updater,
        }
    }

    async fn next_attempt(&mut self) -> Attempt {
        timeout(
            RETRY + constants::DNS_LOOKUP_TIMEOUT + Duration::from_secs(1),
            self.attempts.recv(),
        )
        .await
        .expect("a configured target should be retried")
        .expect("maintenance should remain alive")
    }

    fn add_connected(&self, addr: PeerSocketAddr) {
        let (sender, _receiver) = mpsc::channel(1);
        self.connected.send_modify(|peers| {
            peers.insert(
                addr,
                (constants::CURRENT_NETWORK_PROTOCOL_VERSION, false, sender),
            );
        });
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
        self.updater_task.abort();
    }
}

fn config(targets: &[&str]) -> Config {
    Config {
        cache_dir: CacheDir::disabled(),
        peerset_initial_target_size: 2,
        persistent_peers: targets.iter().map(|target| target.to_string()).collect(),
        ..Config::default()
    }
}

#[tokio::test(start_paused = true)]
async fn persistent_retries_failure_and_disconnect_with_fresh_dns() {
    let resolutions = Arc::new(AtomicUsize::new(0));
    let count = resolutions.clone();
    let mut network = Harness::new(config(&["edge:8233"]), 1, move |_| {
        let call = count.fetch_add(1, Ordering::SeqCst);
        async move {
            [match call {
                0 => "127.0.0.2:8233",
                1 => "127.0.0.3:8233",
                _ => "127.0.0.4:8233",
            }
            .parse()
            .unwrap()]
            .into()
        }
    });
    let failed = network.next_attempt().await;
    assert_eq!(failed.addr, "127.0.0.2:8233".parse().unwrap());
    assert!(failed.tracker.is_none());
    let connected = network.next_attempt().await;
    assert_eq!(connected.addr, "127.0.0.3:8233".parse().unwrap());
    assert!(connected.at.duration_since(failed.at) >= RETRY);
    tokio::time::advance(RETRY * 2).await;
    assert!(network.attempts.try_recv().is_err());
    assert_eq!(resolutions.load(Ordering::SeqCst), 2);
    drop(connected.tracker);
    let replacement = network.next_attempt().await;
    assert_eq!(replacement.addr, "127.0.0.4:8233".parse().unwrap());
    assert_eq!(resolutions.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn persistent_tries_other_dns_answers_before_waiting_to_resolve_again() {
    let mut network = Harness::new(config(&["edge:1"]), 1, |_| async {
        ["127.0.0.2:1", "127.0.0.3:1"]
            .into_iter()
            .map(|addr| addr.parse().unwrap())
            .collect()
    });
    let failed = network.next_attempt().await;
    let connected = network.next_attempt().await;
    assert!(failed.tracker.is_none());
    assert!(connected.tracker.is_some());
    assert_ne!(failed.addr, connected.addr);
    assert!(
        connected.at.duration_since(failed.at) >= constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL
    );
    assert!(connected.at.duration_since(failed.at) < RETRY);
}

struct DropFlag(Arc<AtomicBool>);
impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(start_paused = true)]
async fn persistent_empty_or_stalled_dns_does_not_block_other_targets_or_shutdown() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let mut network = Harness::new(
        config(&["stalled:1", "empty:1", "working:1"]),
        0,
        move |host| {
            let flag = flag.clone();
            async move {
                match host.as_str() {
                    "stalled:1" => {
                        let _cancelled = DropFlag(flag);
                        std::future::pending::<HashSet<PeerSocketAddr>>().await
                    }
                    "empty:1" => HashSet::new(),
                    _ => ["127.0.0.2:1".parse().unwrap()].into(),
                }
            }
        },
    );
    let attempt = timeout(Duration::from_secs(1), network.attempts.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(attempt.addr, "127.0.0.2:1".parse().unwrap());
    network.task.abort();
    assert!((&mut network.task).await.unwrap_err().is_cancelled());
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn persistent_reserved_slot_survives_a_full_ordinary_pool() {
    let mut config = config(&["edge:1"]);
    config.peerset_initial_target_size = 1;
    let mut network = Harness::new(config.clone(), 0, |_| async {
        ["127.0.0.2:1".parse().unwrap()].into()
    });
    let mut counter = ActiveConnectionCounter::new_counter();
    let mut held = Vec::new();
    for host in ["127.0.0.3:1", "127.0.0.4:1"] {
        network.add_connected(host.parse().unwrap());
        held.push(counter.track_connection());
    }
    assert_eq!(
        counter.update_count(),
        config.ordinary_outbound_connection_limit()
    );
    network
        .address_service
        .clone()
        .oneshot(AddressBookRequest::Change(MetaAddr::new_initial_peer(
            "127.0.0.5:1".parse().unwrap(),
        )))
        .await
        .unwrap();
    let empty = service_fn(|_| async { Err::<Response, BoxError>("no crawl peers".into()) });
    let (next, crawl) = crawler_services(network.address_service.clone(), empty);
    let (mut demand, demand_rx) = futures_mpsc::channel(config.peerset_outbound_connection_limit());
    demand.send(MorePeers).await.unwrap();
    let normal = tokio::spawn(crawl_and_dial(
        config.clone(),
        demand,
        demand_rx,
        next,
        crawl,
        network.address_service.clone(),
        service_fn(|_| async { panic!("ordinary peers must not consume the reserved slot") }),
        network.peer_sender.clone(),
        counter,
        network.address_updater.clone(),
    ));
    let persistent = network.next_attempt().await;
    assert_eq!(persistent.addr, "127.0.0.2:1".parse().unwrap());
    assert_eq!(held.len() + 1, config.peerset_outbound_connection_limit());
    tokio::time::advance(config.crawl_new_peer_interval * 2).await;
    tokio::task::yield_now().await;
    assert!(!normal.is_finished());
    assert!(network.attempts.try_recv().is_err());
    normal.abort();
    assert!(normal.await.unwrap_err().is_cancelled());
}

#[tokio::test(start_paused = true)]
async fn persistent_preserves_bans_endpoint_deduplication_and_ip_limits() {
    let mut network = Harness::new(
        config(&[
            "duplicate:1",
            "same-subnet:1",
            "banned:1",
            "alias-a:1",
            "alias-b:1",
        ]),
        0,
        |host| async move {
            [match host.as_str() {
                "duplicate:1" => "127.0.0.2:1",
                "same-subnet:1" => "[2001:db8::2]:1",
                "banned:1" => "127.0.0.3:1",
                _ => "127.0.0.4:1",
            }
            .parse()
            .unwrap()]
            .into()
        },
    );
    network.add_connected("127.0.0.2:1".parse().unwrap());
    network.add_connected("[2001:db8::1]:2".parse().unwrap());
    network.bans.send_modify(|bans| {
        bans.ban("127.0.0.3".parse().unwrap());
    });
    let held = network.next_attempt().await;
    assert_eq!(held.addr, "127.0.0.4:1".parse().unwrap());
    tokio::time::advance(RETRY * 2).await;
    tokio::task::yield_now().await;
    assert!(network.attempts.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn persistent_regtest_only_connects_to_localhost() {
    let mut config = config(&["edge:1"]);
    config.network = Network::new_regtest(Default::default());
    let mut network = Harness::new(config, 0, |_| async {
        ["192.0.2.1:1", "127.0.0.1:1"]
            .into_iter()
            .map(|addr| addr.parse().unwrap())
            .collect()
    });
    assert_eq!(
        network.next_attempt().await.addr,
        "127.0.0.1:1".parse().unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn persistent_shutdown_cancels_inflight_handshakes() {
    let config = config(&["edge:1"]);
    let (_, _, updater, _, _, updater_task) =
        AddressBookUpdater::spawn(&config, config.listen_addr);
    let (peers, _receiver) = futures_mpsc::channel(1);
    let (_bans, bans_rx) = watch::channel(BanList::default());
    let (_connected, connected_rx) = watch::channel(AdvertPeers::new());
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let (started, mut attempts) = mpsc::unbounded_channel();
    let connector = service_fn(move |request: OutboundConnectorRequest| {
        let cancelled = DropFlag(flag.clone());
        started.send(()).unwrap();
        async move {
            let _request = request;
            let _cancelled = cancelled;
            std::future::pending::<Result<DiscoveredPeer, BoxError>>().await
        }
    });
    let task = tokio::spawn(maintain(
        config,
        connector,
        peers,
        updater,
        bans_rx,
        connected_rx,
        |_| async { ["127.0.0.2:1".parse().unwrap()].into() },
    ));
    timeout(Duration::from_secs(1), attempts.recv())
        .await
        .unwrap()
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(cancelled.load(Ordering::SeqCst));
    updater_task.abort();
}

#[tokio::test(start_paused = true)]
async fn persistent_and_ordinary_sources_share_pacing_without_serializing_handshakes() {
    let (attempts, mut started) = mpsc::unbounded_channel();
    let connector = paced_connector(service_fn(move |request: OutboundConnectorRequest| {
        let attempts = attempts.clone();
        async move {
            attempts.send(Instant::now()).unwrap();
            let _request = request;
            std::future::pending::<Result<(), BoxError>>().await
        }
    }));
    let request = |addr: &str| OutboundConnectorRequest {
        addr: addr.parse().unwrap(),
        connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
    };
    let ordinary = tokio::spawn(connector.clone().oneshot(request("127.0.0.1:8233")));
    let persistent = tokio::spawn(connector.oneshot(request("127.0.0.2:8233")));
    let first = timeout(Duration::from_secs(1), started.recv())
        .await
        .unwrap()
        .unwrap();
    let second = timeout(
        constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL * 2,
        started.recv(),
    )
    .await
    .expect("another handshake can start while the first is pending")
    .unwrap();
    assert_eq!(
        second - first,
        constants::MIN_OUTBOUND_PEER_CONNECTION_INTERVAL
    );
    ordinary.abort();
    persistent.abort();
    assert!(ordinary.await.unwrap_err().is_cancelled());
    assert!(persistent.await.unwrap_err().is_cancelled());
}
