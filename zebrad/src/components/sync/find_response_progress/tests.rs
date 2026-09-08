//! Tests for response-level verification progress.

use tokio::sync::mpsc::error::TryRecvError;
use zebra_network::FindResponseFeedback;

use super::FindResponseProgress;

mod prop;

/// Later verification cannot replace an already reported missing-hash stall.
#[test]
fn verification_does_not_override_missing_hash() {
    let _test_guard = zebra_test::init();

    let (feedback, observer) = FindResponseFeedback::new_for_test();
    let progress = FindResponseProgress::new(2, feedback);

    progress.record_missing_hash();
    progress.record_verified_hash();

    assert_eq!(observer.try_outcome(), Ok(Some(false)));
    assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
}
