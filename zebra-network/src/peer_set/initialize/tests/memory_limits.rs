//! Tests for the memory limits [`init_with_limits`] applies.

use std::{net::SocketAddr, time::Duration};

use indexmap::IndexSet;
use tower::service_fn;

use zebra_chain::{chain_tip::NoChainTip, parameters::Network};

use crate::{
    address_book_updater::MIN_CHANNEL_SIZE,
    config::CacheDir,
    constants::{
        DEFAULT_PEERSET_INITIAL_TARGET_SIZE, INBOUND_PEER_LIMIT_MULTIPLIER,
        OUTBOUND_PEER_LIMIT_MULTIPLIER,
    },
    peer_set::initialize::{init_with_limits, MemoryLimits},
    protocol::types::PeerServices,
    quic::{self, tests::assert_peer_grants_stream_window, QuicWindows},
    BoxError, Config, Response, V2Endpoint, MIN_V2_SYNC_OUTBOUND_CONNECTIONS,
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

/// Starts a loopback node from `config` with `limits`, and returns the capacity of its
/// misbehavior channel, which is its total connection limit, or at least
/// [`MIN_CHANNEL_SIZE`].
async fn misbehavior_capacity(config: Config, limits: MemoryLimits) -> usize {
    let (_peer_set, _address_book, misbehavior_tx, _advertiser) = init_with_limits(
        config,
        PeerServices::NODE_NETWORK,
        service_fn(|_| async { Ok::<Response, BoxError>(Response::Nil) }),
        NoChainTip,
        "memory limits test".to_string(),
        Vec::new(),
        limits,
    )
    .await;

    misbehavior_tx.max_capacity()
}

/// [`init_with_limits`] applies the limits' peer set target size before it sizes anything from
/// the connection limits, whatever the configured target size is.
#[tokio::test]
async fn init_with_limits_scales_the_connection_limits_by_their_target_size() {
    let _init_guard = zebra_test::init();
    let target = |peerset_initial_target_size| MemoryLimits {
        peerset_initial_target_size: Some(peerset_initial_target_size),
        ..MemoryLimits::default()
    };
    let connections = |peerset_initial_target_size: usize| {
        let limit = peerset_initial_target_size
            * (OUTBOUND_PEER_LIMIT_MULTIPLIER + INBOUND_PEER_LIMIT_MULTIPLIER);
        limit.max(MIN_CHANNEL_SIZE)
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        assert_eq!(
            misbehavior_capacity(loopback_config(None), MemoryLimits::default()).await,
            connections(DEFAULT_PEERSET_INITIAL_TARGET_SIZE),
            "the default limits keep the default target size"
        );
        assert_eq!(
            misbehavior_capacity(loopback_config(None), target(2)).await,
            connections(2),
            "the limits replace the default target size"
        );

        let configured = Config {
            peerset_initial_target_size: 3,
            ..loopback_config(None)
        };
        assert_eq!(
            misbehavior_capacity(configured, target(2)).await,
            connections(2),
            "the limits replace a configured target size too: their caller decides which wins"
        );
    })
    .await
    .expect("the test completes well within the timeout");
}

/// [`init_with_limits`] logs the connection limits and windows it uses once: at info level when
/// the limits change them, and at debug level when they do not. It warns when it raises windows
/// below the compact ones.
#[tokio::test]
#[tracing_test::traced_test]
// No `zebra_test::init()`: `traced_test` installs the subscriber that records the logs.
async fn init_with_limits_logs_the_limits_it_applies() {
    let message = "network connection limits, and version 2 flow-control windows in bytes";
    let has_line = |level: &'static str, message: &'static str, fields: &'static str| {
        move |lines: &[&str]| {
            let matching = lines
                .iter()
                .filter(|line| line.contains(message) && line.contains(fields))
                .collect::<Vec<_>>();
            match matching.as_slice() {
                [line] if line.contains(level) => Ok(()),
                other => Err(format!(
                    "expected one {level} line with {fields}, got {other:?}"
                )),
            }
        }
    };

    tokio::time::timeout(TEST_TIMEOUT, async {
        misbehavior_capacity(loopback_config(None), MemoryLimits::default()).await;
        if tracing::level_enabled!(tracing::Level::DEBUG) {
            logs_assert(has_line(
                "DEBUG",
                message,
                "peerset_initial_target_size=25 outbound_connection_limit=75",
            ));
        }

        let configured = Config {
            peerset_initial_target_size: 5,
            ..loopback_config(None)
        };
        let unchanged = MemoryLimits {
            peerset_initial_target_size: Some(5),
            ..MemoryLimits::default()
        };
        misbehavior_capacity(configured, unchanged).await;
        if tracing::level_enabled!(tracing::Level::DEBUG) {
            logs_assert(has_line(
                "DEBUG",
                message,
                "peerset_initial_target_size=5 outbound_connection_limit=15",
            ));
        }

        let compact = MemoryLimits {
            peerset_initial_target_size: Some(8),
            quic: QuicWindows::compact(),
        };
        misbehavior_capacity(loopback_config(None), compact).await;
        logs_assert(has_line(
            " INFO ",
            message,
            "peerset_initial_target_size=8 outbound_connection_limit=24 \
             inbound_connection_limit=40 v2_outbound_connection_limit=0 \
             v2_inbound_connection_limit=0 stream_receive_window=2228224 \
             connection_receive_window=4456448 send_window=3145728",
        ));

        let below_compact = MemoryLimits {
            peerset_initial_target_size: Some(9),
            quic: QuicWindows {
                stream_receive_window: 1,
                connection_receive_window: 1,
                send_window: 1,
            },
        };
        misbehavior_capacity(loopback_config(None), below_compact).await;
        logs_assert(has_line(
            " WARN ",
            "raising the version 2 flow-control windows below their minimum",
            "stream_receive_window: 1,",
        ));
        logs_assert(has_line(
            " INFO ",
            message,
            "peerset_initial_target_size=9 outbound_connection_limit=27 \
             inbound_connection_limit=45 v2_outbound_connection_limit=0 \
             v2_inbound_connection_limit=0 stream_receive_window=2228224 \
             connection_receive_window=4456448 send_window=3145728",
        ));
    })
    .await
    .expect("the test completes well within the timeout");
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

/// The budget charges the actual legacy codec reservation and two largest v2 request bodies per
/// connection in either direction, and every v2 connection's transport windows.
#[test]
fn buffered_request_bytes_use_the_protocol_limits() {
    const MIB: u64 = 1024 * 1024;
    let compact = QuicWindows::compact();

    assert_eq!(
        crate::protocol::v2::request::max_body_len(crate::protocol::v2::types::StreamType::GetTx,),
        Some(3_250_003)
    );
    assert_eq!(
        Config::buffered_request_bytes_for_target(8, V2Endpoint::Enabled, compact),
        (40 + 24) * (2 * MIB + 24)
            + (20 + 12) * (2 * 3_250_003)
            + (20 + 12) * (2 * 2_228_224 + 3 * MIB)
    );
    assert_eq!(
        Config::buffered_request_bytes_for_target(8, V2Endpoint::Enabled, compact),
        585_489_088,
        "64 legacy and 32 v2 peers retain 342,219,456 body bytes; \
         the 32 v2 connections add 243,269,632 transport bytes"
    );
    assert_eq!(
        Config::buffered_request_bytes_for_target(8, V2Endpoint::Disabled, compact),
        134_219_264,
        "without v2, only the 64 legacy reservations are charged"
    );
}

/// The floor covers one target, targets grow monotonically, and windows grow only when the same
/// target's larger charge also fits.
#[test]
fn budgeted_limits_fit_their_buffered_request_bytes() {
    const MIB: u64 = 1024 * 1024;
    let minimum = MemoryLimits::minimum_budget_bytes(V2Endpoint::Enabled);
    assert_eq!(minimum, 59_083_954);
    assert_eq!(minimum.div_ceil(MIB), 57);
    assert_eq!(
        Config::peer_target_for_memory(minimum - 1, V2Endpoint::Enabled, QuicWindows::compact()),
        None
    );

    for (share, serves_artifacts, peers, windows) in [
        (57 * MIB, true, 1, QuicWindows::compact()),
        (80_317_618, false, 1, QuicWindows::DEFAULT),
        (80_317_618, true, 1, QuicWindows::compact()),
        (86_609_074, true, 1, QuicWindows::SERVING),
        (146_372_272, true, 2, QuicWindows::compact()),
        (512 * MIB, true, 7, QuicWindows::compact()),
    ] {
        let limits = MemoryLimits::for_budget(share, V2Endpoint::Enabled, serves_artifacts);
        assert_eq!(limits.peerset_initial_target_size, Some(peers), "{share}");
        assert_eq!(limits.quic, windows, "{share}");
        assert!(
            Config::buffered_request_bytes_for_target(peers, V2Endpoint::Enabled, limits.quic)
                <= share,
            "the derived limits outspend {share}: {limits:?}"
        );
        if peers < DEFAULT_PEERSET_INITIAL_TARGET_SIZE {
            let next = Config::buffered_request_bytes_for_target(
                peers + 1,
                V2Endpoint::Enabled,
                QuicWindows::compact(),
            );
            assert!(next > share, "a larger target fits at {share}: {next}");
        }
    }
}

/// With the checkpoint sync engine, a peer target of one charges and allows
/// [`MIN_V2_SYNC_OUTBOUND_CONNECTIONS`] outbound version 2 connections, so the engine has the
/// peers it streams from; larger targets already allow that many, and charge the same.
#[test]
fn checkpoint_sync_floors_the_v2_outbound_connections() {
    const MIB: u64 = 1024 * 1024;
    let compact = QuicWindows::compact();
    // Two largest request bodies and the compact transport windows.
    let v2_connection = 2 * 3_250_003 + (2 * 2_228_224 + 3 * MIB);

    let minimum = MemoryLimits::minimum_budget_bytes(V2Endpoint::CheckpointSync);
    assert_eq!(
        minimum,
        MemoryLimits::minimum_budget_bytes(V2Endpoint::Enabled) + v2_connection,
        "the engine's second outbound v2 connection is charged"
    );
    assert_eq!(minimum, 73_186_136);
    assert_eq!(minimum.div_ceil(MIB), 70);
    assert_eq!(
        Config::peer_target_for_memory(minimum - 1, V2Endpoint::CheckpointSync, compact),
        None
    );
    assert_eq!(
        MemoryLimits::for_budget(minimum, V2Endpoint::CheckpointSync, true)
            .peerset_initial_target_size,
        Some(1)
    );

    for target in 2..=DEFAULT_PEERSET_INITIAL_TARGET_SIZE {
        assert_eq!(
            Config::buffered_request_bytes_for_target(target, V2Endpoint::CheckpointSync, compact),
            Config::buffered_request_bytes_for_target(target, V2Endpoint::Enabled, compact),
            "target {target} already allows the engine's outbound connections"
        );
    }

    assert_eq!(MIN_V2_SYNC_OUTBOUND_CONNECTIONS, 2);
}
