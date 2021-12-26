//! Fixed test vectors for isolated Zebra connections.

use super::super::*;

#[tokio::test]
async fn connect_isolated_sends_minimally_distinguished_version_message() {
    use std::net::SocketAddr;

    use futures::stream::StreamExt;
    use tokio_util::codec::Framed;

    use crate::{
        protocol::external::{AddrInVersion, Codec, Message},
        types::PeerServices,
    };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen_addr = listener.local_addr().unwrap();

    let fixed_isolated_addr: SocketAddr = "0.0.0.0:8233".parse().unwrap();

    let conn = tokio::net::TcpStream::connect(listen_addr).await.unwrap();

    tokio::spawn(connect_isolated(conn, "".to_string()));

    let (conn, _) = listener.accept().await.unwrap();

    let mut stream = Framed::new(conn, Codec::builder().finish());
    if let Message::Version {
        services,
        timestamp,
        address_from,
        user_agent,
        start_height,
        relay,
        ..
    } = stream
        .next()
        .await
        .expect("stream item")
        .expect("item is Ok(msg)")
    {
        // Check that the version message sent by connect_isolated
        // has the fields specified in the Stolon RFC.
        assert_eq!(services, PeerServices::empty());
        assert_eq!(timestamp.timestamp() % (5 * 60), 0);
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
