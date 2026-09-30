//! Property tests for find-response feedback ownership and release.

use proptest::prelude::*;
use tokio::sync::mpsc::{self, error::TryRecvError};

use super::super::{
    test_addr, FindRequestId, FindResponseEvent, FindResponseFeedback, FindResponseOutcome,
};

proptest! {
    /// Explicit classification releases feedback even while other clones remain alive.
    #[test]
    fn first_classification_releases_feedback_immediately(
        first_feedback in FeedbackAction::mark_useful_or_stalled(),
        late_conflicting_feedbacks in proptest::collection::vec(FeedbackAction::any(), 1..10),
    ) {
        let _test_guard = zebra_test::init();

        let addr = test_addr(1);
        let request_id = FindRequestId::from(1);
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let feedback = FindResponseFeedback::new(addr, request_id, sender);

        let late_feedbacks: Vec<_> = late_conflicting_feedbacks
            .into_iter()
            .map(|action| (action, feedback.clone()))
            .collect();

        let expected_outcome = first_feedback.expected_outcome();

        first_feedback.apply_to(feedback);

        prop_assert_eq!(
            receiver.try_recv(),
            Ok(FindResponseEvent::new(addr, request_id, expected_outcome)),
        );
        prop_assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));

        for (action, feedback) in late_feedbacks {
            action.apply_to(feedback);

            prop_assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
        }
    }

    /// Only the final unclassified owner releases feedback and closes the channel.
    #[test]
    fn only_final_drop_releases_feedback(clone_count in 0usize..10) {
        let _test_guard = zebra_test::init();

        let addr = test_addr(1);
        let request_id = FindRequestId::from(1);
        let (sender, mut receiver) = mpsc::unbounded_channel();

        let feedback = FindResponseFeedback::new(addr, request_id, sender);
        let clones = vec![feedback.clone(); clone_count];

        prop_assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

        for feedback in clones {
            FeedbackAction::Drop.apply_to(feedback);

            prop_assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        }

        FeedbackAction::Drop.apply_to(feedback);

        prop_assert_eq!(
            receiver.try_recv(),
            Ok(FindResponseEvent::new(
                addr,
                request_id,
                FeedbackAction::Drop.expected_outcome(),
            )),
        );
        prop_assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
    }
}

/// An action taken by one owner of a feedback capability.
#[derive(Clone, Copy, Debug)]
enum FeedbackAction {
    MarkUseful,
    MarkStalled,
    Drop,
}

impl FeedbackAction {
    /// Generates either explicit response classification.
    fn mark_useful_or_stalled() -> impl Strategy<Value = Self> {
        prop_oneof![Just(Self::MarkUseful), Just(Self::MarkStalled)]
    }

    /// Generates any action on a feedback capability.
    fn any() -> impl Strategy<Value = Self> {
        prop_oneof![Self::mark_useful_or_stalled(), Just(Self::Drop)]
    }

    /// Applies this action, consuming `feedback`.
    fn apply_to(&self, feedback: FindResponseFeedback) {
        match self {
            Self::MarkUseful => feedback.mark_useful(),
            Self::MarkStalled => feedback.mark_stalled(),
            Self::Drop => drop(feedback),
        }
    }

    /// Returns the outcome when this action settles the response.
    fn expected_outcome(&self) -> FindResponseOutcome {
        match self {
            Self::MarkUseful => FindResponseOutcome::Useful,
            Self::MarkStalled => FindResponseOutcome::Stalled,
            Self::Drop => FindResponseOutcome::Unclassified,
        }
    }
}
