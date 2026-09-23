//! Properties of response classification across hash counts and completion orders.

use std::iter::once;

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

    /// Abandonment prevents useful credit and releases neutral feedback after all outcomes.
    #[test]
    fn abandoned_hash_releases_neutral_feedback(
        before in prop::collection::vec(verified_or_abandoned(), 0..=5),
        after in prop::collection::vec(verified_or_abandoned(), 0..=5),
    ) {
        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let hash_count = before.len() + 1 + after.len();
        let progress = FindResponseProgress::new(hash_count, feedback);

        let mut hashes = before.into_iter().chain(once(HashFeedback::Abandoned)).chain(after);
        let last = hashes.next_back()
            .expect("the sequence contains a guaranteed abandoned outcome");

        for hash in hashes {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        last.apply(&progress);
        prop_assert_eq!(observer.try_outcome(), Ok(None));
        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }

    /// Invalidity overrides earlier abandonment, and no later outcome changes the stall.
    ///
    /// Abandonment must not finalize neutral feedback while an unresolved hash can prove invalid.
    #[test]
    fn invalid_hash_overrides_abandonment(
        before in prop::collection::vec(Just(HashFeedback::Verified), 0..=5),
        between in prop::collection::vec(verified_or_abandoned(), 0..=5),
        after in prop::collection::vec(any_hash_feedback(), 0..=5),
    ) {
        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let hash_count = before.len() + 1 + between.len() + 1 + after.len();
        let progress = FindResponseProgress::new(hash_count, feedback);

        for hash in before {
            hash.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        HashFeedback::Abandoned.apply(&progress);
        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));

        for hash in between {
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

    /// Only the final progress owner releases an unfinished response neutrally.
    ///
    /// Partial verification must not earn useful credit or release feedback while owners remain.
    #[test]
    fn final_progress_owner_drop_releases_feedback(
        owner_count in 1usize..=6,
        verified_count in 0usize..=5,
        unresolved_count in 1usize..=5,
    ) {
        let _test_guard = zebra_test::init();

        let (feedback, observer) = FindResponseFeedback::new_for_test();
        let progress = FindResponseProgress::new(verified_count + unresolved_count, feedback);
        let other_owners: Vec<_> = (1..owner_count).map(|_| progress.clone()).collect();

        for _ in 0..verified_count {
            HashFeedback::Verified.apply(&progress);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        for owner in other_owners {
            drop(owner);
            prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));
        }

        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Empty));

        drop(progress);
        prop_assert_eq!(observer.try_outcome(), Ok(None));
        prop_assert_eq!(observer.try_outcome(), Err(TryRecvError::Disconnected));
    }
}

/// One block hash's outcome reported to the response progress tracker.
#[derive(Clone, Copy, Debug)]
enum HashFeedback {
    Verified,
    Missing,
    Invalid,
    Abandoned,
}

impl HashFeedback {
    /// Applies this outcome to the supplied [`FindResponseProgress`].
    fn apply(self, progress: &FindResponseProgress) {
        match self {
            Self::Verified => progress.record_verified_hash(),
            Self::Missing => progress.record_missing_hash(),
            Self::Invalid => progress.record_invalid_hash(),
            Self::Abandoned => progress.record_abandoned_hash(),
        }
    }
}

/// Generates any outcome for a hash remaining after terminal feedback.
fn any_hash_feedback() -> impl Strategy<Value = HashFeedback> {
    prop_oneof![
        Just(HashFeedback::Verified),
        Just(HashFeedback::Missing),
        Just(HashFeedback::Invalid),
        Just(HashFeedback::Abandoned),
    ]
}

/// Generates outcomes without evidence of missing or invalid blocks.
fn verified_or_abandoned() -> impl Strategy<Value = HashFeedback> {
    prop_oneof![Just(HashFeedback::Verified), Just(HashFeedback::Abandoned)]
}
