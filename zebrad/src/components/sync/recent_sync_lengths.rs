//! A channel which holds a list of recent syncer response lengths.

use std::sync::Arc;

use tokio::sync::watch;

#[cfg(test)]
mod tests;

/// A helper type which holds a list of recent syncer response lengths.
/// These sync lengths can be used to work out if Zebra has reached the end of the chain.
///
/// New lengths are added to the front of the list.
/// Old lengths are dropped if the list is longer than `MAX_RECENT_LENGTHS`.
//
// TODO: disable the mempool if obtain or extend tips return errors?
pub struct RecentSyncLengths {
    /// A sender for an array of recent sync lengths.
    sender: watch::Sender<Vec<usize>>,

    /// A local copy of the contents of `sender`.
    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    recent_lengths: Vec<usize>,

    /// Optional callback invoked when a non-zero batch is observed.
    ///
    /// The sync status uses this to track when meaningful progress happened,
    /// so readiness can tolerate quiet periods without losing peer signals.
    progress_callback: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl RecentSyncLengths {
    /// The maximum number of lengths sent by `RecentSyncLengths`.
    ///
    /// Older lengths are dropped.
    ///
    /// This length was chosen as a tradeoff between:
    /// * clearing temporary errors and temporary syncs quickly
    /// * distinguishing between temporary and sustained syncs/errors
    /// * activating the syncer shortly after reaching the chain tip
    pub const MAX_RECENT_LENGTHS: usize = 3;

    /// Create a new instance of [`RecentSyncLengths`]
    /// and a [`watch::Receiver`] endpoint for receiving recent sync lengths.
    pub fn new(
        progress_callback: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Self, watch::Receiver<Vec<usize>>) {
        let (sender, receiver) = watch::channel(Vec::new());

        (
            RecentSyncLengths {
                sender,
                recent_lengths: Vec::with_capacity(Self::MAX_RECENT_LENGTHS),
                progress_callback,
            },
            receiver,
        )
    }

    // We skip the genesis block download, because it just uses the genesis hash directly,
    // rather than asking peers for the next blocks in the chain.
    // (And if genesis downloads kept failing, we could accidentally activate the mempool.)

    /// Insert a sync length from [`ChainSync::obtain_tips`] at the front of the
    /// list.
    ///
    /// [`ChainSync::obtain_tips`]: super::ChainSync::obtain_tips
    #[instrument(skip(self), fields(self.recent_lengths))]
    pub fn push_obtain_tips_length(&mut self, sync_length: usize) {
        // currently, we treat lengths from obtain and extend tips exactly the same,
        // but we might want to ignore some obtain tips lengths
        //
        // See "Response Lengths During Sync -> Details" in:
        // https://github.com/ZcashFoundation/zebra/issues/2592#issuecomment-897304684
        self.update(sync_length)
    }

    /// Insert a sync length from [`ChainSync::extend_tips`] at the front of the
    /// list.
    ///
    /// [`ChainSync::extend_tips`]: super::ChainSync::extend_tips
    #[instrument(skip(self), fields(self.recent_lengths))]
    pub fn push_extend_tips_length(&mut self, sync_length: usize) {
        self.update(sync_length)
    }

    /// Sends a list update to listeners.
    ///
    /// Prunes recent lengths, if the list is longer than `MAX_RECENT_LENGTHS`.
    fn update(&mut self, sync_length: usize) {
        self.recent_lengths.insert(0, sync_length);

        self.recent_lengths.truncate(Self::MAX_RECENT_LENGTHS);

        debug!(
            recent_lengths = ?self.recent_lengths,
            "sending recent sync lengths"
        );

        // ignore dropped receivers
        let _ = self.sender.send(self.recent_lengths.clone());

        if sync_length > 0 {
            if let Some(callback) = &self.progress_callback {
                callback();
            }
        }
    }
}
