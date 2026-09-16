//! Property tests for find-response feedback observation and ordering.

use proptest::prelude::*;
use tokio::sync::mpsc::error::TryRecvError;

use super::{
    test_addr, FindRequestId, FindResponseEvent, FindResponseFeedback, FindResponseOutcome,
    FindResponseStallTracker,
};

proptest! {
    /// Keeps feedback pending until the final unclassified owner is dropped.
    #[test]
    fn dropping_non_final_feedback_clone_keeps_pending(clone_count in 0usize..10) {
        let _test_guard = zebra_test::init();
        let (feedback, mut observer) = FindResponseFeedback::new_for_test();
        let clones = vec![feedback.clone(); clone_count];

        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));

        for clone in clones {
            drop(clone);

            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        drop(feedback);

        prop_assert_eq!(observer.try_outcome(), Ok(None));
        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }

    /// Applies outcomes in request order regardless of their arrival order.
    ///
    /// The useful response resets prior stalls, and the neutral response must
    /// preserve the count. Only receiving all five outcomes reaches the threshold.
    #[test]
    fn find_response_outcomes_are_ordered(
        responses in Just(vec![
            (0, FindResponseOutcome::Useful),
            (1, FindResponseOutcome::Stalled),
            (2, FindResponseOutcome::Unclassified),
            (3, FindResponseOutcome::Stalled),
            (4, FindResponseOutcome::Stalled),
        ]).prop_shuffle(),
    ) {
        let _test_guard = zebra_test::init();
        let mut tracker = FindResponseStallTracker::new();
        let addr = test_addr(1);

        // Start one stall below disconnection; the useful response must reset this count.
        prop_assert!(!tracker.record_stall(addr));
        prop_assert!(!tracker.record_stall(addr));

        // Register requests in order: useful, stalled, neutral, stalled, stalled.
        for request_id in 0..5 {
            tracker.begin_request(addr, FindRequestId::from(request_id));
        }

        // Deliver outcomes in shuffled order; only the final arrival can release all three stalls.
        for (index, (request_id, outcome)) in responses.into_iter().enumerate() {
            prop_assert_eq!(
                tracker.record_response(FindResponseEvent::new(
                    addr,
                    FindRequestId::from(request_id),
                    outcome,
                )),
                index == 4,
            );
        }

        // Removing a peer discards pending responses from its old connection.
        let stale_request = FindRequestId::from(5);
        tracker.begin_request(addr, stale_request);
        tracker.clear(addr);

        prop_assert!(!tracker.record_response(FindResponseEvent::new(
            addr,
            stale_request,
            FindResponseOutcome::Stalled,
        )));
        prop_assert!(!tracker.record_stall(addr));
    }
}
