//! Property tests for connection-owned stall tracking notifications.

use proptest::prelude::*;
use tokio::sync::mpsc;

use crate::PeerSocketAddr;

use super::super::{ConnectionGuard, ConnectionId, PeerStallEvent};

proptest! {
    /// Dropping a guard reports its peer and connection identity while other senders survive.
    #[test]
    fn dropping_connection_guard_reports_closure(
        peer in any::<PeerSocketAddr>(),
        connection_id in any::<ConnectionId>(),
    ) {
        let _test_guard = zebra_test::init();

        let (response_sender, mut receiver) = mpsc::unbounded_channel();
        let guard = ConnectionGuard::new(peer, connection_id, response_sender.clone());

        prop_assert_eq!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        );

        drop(guard);

        prop_assert_eq!(
            receiver.try_recv(),
            Ok(PeerStallEvent::ConnectionClosed {
                peer,
                connection_id,
            }),
            "dropping the guard must report its connection's closure"
        );
        prop_assert_eq!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty),
            "no additional closure event should be queued while the channel remains open"
        );
    }
}
