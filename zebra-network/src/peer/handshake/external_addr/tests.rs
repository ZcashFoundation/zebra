//! Deterministic discovery and on-wire handshake coverage.

use tower::{service_fn, util::BoxCloneService, ServiceExt};

use super::*;
use crate::{peer::handshake::*, peer_set::ActiveConnectionCounter};

type TestHandshake = Handshake<BoxCloneService<Request, Response, BoxError>>;

fn handshake(config: Config) -> TestHandshake {
    Handshake::builder()
        .with_config(config)
        .with_advertised_services(PeerServices::NODE_NETWORK)
        .with_inbound_service(
            service_fn(|_| async { Ok::<_, BoxError>(Response::Nil) }).boxed_clone(),
        )
        .finish()
        .unwrap()
}

fn addresses(state: &mut ExternalAddrs, now: Instant) -> Vec<SocketAddr> {
    state
        .addresses("[::]:18233".parse().unwrap(), None, now)
        .into_iter()
        .flatten()
        .collect()
}

fn observe(state: &mut ExternalAddrs, reporter: &str, address: &str, now: Instant) {
    state.observe(reporter.parse().unwrap(), address.parse().unwrap(), now);
}

#[test]
fn discovery_requires_independent_groups_and_tracks_families_and_rollover() {
    let now = Instant::now();
    let mut state = ExternalAddrs::default();
    for reporter in ["8.8.8.8", "8.8.4.4", "8.8.9.9", "8.8.8.8"] {
        observe(&mut state, reporter, "45.33.32.1", now);
    }
    assert!(addresses(&mut state, now).is_empty());
    observe(&mut state, "9.9.9.9", "45.33.32.1", now);
    assert_eq!(
        addresses(&mut state, now),
        ["45.33.32.1:18233".parse().unwrap()]
    );

    observe(&mut state, "8.8.8.8", "93.184.216.34", now);
    assert_eq!(
        addresses(&mut state, now),
        ["45.33.32.1:18233".parse().unwrap()]
    );
    observe(&mut state, "9.9.9.9", "93.184.216.34", now);
    for reporter in ["2606:4700::1", "2001:4860::8888"] {
        observe(&mut state, reporter, "2606:4700::1111", now);
    }
    assert_eq!(
        addresses(&mut state, now),
        [
            "93.184.216.34:18233".parse().unwrap(),
            "[2606:4700::1111]:18233".parse().unwrap(),
        ]
    );
    assert_eq!(
        addresses(&mut state, now + LIFETIME),
        [
            "93.184.216.34:18233".parse().unwrap(),
            "[2606:4700::1111]:18233".parse().unwrap(),
        ]
    );
    assert!(state.observations.iter().all(Option::is_none));

    for reporter in ["2606:4700::1", "2606:4700:abcd::2"] {
        observe(&mut state, reporter, "2606:4700::1111", now + LIFETIME);
    }
    assert_eq!(addresses(&mut state, now + LIFETIME).len(), 2);
    observe(
        &mut state,
        "2001:4860::8888",
        "2606:4700::1111",
        now + LIFETIME,
    );
    assert_eq!(addresses(&mut state, now + LIFETIME).len(), 2);
}

#[test]
fn discovery_bounds_storage_and_does_not_multiply_churning_subnet_votes() {
    let now = Instant::now();
    let mut state = ExternalAddrs::default();
    for index in 0..MAX_OBSERVATIONS + 20 {
        state.observe(
            Ipv4Addr::new(11, u8::try_from(index).unwrap(), 0, 1).into(),
            "45.33.32.1".parse().unwrap(),
            now + Duration::from_secs(u64::try_from(index).unwrap()),
        );
    }
    assert_eq!(
        state.observations.iter().flatten().count(),
        MAX_OBSERVATIONS
    );
    let groups: Vec<_> = state
        .observations
        .iter()
        .flatten()
        .map(|o| o.group)
        .collect();
    for index in 0..100 {
        state.observe(
            Ipv4Addr::new(11, 83, 1, index).into(),
            "93.184.216.34".parse().unwrap(),
            now + Duration::from_secs(100),
        );
    }
    assert_eq!(
        groups,
        state
            .observations
            .iter()
            .flatten()
            .map(|o| o.group)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        state
            .observations
            .iter()
            .flatten()
            .filter(|o| o.address == "93.184.216.34".parse::<IpAddr>().unwrap())
            .count(),
        1
    );
}

#[test]
fn discovery_rejects_unsafe_reports_and_reporters() {
    let mut state = ExternalAddrs::default();
    let now = Instant::now();
    for address in [
        "0.1.2.3",
        "10.1.2.3",
        "100.64.0.1",
        "100.127.255.254",
        "127.0.0.1",
        "169.254.1.2",
        "172.16.0.1",
        "172.31.255.254",
        "192.0.0.9",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.19.1.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:192.168.1.1",
        "64:ff9b::808:808",
        "100::1",
        "2001::1",
        "2001:2::1",
        "2001:10::1",
        "2001:20::1",
        "2001:db8::1",
        "2002::1",
        "3fff::1",
        "fc00::1",
        "fe80::1",
        "ff02::1",
    ] {
        let ip = address.parse().unwrap();
        assert!(!is_public(ip), "accepted {address}");
        state.observe("8.8.8.8".parse().unwrap(), ip, now);
        state.observe(ip, "45.33.32.1".parse().unwrap(), now);
    }
    assert!(state.observations.iter().all(Option::is_none));
    for address in [
        "8.8.8.8",
        "45.33.32.1",
        "2001:4860::8888",
        "2606:4700::1111",
    ] {
        assert!(is_public(address.parse().unwrap()));
    }
}

#[test]
fn discovery_rejects_cross_family_reports_but_accepts_canonical_ipv4() {
    let mut state = ExternalAddrs::default();
    let now = Instant::now();
    for reporter in ["8.8.8.8", "9.9.9.9"] {
        observe(&mut state, reporter, "2606:4700::1111", now);
    }
    for reporter in ["2606:4700::1", "2001:4860::8888"] {
        observe(&mut state, reporter, "45.33.32.1", now);
    }
    assert!(state.observations.iter().all(Option::is_none));
    observe(&mut state, "::ffff:8.8.8.8", "::ffff:45.33.32.1", now);
    observe(&mut state, "9.9.9.9", "45.33.32.1", now);
    assert_eq!(
        addresses(&mut state, now),
        ["45.33.32.1:18233".parse().unwrap()]
    );
}

#[test]
fn discovery_configuration_precedence_and_ties() {
    let now = Instant::now();
    let mut state = ExternalAddrs::default();
    for reporter in ["8.8.8.8", "9.9.9.9"] {
        observe(&mut state, reporter, "45.33.32.1", now);
    }
    let public = "93.184.216.34:18234".parse().unwrap();
    let external = "1.1.1.1:18235".parse().unwrap();
    assert_eq!(state.addresses(public, None, now), [Some(public), None]);
    assert_eq!(
        state.addresses(public, Some(external), now),
        [Some(external), None]
    );
    assert_eq!(
        state.addresses("0.0.0.0:0".parse().unwrap(), None, now),
        [None, None]
    );
    for reporter in ["1.1.1.1", "4.2.2.2"] {
        observe(&mut state, reporter, "93.184.216.34", now);
    }
    assert!(
        addresses(&mut state, now).is_empty(),
        "equal competing quorums are ambiguous"
    );
}

/// Run the actual Handshake service over duplex transports, draining through a Pong to
/// prove that every unsolicited Addr sent before handing off the connection was observed.
async fn exchange(
    handshake: TestHandshake,
    connected_addr: ConnectedAddr,
    report: &str,
    accepted: bool,
) -> (VersionMessage, Vec<MetaAddr>) {
    let report: SocketAddr = report.parse().unwrap();
    let (local, remote) = tokio::io::duplex(65536);
    let network = handshake.config.network.clone();
    let request = HandshakeRequest {
        data_stream: local,
        connected_addr,
        connection_tracker: ActiveConnectionCounter::new_counter().track_connection(),
    };
    let remote = async move {
        let mut peer = Framed::new(remote, Codec::builder().for_network(&network).finish());
        let Some(Ok(Message::Version(version))) = peer.next().await else {
            panic!("expected initial Version");
        };
        let mut response = version.clone();
        response.nonce = Nonce::default();
        response.services = if accepted {
            PeerServices::NODE_NETWORK
        } else {
            PeerServices::empty()
        };
        response.address_from = AddrInVersion::new(
            "8.8.8.8:8233".parse::<SocketAddr>().unwrap(),
            response.services,
        );
        response.address_recv = AddrInVersion::new(report, PeerServices::NODE_NETWORK);
        peer.send(response.into()).await.unwrap();
        peer.send(Message::Verack).await.unwrap();
        (version, peer)
    };
    let (result, (version, mut peer)) = timeout(
        Duration::from_secs(10),
        future::join(handshake.oneshot(request), remote),
    )
    .await
    .unwrap();
    if !accepted {
        assert!(result.is_err());
        return (version, Vec::new());
    }
    let _client = result.expect("scripted handshake succeeds");
    let ping = Nonce::default();
    peer.send(Message::Ping(ping)).await.unwrap();
    let adverts = timeout(Duration::from_secs(10), async {
        let mut adverts = Vec::new();
        loop {
            match peer.next().await.unwrap().unwrap() {
                Message::Addr(addresses) => adverts.extend(addresses),
                Message::Ping(nonce) => peer.send(Message::Pong(nonce)).await.unwrap(),
                Message::Pong(nonce) if nonce == ping => break adverts,
                Message::Verack => {}
                message => panic!("unexpected message: {message:?}"),
            }
        }
    })
    .await
    .unwrap();
    (version, adverts)
}

#[tokio::test]
async fn discovery_advertises_listener_port_and_updates_future_version_across_clones() {
    let _guard = zebra_test::init();
    let handshake = handshake(Config {
        listen_addr: "[::]:18233".parse().unwrap(),
        ..Config::default()
    });
    let before = DateTime32::now();
    for (reporter, expected_adverts) in [
        ("8.8.8.8:8233", 0),
        ("9.9.9.9:8233", 1),
        ("1.1.1.1:8233", 1),
    ] {
        let (version, adverts) = exchange(
            handshake.clone(),
            ConnectedAddr::new_outbound_direct(reporter.parse().unwrap()),
            "45.33.32.1:49152",
            true,
        )
        .await;
        assert_eq!(adverts.len(), expected_adverts);
        if reporter.starts_with("1.1.") {
            assert_eq!(
                version.address_from.addr(),
                "45.33.32.1:18233".parse().unwrap()
            );
        } else {
            assert!(version.address_from.addr().ip().is_unspecified());
        }
        for advert in adverts {
            assert_eq!(advert.addr(), "45.33.32.1:18233".parse().unwrap());
            assert_eq!(advert.services(), Some(PeerServices::NODE_NETWORK));
            assert!(advert.last_seen().unwrap() >= before);
            assert!(advert.last_seen().unwrap() <= DateTime32::now());
        }
    }
    for reporter in ["[2606:4700::1]:8233", "[2001:4860::8888]:8233"] {
        exchange(
            handshake.clone(),
            ConnectedAddr::new_outbound_direct(reporter.parse().unwrap()),
            "[2606:4700::1111]:49153",
            true,
        )
        .await;
    }
    let (version, adverts) = exchange(
        handshake,
        ConnectedAddr::new_outbound_direct("[2001:4860::8888]:8233".parse().unwrap()),
        "[2606:4700::1111]:49154",
        true,
    )
    .await;
    assert_eq!(
        version.address_from.addr(),
        "[2606:4700::1111]:18233".parse().unwrap()
    );
    assert_eq!(adverts.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn discovery_retains_elected_address_without_new_outbound_handshakes() {
    let _guard = zebra_test::init();
    let handshake = handshake(Config {
        listen_addr: "[::]:18233".parse().unwrap(),
        ..Config::default()
    });
    for reporter in ["8.8.8.8:8233", "9.9.9.9:8233"] {
        exchange(
            handshake.clone(),
            ConnectedAddr::new_outbound_direct(reporter.parse().unwrap()),
            "45.33.32.1:49152",
            true,
        )
        .await;
    }
    tokio::time::advance(LIFETIME).await;
    let (version, adverts) = exchange(
        handshake.clone(),
        ConnectedAddr::new_inbound_direct("1.1.1.1:49152".parse().unwrap()),
        "45.33.32.1:49152",
        true,
    )
    .await;
    assert_eq!(
        version.address_from.addr(),
        "45.33.32.1:18233".parse().unwrap()
    );
    assert_eq!(adverts[0].addr(), "45.33.32.1:18233".parse().unwrap());
    assert!(handshake
        .external_addrs
        .lock()
        .await
        .observations
        .iter()
        .all(Option::is_none));
}

#[tokio::test]
async fn discovery_ignores_failed_private_inbound_and_proxy_reports_without_leaking() {
    let _guard = zebra_test::init();
    let handshake = handshake(Config {
        listen_addr: "[::]:18233".parse().unwrap(),
        ..Config::default()
    });
    let now = Instant::now();
    observe(
        &mut *handshake.external_addrs.lock().await,
        "8.8.8.8",
        "45.33.32.1",
        now,
    );
    let connections = [
        ConnectedAddr::new_outbound_direct("9.9.9.9:8233".parse().unwrap()),
        ConnectedAddr::new_outbound_direct("127.0.0.1:8233".parse().unwrap()),
        ConnectedAddr::new_inbound_direct("9.9.9.9:49152".parse().unwrap()),
        ConnectedAddr::new_outbound_proxy(
            "9.9.9.9:9050".parse().unwrap(),
            "127.0.0.1:49152".parse().unwrap(),
        ),
        ConnectedAddr::new_inbound_proxy("127.0.0.1:49152".parse().unwrap()),
        ConnectedAddr::new_isolated(),
    ];
    for connected in [connections[0], connections[1], connections[3]] {
        exchange(handshake.clone(), connected, "45.33.32.1:49152", false).await;
    }
    assert_eq!(
        handshake
            .external_addrs
            .lock()
            .await
            .observations
            .iter()
            .flatten()
            .count(),
        1
    );

    for connected in connections.into_iter().skip(1) {
        let (version, adverts) =
            exchange(handshake.clone(), connected, "45.33.32.1:49152", true).await;
        assert!(version.address_from.addr().ip().is_unspecified());
        assert!(adverts.is_empty());
    }
    for (reporter, report) in [
        ("9.9.9.9:8233", "[2606:4700::1111]:49152"),
        ("[2001:4860::8888]:8233", "45.33.32.1:49152"),
    ] {
        let (version, adverts) = exchange(
            handshake.clone(),
            ConnectedAddr::new_outbound_direct(reporter.parse().unwrap()),
            report,
            true,
        )
        .await;
        assert!(version.address_from.addr().ip().is_unspecified());
        assert!(adverts.is_empty());
    }
    exchange(
        handshake.clone(),
        ConnectedAddr::new_outbound_direct("9.9.9.9:8233".parse().unwrap()),
        "192.168.1.1:49152",
        true,
    )
    .await;
    assert_eq!(
        handshake
            .external_addrs
            .lock()
            .await
            .observations
            .iter()
            .flatten()
            .count(),
        1
    );
}

#[tokio::test]
async fn discovery_wire_configuration_overrides_reports() {
    let _guard = zebra_test::init();
    for external in [None, Some("1.1.1.1:18235".parse().unwrap())] {
        let listener: SocketAddr = "93.184.216.34:18234".parse().unwrap();
        let handshake = handshake(Config {
            listen_addr: listener,
            external_addr: external,
            ..Config::default()
        });
        let expected = external.unwrap_or(listener);
        let (version, adverts) = exchange(
            handshake.clone(),
            ConnectedAddr::new_outbound_direct("8.8.8.8:8233".parse().unwrap()),
            "45.33.32.1:49152",
            true,
        )
        .await;
        assert_eq!(*version.address_from.addr(), expected);
        assert_eq!(adverts.len(), 1);
        assert_eq!(*adverts[0].addr(), expected);
        let (version, adverts) = exchange(
            handshake,
            ConnectedAddr::new_outbound_proxy(
                "8.8.8.8:9050".parse().unwrap(),
                "127.0.0.1:49152".parse().unwrap(),
            ),
            "45.33.32.1:49152",
            true,
        )
        .await;
        assert!(version.address_from.addr().ip().is_unspecified());
        assert!(adverts.is_empty());
    }
}
