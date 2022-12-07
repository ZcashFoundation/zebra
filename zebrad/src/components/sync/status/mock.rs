//! Test-only mocking code for [`SyncStatus`].

// This code is currently unused with some feature combinations.
#![allow(dead_code)]

use crate::components::sync::RecentSyncLengths;

use super::SyncStatus;

// TODO: move these methods to RecentSyncLengths
impl SyncStatus {
    /// Feed the given [`RecentSyncLengths`] it order to make the matching
    /// [`SyncStatus`] report that it's close to the tip.
    pub(crate) fn sync_close_to_tip(recent_syncs: &mut RecentSyncLengths) {
        for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
            recent_syncs.push_extend_tips_length(1);
        }
    }

    /// Feed the given [`RecentSyncLengths`] it order to make the matching
    /// [`SyncStatus`] report that it's not close to the tip.
    pub(crate) fn sync_far_from_tip(recent_syncs: &mut RecentSyncLengths) {
        for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
            recent_syncs.push_extend_tips_length(Self::MIN_DIST_FROM_TIP * 10);
        }
    }
}
