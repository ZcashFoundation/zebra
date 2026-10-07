//! Property tests for connection-scoped stall tracking.

use proptest::prelude::*;

use crate::{peer::PeerError, peer_set::stall_tracker::FIND_RESPONSE_STALL_THRESHOLD};

use super::{FindRequestKind, TestScenario};

proptest! {
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
}
