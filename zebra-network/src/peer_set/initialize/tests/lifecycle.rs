//! Keep the runtime alive while checking ownership of real network tasks and channels.

use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures::{channel::mpsc, stream, StreamExt};
use indexmap::IndexSet;
use tokio::{net::TcpListener, time::timeout};
use tower::{buffer::Buffer, discover::Change, service_fn, util::BoxService, Service, ServiceExt};
use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    address_book_updater::AddressBookUpdater,
    config::CacheDir,
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    peer::{self, ClientTestHarness, LoadTrackedClient, MinimumPeerVersion},
    peer_set::{
        crawler_services,
        initialize::crawl_and_dial,
        set::{tests::PeerSetBuilder, MorePeers},
        ActiveConnectionCounter,
    },
    BoxError, Config, NetworkService, PeerSocketAddr, Request, Response,
};

const SHUTDOWN_BOUND: Duration = Duration::from_secs(5);

/// A non-owning background handle must not keep its Buffer worker and peer clients alive.
#[tokio::test(start_paused = true)]
async fn dropping_network_buffer_stops_worker() {
    check_background_owner(false).await;
}

/// The crawler's non-owning handle must not keep the network alive.
#[tokio::test(start_paused = true)]
async fn dropping_network_buffer_stops_crawler() {
    check_background_owner(true).await;
}

/// Consuming the last service handle must not cancel the request started by `oneshot`.
#[tokio::test(start_paused = true)]
async fn last_owning_oneshot_keeps_network_alive_until_response() {
    check_response_owner(false).await;
}

/// An abandoned response must release its ownership even after `oneshot` consumed the service.
#[tokio::test(start_paused = true)]
async fn dropping_last_owning_response_stops_network() {
    check_response_owner(true).await;
}

async fn check_response_owner(cancel_response: bool) {
    let _init_guard = zebra_test::init();
    let (client, mut client_harness) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .finish();
    let peer_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
    let peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> =
        vec![Ok(Change::Insert(peer_addr, client.into()))];
    let (minimum_version, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (peer_set, _guard) = PeerSetBuilder::new()
        .with_discover(stream::iter(peers).chain(stream::pending()))
        .with_minimum_peer_version(minimum_version)
        .build();
    let (network, poller) = peer_set.into_buffer(crate::constants::PEERSET_BUFFER_SIZE);
    let background = network.background_service();
    let mut response = Some(Box::pin(network.oneshot(Request::Peers)));

    let request = timeout(SHUTDOWN_BOUND, async {
        loop {
            if let Some(request) = client_harness
                .try_to_receive_outbound_client_request()
                .request()
            {
                break request;
            }
            tokio::select! {
                result = response.as_mut().unwrap() => {
                    panic!("the last owning request completed before its peer answered: {result:?}");
                }
                _ = tokio::time::sleep(Duration::from_millis(1)) => {}
            }
        }
    })
    .await
    .expect("the response future must preserve ownership after oneshot consumes the service");
    assert_eq!(request.request, Request::Peers);

    if cancel_response {
        drop(response.take());
        drop(request);
    } else {
        request.tx.send(Ok(Response::Peers(Vec::new()))).unwrap();
        assert_eq!(
            timeout(SHUTDOWN_BOUND, response.as_mut().unwrap())
                .await
                .expect("the last owning request must finish")
                .unwrap(),
            Response::Peers(Vec::new()),
        );
    }

    // Keep the completed future allocated: completion itself must release shutdown ownership.
    timeout(SHUTDOWN_BOUND, async {
        assert!(poller.await.unwrap_err().is_cancelled());
        assert!(background.oneshot(Request::Peers).await.is_err());
        while !client_harness
            .try_to_receive_outbound_client_request()
            .is_closed()
        {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("completing or abandoning the last owning request must stop the network");
    assert!(!client_harness.wants_connection_heartbeats());
}

async fn check_background_owner(crawler_only: bool) {
    let _init_guard = zebra_test::init();
    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        peerset_initial_target_size: 1,
        ..Config::default()
    };
    let (client, mut client_harness) = ClientTestHarness::build()
        .with_version(CURRENT_NETWORK_PROTOCOL_VERSION)
        .finish();
    let peer_addr: PeerSocketAddr = "127.0.0.2:1".parse().unwrap();
    let peers: Vec<Result<Change<PeerSocketAddr, LoadTrackedClient>, BoxError>> =
        vec![Ok(Change::Insert(peer_addr, client.into()))];
    let (minimum_version, _tip) = MinimumPeerVersion::with_mock_chain_tip(&Network::Mainnet);
    let (peer_set, mut guard) = PeerSetBuilder::new()
        .with_config(config.clone())
        .with_discover(stream::iter(peers).chain(stream::pending()))
        .with_minimum_peer_version(minimum_version)
        .build();
    let (buffer, worker) = Buffer::pair(
        BoxService::new(peer_set),
        crate::constants::PEERSET_BUFFER_SIZE,
    );
    let worker = tokio::spawn(worker);
    let network = NetworkService::new(buffer, vec![worker.abort_handle()]);
    let mut last_external_handle = network.clone();
    drop(network);

    // Preservation control: dropping only one external clone must not stop the network.
    let response = last_external_handle
        .ready()
        .await
        .unwrap()
        .call(Request::Peers);
    let request = timeout(SHUTDOWN_BOUND, async {
        loop {
            if let Some(request) = client_harness
                .try_to_receive_outbound_client_request()
                .request()
            {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the remaining external handle must still route requests");
    assert_eq!(request.request, Request::Peers);
    request.tx.send(Ok(Response::Peers(Vec::new()))).unwrap();
    assert_eq!(response.await.unwrap(), Response::Peers(Vec::new()));

    let (owned_task, background_tasks) = if crawler_only {
        let (_address_book, _bans, address_updater, address_service, _metrics, address_task) =
            AddressBookUpdater::spawn(&config, config.listen_addr);
        let (next_peer, crawl_service) = crawler_services(
            address_service.clone(),
            last_external_handle.background_service(),
        );
        let (demand_tx, demand_rx) = mpsc::channel::<MorePeers>(1);
        let (peerset_tx, _peerset_rx) = mpsc::channel(1);
        let handshake = peer::Handshake::builder()
            .with_config(config.clone())
            .with_inbound_service(service_fn(|_| async { Ok::<_, BoxError>(Response::Nil) }))
            .with_user_agent("lifecycle test".to_owned())
            .with_latest_chain_tip(NoChainTip)
            .finish()
            .unwrap();
        let crawler = tokio::spawn(crawl_and_dial(
            config,
            demand_tx,
            demand_rx,
            next_peer,
            crawl_service,
            address_service,
            peer::Connector::new(handshake),
            peerset_tx,
            ActiveConnectionCounter::new_counter(),
            address_updater,
        ));
        let crawler_observer = crawler.abort_handle();

        // Observe the actual crawler's initial timer-driven Peers request through the Buffer.
        let crawl_request = timeout(SHUTDOWN_BOUND, async {
            loop {
                if let Some(request) = client_harness
                    .try_to_receive_outbound_client_request()
                    .request()
                {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("the real crawler must run before its ownership is tested");
        assert_eq!(crawl_request.request, Request::Peers);
        crawl_request
            .tx
            .send(Ok(Response::Peers(Vec::new())))
            .unwrap();
        (crawler_observer, vec![crawler, address_task])
    } else {
        (worker.abort_handle(), Vec::new())
    };
    // With no crawler, leave task delivery pending rather than send an invalid empty task set.
    if !background_tasks.is_empty() {
        last_external_handle.extend_tasks(background_tasks.iter().map(|task| task.abort_handle()));
        guard
            .background_tasks_sender()
            .take()
            .unwrap()
            .send(background_tasks)
            .unwrap();
    }
    let background = last_external_handle.background_service();

    drop(last_external_handle);
    timeout(SHUTDOWN_BOUND, async {
        assert!(worker.await.unwrap_err().is_cancelled());
        loop {
            let received = client_harness.try_to_receive_outbound_client_request();
            if received.is_closed() && owned_task.is_finished() {
                break;
            }
            if let Some(request) = received.request() {
                assert_eq!(request.request, Request::Peers);
                let _ = request.tx.send(Ok(Response::Peers(Vec::new())));
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect(
        "dropping the last external network handle must close peer channels and stop owned tasks",
    );
    assert!(
        timeout(SHUTDOWN_BOUND, background.oneshot(Request::Peers))
            .await
            .expect("a retained background handle must observe shutdown")
            .is_err(),
        "background handles must not keep the network alive",
    );
    assert!(!client_harness.wants_connection_heartbeats());
}

/// Real init, TCP listener, handshake, Connection and crawler resources must all be released
/// without dropping the runtime or first forcing a network error.
#[tokio::test]
async fn dropping_network_handles_stops_owned_tasks() {
    let _init_guard = zebra_test::init();
    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    // Closure-owned sender witnesses release of the real handshaker/connection inbound service,
    // including the crawler's outbound connector. This is channel closure, not an Arc count.
    let (inbound_owner, mut inbound_released) = tokio::sync::mpsc::channel::<()>(1);
    let inbound_network = Arc::new(OnceLock::new());
    let setup_network = inbound_network.clone();
    let inbound = service_fn(move |request| {
        let _owner = &inbound_owner;
        let _network = &inbound_network;
        async move {
            Ok::<_, BoxError>(match request {
                Request::Peers => Response::Peers(Vec::new()),
                _ => Response::Nil,
            })
        }
    });
    let config = Config {
        listen_addr: "127.0.0.1:0".parse().unwrap(),
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        peerset_initial_target_size: 1,
        ..Config::default()
    };
    let (network, address_book, misbehavior) = timeout(
        SHUTDOWN_BOUND,
        crate::init(config, inbound, NoChainTip, "lifecycle test".to_owned()),
    )
    .await
    .expect("local-only initialization must finish");
    // Match the node's inbound/mempool ownership: network-owned services retain a raw handle.
    assert!(
        setup_network.set(network.background_service()).is_ok(),
        "the inbound network handle is initialized once"
    );
    drop(setup_network);
    let listen_addr = address_book
        .lock()
        .expect("the freshly initialized address book has not panicked")
        .local_listener_socket_addr();
    let mut connected_peer = timeout(
        SHUTDOWN_BOUND,
        crate::connect_isolated_tcp_direct_with_inbound(
            &Network::Mainnet,
            listen_addr,
            "lifecycle peer".to_owned(),
            service_fn(|_| async { Ok::<_, BoxError>(Response::Peers(Vec::new())) }),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let last_external_handle = network.clone();
    drop(network);
    timeout(
        SHUTDOWN_BOUND,
        last_external_handle.clone().oneshot(Request::Peers),
    )
    .await
    .expect("the remaining external handle must still route requests")
    .expect("the real connected peer must respond successfully");
    drop(last_external_handle);

    let listener_closed = async {
        loop {
            match TcpListener::bind(listen_addr).await {
                Ok(listener) => break listener,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected listener rebind failure: {error}"),
            }
        }
    };
    let connection_closed = async {
        loop {
            if connected_peer.ready().await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    timeout(SHUTDOWN_BOUND, async {
        let (_rebound_listener, (), closed, ()) = tokio::join!(
            listener_closed,
            connection_closed,
            inbound_released.recv(),
            misbehavior.closed(),
        );
        assert_eq!(closed, None);
    })
    .await
    .expect(
        "listener, peer connection and crawler-owned inbound service must stop with runtime alive",
    );
}
