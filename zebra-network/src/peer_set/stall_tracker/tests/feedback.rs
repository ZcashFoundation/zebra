//! Tests for the find-response feedback capability.

use tokio::sync::mpsc::{self, error::TryRecvError};

use super::{
    test_addr, FindRequestId, FindResponseEvent, FindResponseFeedback, FindResponseOutcome,
};

/// Tests that once usefulness is reported the feedback channel is immediately closed.
#[test]
fn useful_feedback_closes_channel() {
    let _test_guard = zebra_test::init();
    let addr = test_addr(1);
    let request_id = FindRequestId::from(1);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let feedback = FindResponseFeedback::new(addr, request_id, sender);

    feedback.mark_useful();

    assert_eq!(
        receiver.try_recv(),
        Ok(FindResponseEvent::new(
            addr,
            request_id,
            FindResponseOutcome::Useful
        )),
    );
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
}

/// Tests that once a stall is reported the feedback channel is immediately closed.
#[test]
fn stalled_feedback_closes_channel() {
    let _test_guard = zebra_test::init();
    let addr = test_addr(1);
    let request_id = FindRequestId::from(1);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let feedback = FindResponseFeedback::new(addr, request_id, sender);

    feedback.mark_stalled();

    assert_eq!(
        receiver.try_recv(),
        Ok(FindResponseEvent::new(
            addr,
            request_id,
            FindResponseOutcome::Stalled
        )),
    );
    assert_eq!(receiver.try_recv(), Err(TryRecvError::Disconnected));
}
