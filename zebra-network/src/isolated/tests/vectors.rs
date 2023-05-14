//! Fixed test vectors for isolated Zebra connections.

use std::{task::Poll, time::Duration};

use futures::stream::StreamExt;
use tokio_util::codec::Framed;

use crate::{
    constants::CURRENT_NETWORK_PROTOCOL_VERSION,
    protocol::external::{AddrInVersion, Codec, Message},
    types::PeerServices,
    VersionMessage,
};

use super::super::*;

use Network::*;

/// Test that `connect_isolated` sends a version message with minimal distinguishing features,
/// when sent over TCP.
#[tokio::test]
async fn connect_isolated_sends_anonymised_version_message_tcp() {
    let _init_guard = zebra_test::init();

    if zebra_test::net::zebra_skip_network_tests() {
        return;
    }

    connect_isolated_sends_anonymised_version_message_tcp_net(Mainnet).await;
    connect_isolated_sends_anonymised_version_message_tcp_net(Testnet).await;
}

async fn connect_isolated_sends_anonymised_version_message_tcp_net(network: Network) {
    // These tests might fail on machines with no configured IPv4 addresses.
    // (Localhost should be enough.)

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen_addr = listener.local_addr().unwrap();

    // Connection errors are detected using the JoinHandle.
    // (They might also make the test hang.)
    let mut outbound_join_handle = tokio::spawn(connect_isolated_tcp_direct(
        network,
        listen_addr,
        "".to_string(),
    ));

    let (inbound_conn, _) = listener.accept().await.unwrap();

    let mut inbound_stream =
        Framed::new(inbound_conn, Codec::builder().for_network(network).finish());

    check_version_message(network, &mut inbound_stream).await;

    // Let the spawned task run for a short time.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Make sure that the isolated connection did not:
    // - panic, or
    // - return a service.
    //
    // This test doesn't send a version message on `inbound_conn`,
    // so providing a service is incorrect behaviour.
    //
    // A timeout error would be acceptable,
    // but a TCP connection error indicates a potential test setup issue.
    // So we fail on them both, because we expect this test to complete before the timeout.
    let outbound_result = futures::poll!(&mut outbound_join_handle);
    assert!(matches!(outbound_result, Poll::Pending));

    outbound_join_handle.abort();
}

/// Test that `connect_isolated` sends a version message with minimal distinguishing features,
/// when sent in-memory.
///
/// This test also:
/// - checks `PeerTransport` support, and
/// - runs even if network tests are disabled.
#[tokio::test]
async fn connect_isolated_sends_anonymised_version_message_mem() {
    let _init_guard = zebra_test::init();

    connect_isolated_sends_anonymised_version_message_mem_net(Mainnet).await;
    connect_isolated_sends_anonymised_version_message_mem_net(Testnet).await;
}

async fn connect_isolated_sends_anonymised_version_message_mem_net(network: Network) {
    // We expect version messages to be ~100 bytes
    let (inbound_stream, outbound_stream) = tokio::io::duplex(1024);

    let mut outbound_join_handle =
        tokio::spawn(connect_isolated(network, outbound_stream, "".to_string()));

    let mut inbound_stream = Framed::new(
        inbound_stream,
        Codec::builder().for_network(network).finish(),
    );

    check_version_message(network, &mut inbound_stream).await;

    // Let the spawned task run for a short time.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Make sure that the isolated connection did not:
    // - panic, or
    // - return a service.
    //
    // This test doesn't send a version message on `inbound_conn`,
    // so providing a service is incorrect behaviour.
    // (But a timeout error would be acceptable.)
    let outbound_result = futures::poll!(&mut outbound_join_handle);
    assert!(matches!(
        outbound_result,
        Poll::Pending | Poll::Ready(Ok(Err(_)))
    ));

    outbound_join_handle.abort();
}

/// Wait to receive a version message on `inbound_stream`,
/// then check that it is correctly anonymised.
async fn check_version_message<PeerTransport>(
    network: Network,
    inbound_stream: &mut Framed<PeerTransport, Codec>,
) where
    PeerTransport: AsyncRead + Unpin,
{
    // We don't need to send any bytes to get a version message.
    if let Message::Version(VersionMessage {
        version,
        services,
        timestamp,
        address_recv,
        address_from,
        nonce: _,
        user_agent,
        start_height,
        relay,
    }) = inbound_stream
        .next()
        .await
        .expect("stream item")
        .expect("item is Ok(msg)")
    {
        // Check that the version message sent by connect_isolated
        // anonymises all the fields that it possibly can.
        //
        // The version field needs to be accurate, because it controls protocol features.
        // The nonce must be randomised for security.
        //
        // SECURITY TODO: check if the timestamp field can be zeroed, to remove another distinguisher (#3300)

        let mut fixed_isolated_addr: PeerSocketAddr = "0.0.0.0:0".parse().unwrap();
        fixed_isolated_addr.set_port(network.default_port());

        // Required fields should be accurate and match most other peers.
        // (We can't test nonce randomness here.)
        assert_eq!(version, CURRENT_NETWORK_PROTOCOL_VERSION);
        assert_eq!(timestamp.timestamp() % (5 * 60), 0);

        // Other fields should be empty or zeroed.
        assert_eq!(services, PeerServices::empty());
        assert_eq!(
            address_recv,
            // Since we're connecting to the peer, we expect it to have the node flag.
            //
            // SECURITY TODO: should this just be zeroed anyway? (#3300)
            AddrInVersion::new(fixed_isolated_addr, PeerServices::NODE_NETWORK),
        );
        assert_eq!(
            address_from,
            AddrInVersion::new(fixed_isolated_addr, PeerServices::empty()),
        );
        assert_eq!(user_agent, "");
        assert_eq!(start_height.0, 0);
        assert!(!relay);
    } else {
        panic!("handshake did not send version message");
    }
}
