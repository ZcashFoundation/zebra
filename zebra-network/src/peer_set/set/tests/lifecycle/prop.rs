//! Property tests for connection-scoped stall tracking.

use std::time::Duration;

use proptest::prelude::*;
use tokio::time::timeout;

use zebra_chain::parameters::{Network, NetworkUpgrade};

use crate::{
    peer::PeerError,
    peer_set::{stall_tracker::FIND_RESPONSE_STALL_THRESHOLD, PeerStallEvent},
    protocol::external::types::Version,
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

    /// A late guard drop does not erase stalls accumulated by a replacement connection.
    ///
    /// The intervening event must preserve every nonzero pre-threshold stall count.
    #[test]
    fn delayed_old_closure_preserves_replacement_stalls(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let _original = test.connect();
            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

            // Hold the old service outside the ready map to delay its guard's drop.
            let old_service = test
                .peer_set
                .take_ready_service(&test.address)
                .ok_or_else(|| TestCaseError::fail("expected the original ready connection"))?;

            let mut replacement = test.connect();
            test.stall_without_disconnecting(&mut replacement, initial_stalls).await?;

            drop(old_service);
            test.poll();

            test.stall_without_disconnecting(
                &mut replacement,
                FIND_RESPONSE_STALL_THRESHOLD - initial_stalls - 1,
            ).await?;

            test.stall(&mut replacement)
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            test.poll();
            prop_assert!(
                !replacement.wants_connection_heartbeats(),
                "the connection must disconnect at the stall threshold",
            );

            Ok(())
        })?;
    }

    /// Rejecting a duplicate connection preserves the admitted connection's stall count.
    ///
    /// The intervening event must preserve every nonzero pre-threshold stall count.
    #[test]
    fn rejected_duplicate_preserves_current_stalls(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.stall_without_disconnecting(&mut original, initial_stalls).await?;

            let mut duplicate = test.connect();
            // Reject the duplicate before attaching a connection guard.
            test.poll();
            // Allow any erroneous closure event from rejecting the duplicate to be processed.
            test.poll();
            prop_assert!(
                !duplicate.wants_connection_heartbeats(),
                "the duplicate was rejected"
            );
            prop_assert!(
                original.wants_connection_heartbeats(),
                "the admitted connection remains"
            );

            test.stall_without_disconnecting(
                &mut original,
                FIND_RESPONSE_STALL_THRESHOLD - initial_stalls - 1,
            ).await?;

            test.stall(&mut original)
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            test.poll();
            prop_assert!(
                !original.wants_connection_heartbeats(),
                "the connection must disconnect at the stall threshold",
            );

            Ok(())
        })?;
    }

    /// An errored unready service clears tracking when its readiness future drops it.
    ///
    /// Every nonzero pre-threshold history must reset to a full fresh stall allowance.
    #[test]
    fn unready_error_clears_tracking(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.stall_without_disconnecting(&mut original, initial_stalls).await?;
            let _response = test.request()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

            original.set_error(PeerError::ConnectionClosed);
            // Drop the errored unready service, queuing its connection-closure event.
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

    /// A banned ready service releases its connection tracking through the drop guard.
    ///
    /// Every nonzero pre-threshold history must reset to a full fresh stall allowance.
    #[test]
    fn ready_ban_clears_tracking(
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

            test.ban();
            // Drop the banned ready service, queuing its connection-closure event.
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

    /// A banned unready service releases tracking when it becomes ready and is rejected.
    ///
    /// Every nonzero pre-threshold history must reset to a full fresh stall allowance.
    #[test]
    fn unready_ban_clears_tracking(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let mut original = test.connect();
            test.stall_without_disconnecting(&mut original, initial_stalls).await?;

            // Leave classification unpolled even after the mock sends its response.
            let _response = test.request()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;
            original
                .try_to_receive_outbound_client_request()
                .request()
                .ok_or_else(|| TestCaseError::fail("expected an outbound find request"))?
                .tx
                .send(Ok(request_kind.empty_response()))
                .map_err(|_| TestCaseError::fail("the find response receiver was dropped"))?;

            test.ban();
            // Drop the newly ready banned service, queuing its connection-closure event.
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

    /// Version pruning clears the tracking of a ready connection without explicit removal.
    ///
    /// Every nonzero pre-threshold history must reset to a full fresh stall allowance.
    #[test]
    fn version_pruning_clears_tracking(
        request_kind in any::<FindRequestKind>(),
        initial_stalls in 1..FIND_RESPONSE_STALL_THRESHOLD,
    ) {
        let (runtime, _test_guard) = zebra_test::init_async();

        runtime.block_on(async move {
            let mut test = TestScenario::new(request_kind);

            let version =
                Version::min_specified_for_upgrade(&Network::Mainnet, NetworkUpgrade::Nu6_2);
            test.chain_tip
                .send_best_tip_height(NetworkUpgrade::Nu6_2.activation_height(&Network::Mainnet));

            let mut original = test.connect_with_version(version);
            test.stall_without_disconnecting(&mut original, initial_stalls).await?;
            test.ready()
                .await
                .map_err(|error| TestCaseError::fail(format!("{error:?}")))?;

            test.chain_tip
                .send_best_tip_height(NetworkUpgrade::Nu6_3.activation_height(&Network::Mainnet));
            // Prune the outdated ready service, queuing its connection-closure event.
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
