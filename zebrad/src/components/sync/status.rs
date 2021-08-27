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
        let _sync_lengths = self.latest_sync_length.borrow();

        // TODO: Determine if the synchronization is actually close to the tip (#2592).
        true
    }
}
