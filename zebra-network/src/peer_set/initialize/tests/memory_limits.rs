//! Tests for the memory limits [`init_with_limits`] applies.

use std::{net::SocketAddr, time::Duration};

use indexmap::IndexSet;
use tower::service_fn;

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    config::CacheDir,
    peer_set::initialize::{init_with_limits, MemoryLimits},
    protocol::types::PeerServices,
    quic::{self, tests::assert_peer_grants_stream_window, QuicWindows},
    BoxError, Config, Response,
};

/// Every loopback exchange in these tests finishes in well under this.
const TEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The default limits are today's network: the configured peer set target size, and the default
/// QUIC windows.
#[test]
fn default_memory_limits_keep_the_configured_target_and_the_default_windows() {
    assert_eq!(
        MemoryLimits::default(),
        MemoryLimits {
            peerset_initial_target_size: None,
            quic: QuicWindows::default(),
        }
    );
}

/// A config for a node that makes no outbound connections and listens on loopback, with its
/// version 2 endpoint on `v2_listen_addr`.
fn loopback_config(v2_listen_addr: Option<SocketAddr>) -> Config {
    Config {
        listen_addr: "127.0.0.1:0".parse().expect("valid socket address"),
        network: Network::Mainnet,
        initial_mainnet_peers: IndexSet::new(),
        initial_testnet_peers: IndexSet::new(),
        cache_dir: CacheDir::disabled(),
        v2_listen_addr,
        ..Config::default()
    }
}

/// Starts a loopback node with `limits` and its version 2 endpoint enabled, and checks that a
/// peer that dials it is granted `stream_receive_window`.
async fn assert_v2_endpoint_grants(limits: MemoryLimits, stream_receive_window: u32) {
    // The peer must know the node's QUIC port before it dials; probe a free UDP port.
    let v2_listen_addr = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").expect("a UDP port is free");
        probe.local_addr().expect("a bound socket has an address")
    };

    let (_peer_set, _address_book, _misbehavior_tx, _advertiser) = init_with_limits(
        loopback_config(Some(v2_listen_addr)),
        PeerServices::NODE_NETWORK,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        NoChainTip,
        "memory limits test".to_string(),
        Vec::new(),
        limits,
    )
    .await;

    let peer = quic::bind_with_windows(
        &Network::Mainnet,
        "127.0.0.1:0".parse().expect("valid"),
        QuicWindows::default(),
    )
    .expect("binds");
    let connection = quic::connect(&peer, v2_listen_addr)
        .await
        .expect("the node's version 2 endpoint accepts the connection");
    assert_peer_grants_stream_window(&connection, stream_receive_window).await;
}

/// [`init_with_limits`] opens the version 2 endpoint with the limits' QUIC windows, so a peer
/// that dials it is granted the compact stream receive window, not the default one, and raises
/// windows below the compact ones to them.
#[tokio::test]
async fn init_with_limits_opens_the_v2_endpoint_with_their_windows() {
    let _init_guard = zebra_test::init();
    let compact = QuicWindows::compact();
    let below_compact = QuicWindows {
        stream_receive_window: compact.stream_receive_window / 2,
        connection_receive_window: compact.stream_receive_window / 2,
        send_window: compact.send_window / 2,
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        for quic in [compact, below_compact] {
            let limits = MemoryLimits {
                quic,
                ..MemoryLimits::default()
            };
            assert_v2_endpoint_grants(limits, compact.stream_receive_window).await;
        }
    })
    .await
    .expect("the test completes well within the timeout");
}
