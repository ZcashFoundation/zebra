//! Randomised property tests for recent sync lengths.

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
        let (mut recent_sync_lengths, receiver) = RecentSyncLengths::new();

        for update in sync_updates {
            match update {
                ObtainTips(sync_length) => recent_sync_lengths.push_obtain_tips_length(sync_length),
                ExtendTips(sync_length) => recent_sync_lengths.push_extend_tips_length(sync_length),
            }
        }

        prop_assert!(
            receiver.borrow().len() <= RecentSyncLengths::MAX_RECENT_LENGTHS,
            "recent sync lengths: {:?}",
            *receiver.borrow(),
        );
    }

    #[test]
    fn latest_first(
        sync_updates in any::<Vec<SyncLengthUpdate>>(),
    ) {
        let (mut recent_sync_lengths, receiver) = RecentSyncLengths::new();

        let mut latest_sync_length = None;

        for update in sync_updates {
            match update {
                ObtainTips(sync_length) => {
                    recent_sync_lengths.push_obtain_tips_length(sync_length);
                    latest_sync_length = Some(sync_length);
                }
                ExtendTips(sync_length) => {
                    recent_sync_lengths.push_extend_tips_length(sync_length);
                    latest_sync_length = Some(sync_length);
                }
            }
        }

        prop_assert_eq!(
            receiver.borrow().first().cloned(),
            latest_sync_length,
            "recent sync lengths: {:?}",
            *receiver.borrow(),
        );
    }
}
