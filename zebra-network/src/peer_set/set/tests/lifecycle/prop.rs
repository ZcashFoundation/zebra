//! Property tests for connection-scoped stall tracking.

use std::time::Duration;

use proptest::prelude::*;
use tokio::time::timeout;

use crate::{
    peer::PeerError,
    peer_set::{stall_tracker::FIND_RESPONSE_STALL_THRESHOLD, PeerStallEvent},
};

use super::{FindRequestKind, TestScenario};

proptest! {
    /// A closure event for the tracked connection clears its identity and stall history.
    #[test]
    fn matching_connection_closure_clears_tracking(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

            let connection_id = *test
                .peer_set
                .tracked_connections
                .get(&test.address)
                .ok_or_else(|| TestCaseError::fail("the admitted connection must be tracked"))?;

            test.stall_without_disconnecting(&mut original, initial_stalls).await?;

            // Inject the event to test cleanup independently of service-owned guards.
            test.peer_set
                .stall_event_tx
                .send(PeerStallEvent::ConnectionClosed {
                    peer: test.address,
                    connection_id,
                })
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            test.poll();

            prop_assert!(
                !test.peer_set.tracked_connections.contains_key(&test.address),
                "a matching closure event must remove the tracked connection identity",
            );

            test.assert_fresh_stall_allowance()?;

            Ok(())
        })?;
    }

    /// A response future from a removed connection cannot stall its replacement.
    #[test]
    fn delayed_old_response_cannot_stall_replacement(
        request_kind in any::<FindRequestKind>(),
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();

            let old_response = test.request()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            original
                .try_to_receive_outbound_client_request()
                .request()
                .ok_or_else(|| TestCaseError::fail("expected an outbound find request"))?
                .tx
                .send(Ok(request_kind.empty_response()))
                .map_err(|_| TestCaseError::fail("the find response receiver was dropped"))?;

            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            original.set_error(PeerError::ConnectionClosed);
            test.poll();
            prop_assert!(!original.wants_connection_heartbeats());

            let mut replacement = test.connect();
            test.stall_without_disconnecting(&mut replacement, FIND_RESPONSE_STALL_THRESHOLD - 1)
                .await?;

            timeout(Duration::from_secs(5), old_response)
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            test.poll();
            prop_assert!(
                replacement.wants_connection_heartbeats(),
                "a delayed old response must not count as a replacement stall"
            );

            Ok(())
        })?;
    }

    /// A replacement connection does not inherit stalls from an errored ready service.
    ///
    /// New stalls reach the disconnection threshold only if the old count survives.
    #[test]
    fn reconnect_after_ready_error_starts_without_stalls(
        request_kind in any::<FindRequestKind>(),
        original_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.stall_without_disconnecting(&mut original, original_stalls).await?;

            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            original.set_error(PeerError::ConnectionClosed);
            test.poll();
            prop_assert!(
                !original.wants_connection_heartbeats(),
                "the failed connection was removed"
            );

            let mut replacement = test.connect();
            test.stall_without_disconnecting(&mut replacement, FIND_RESPONSE_STALL_THRESHOLD - 1)
                .await?;

            Ok(())
        })?;
    }

    /// An errored ready service releases its identity and stalls without waiting for reconnect.
    ///
    /// Every nonzero pre-threshold history must reset to a full fresh stall allowance.
    #[test]
    fn ready_error_clears_tracking_without_reconnection(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.stall_without_disconnecting(&mut original, initial_stalls).await?;
            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

            original.set_error(PeerError::ConnectionClosed);
            // Drop the errored ready service, queuing its connection-closure event.
            test.poll();
            // Process the closure event to clear the connection's identity and stalls.
            test.poll();

            prop_assert!(!original.wants_connection_heartbeats());
            prop_assert!(!test
                .peer_set
                .tracked_connections
                .contains_key(&test.address));

            test.assert_fresh_stall_allowance()?;

            Ok(())
        })?;
    }
}
