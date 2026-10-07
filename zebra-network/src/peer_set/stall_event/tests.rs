//! Tests for connection-owned stall tracking notifications.

use std::net::SocketAddr;

use tokio::sync::mpsc;

use super::{ConnectionGuard, ConnectionId, PeerStallEvent};

/// Dropping the connection guard reports closure even while response senders survive.
#[test]
fn dropping_connection_guard_reports_closure() {
    let _test_guard = zebra_test::init();
    let peer = "127.0.0.1:8233".parse::<SocketAddr>().unwrap().into();
    let connection_id = ConnectionId::from(1);
    let (response_sender, mut receiver) = mpsc::unbounded_channel();
    let guard = ConnectionGuard::new(peer, connection_id, response_sender.clone());

    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    drop(guard);

    assert!(
        matches!(
            receiver.try_recv(),
            Ok(PeerStallEvent::ConnectionClosed { peer: closed_peer, connection_id: closed_id })
                if closed_peer == peer && closed_id == connection_id
        ),
        "dropping the guard must report its connection's closure"
    );
    assert!(
        matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
        "the surviving response sender must not cause a second closure event"
    );
    drop(response_sender);
}
