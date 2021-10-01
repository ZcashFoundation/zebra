use tokio::sync::watch;

use super::RecentSyncLengths;

#[cfg(test)]
mod tests;

/// A helper type to determine if the synchronizer has likely reached the chain tip.
///
/// This type can be used as a handle, so cloning it is cheap.
#[derive(Clone, Debug)]
pub struct SyncStatus {
    latest_sync_length: watch::Receiver<Vec<usize>>,
}

impl SyncStatus {
    /// The threshold that determines if the synchronization is at the chain
    /// tip.
    ///
    /// This is based on the fact that sync lengths are around 2-20 blocks long
    /// once Zebra reaches the tip.
    const MIN_DIST_FROM_TIP: usize = 20;

    /// Create an instance of [`SyncStatus`].
    ///
    /// The status is determined based on the latest counts of synchronized blocks, observed
    /// through `latest_sync_length`.
    pub fn new() -> (Self, RecentSyncLengths) {
        let (recent_sync_lengths, latest_sync_length) = RecentSyncLengths::new();
        let status = SyncStatus { latest_sync_length };

        (status, recent_sync_lengths)
    }

    /// Wait until the synchronization is likely close to the tip.
    ///
    /// Returns an error if communication with the synchronizer is lost.
    pub async fn wait_until_close_to_tip(&mut self) -> Result<(), watch::error::RecvError> {
        while !self.is_close_to_tip() {
            self.latest_sync_length.changed().await?;
        }

        Ok(())
    }

    /// Check if the synchronization is likely close to the chain tip.
    pub fn is_close_to_tip(&self) -> bool {
        let sync_lengths = self.latest_sync_length.borrow();

        // Return early if sync_lengths is empty.
        if sync_lengths.is_empty() {
            return false;
        }

        // Compute the sum of the `sync_lengths`.
        // The sum is computed by saturating addition in order to avoid overflowing.
        let sum = sync_lengths
            .iter()
            .fold(0usize, |sum, rhs| sum.saturating_add(*rhs));

        // Compute the average sync length.
        // This value effectively represents a simple moving average.
        let avg = sum / sync_lengths.len();

        // The synchronization process is close to the chain tip once the
        // average sync length falls below the threshold.
        avg < Self::MIN_DIST_FROM_TIP
    }

    /// Feed the given [`RecentSyncLengths`] it order to make the matching
    /// [`SyncStatus`] report that it's close to the tip.
    #[cfg(test)]
    pub(crate) fn sync_close_to_tip(recent_syncs: &mut RecentSyncLengths) {
        for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
            recent_syncs.push_extend_tips_length(1);
        }
    }

    /// Feed the given [`RecentSyncLengths`] it order to make the matching
    /// [`SyncStatus`] report that it's not close to the tip.
    #[cfg(test)]
    pub(crate) fn sync_far_from_tip(recent_syncs: &mut RecentSyncLengths) {
        for _ in 0..RecentSyncLengths::MAX_RECENT_LENGTHS {
            recent_syncs.push_extend_tips_length(Self::MIN_DIST_FROM_TIP * 10);
        }
    }
}
