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

    /// The first missing hash stalls a response, and no later outcome changes its feedback.
    #[test]
    fn missing_hash_overrides_verified_progress(
        before in prop::collection::vec(Just(HashFeedback::Verified), 0..=5),
        after in prop::collection::vec(any_hash_feedback(), 0..=5),
    ) {
        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let progress = FindResponseProgress::new(before.len() + 1 + after.len(), feedback);

        for hash in before {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        HashFeedback::Missing.apply(&progress);
        prop_assert_eq!(observer.try_outcome(), Ok(Some(false)));

        for hash in after {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
        }

        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }

    /// The first invalid hash stalls a response, and no later outcome changes its feedback.
    #[test]
    fn invalid_hash_overrides_verified_progress(
        before in prop::collection::vec(Just(HashFeedback::Verified), 0..=5),
        after in prop::collection::vec(any_hash_feedback(), 0..=5),
    ) {
        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let progress = FindResponseProgress::new(before.len() + 1 + after.len(), feedback);

        for hash in before {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        HashFeedback::Invalid.apply(&progress);
        prop_assert_eq!(observer.try_outcome(), Ok(Some(false)));

        for hash in after {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
        }

        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }
}

/// One block hash's outcome reported to the response progress tracker.
#[derive(Clone, Copy, Debug)]
enum HashFeedback {
    Verified,
    Missing,
    Invalid,
}

impl HashFeedback {
    /// Applies this outcome to the supplied [`FindResponseProgress`].
    fn apply(self, progress: &FindResponseProgress) {
        match self {
            Self::Verified => progress.record_verified_hash(),
            Self::Missing => progress.record_missing_hash(),
            Self::Invalid => progress.record_invalid_hash(),
        }
    }
}

/// Generates any outcome for a hash remaining after terminal feedback.
fn any_hash_feedback() -> impl Strategy<Value = HashFeedback> {
    prop_oneof![
        Just(HashFeedback::Verified),
        Just(HashFeedback::Missing),
        Just(HashFeedback::Invalid),
    ]
}
