//! Randomised property tests for recent sync lengths.

use std::cmp::min;

use proptest::prelude::*;
use proptest_derive::Arbitrary;

use super::super::RecentSyncLengths;

#[derive(Arbitrary, Clone, Copy, Debug)]
enum SyncLengthUpdate {
    ObtainTips(usize),
    ExtendTips(usize),
}

use SyncLengthUpdate::*;

proptest! {
    #[test]
    fn max_recent_lengths(
        sync_updates in any::<Vec<SyncLengthUpdate>>(),
    ) {
        let (mut recent_sync_lengths, receiver) = RecentSyncLengths::new(None);

        for (count, update) in sync_updates.into_iter().enumerate() {
            match update {
                ObtainTips(sync_length) => recent_sync_lengths.push_obtain_tips_length(sync_length),
                ExtendTips(sync_length) => recent_sync_lengths.push_extend_tips_length(sync_length),
            }

            prop_assert_eq!(
                receiver.borrow().len(),
                min(count + 1, RecentSyncLengths::MAX_RECENT_LENGTHS),
                "recent sync lengths: {:?}",
                *receiver.borrow(),
            );
        }
    }

    #[test]
    fn latest_first(
        sync_updates in any::<Vec<SyncLengthUpdate>>(),
    ) {
        let (mut recent_sync_lengths, receiver) = RecentSyncLengths::new(None);

        for update in sync_updates {
            let latest_sync_length = match update {
                ObtainTips(sync_length) => {
                    recent_sync_lengths.push_obtain_tips_length(sync_length);
                    sync_length
                }
                ExtendTips(sync_length) => {
                    recent_sync_lengths.push_extend_tips_length(sync_length);
                    sync_length
                }
            };

            prop_assert_eq!(
                receiver.borrow().first().cloned(),
                Some(latest_sync_length),
                "recent sync lengths: {:?}",
                *receiver.borrow(),
            );
        }
    }
}
