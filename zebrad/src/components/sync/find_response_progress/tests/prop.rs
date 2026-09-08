//! Properties of response classification across hash counts and completion orders.

use proptest::prelude::*;
use tokio::sync::mpsc::error::TryRecvError;
use zebra_network::FindResponseFeedback;

use super::super::FindResponseProgress;

proptest! {
    /// Credits a response exactly once, after every accepted hash verifies.
    #[test]
    fn all_hashes_must_verify(hash_count in 1usize..=11) {
        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let progress = FindResponseProgress::new(hash_count, feedback);

        for _ in 1..hash_count {
            HashFeedback::Verified.apply(&progress);

            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        HashFeedback::Verified.apply(&progress);

        prop_assert_eq!(observer.try_outcome(), Ok(Some(true)));
        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }
}

/// One block hash's outcome reported to the response progress tracker.
#[derive(Clone, Copy, Debug)]
enum HashFeedback {
    Verified,
}

impl HashFeedback {
    /// Applies this outcome to the supplied [`FindResponseProgress`].
    fn apply(self, progress: &FindResponseProgress) {
        match self {
            Self::Verified => progress.record_verified_hash(),
        }
    }
}
