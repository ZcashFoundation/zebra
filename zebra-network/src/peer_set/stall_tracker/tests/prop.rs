//! Property tests for find-response feedback observation and ordering.

use proptest::prelude::*;
use tokio::sync::mpsc::error::TryRecvError;

use super::FindResponseFeedback;

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
}
