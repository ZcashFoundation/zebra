//! A channel which holds a list of recent syncer response lengths.

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
#[derive(Debug)]
pub struct RecentSyncLengths {
    ///
    sender: watch::Sender<Vec<usize>>,

    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    recent_lengths: Vec<usize>,
}

impl RecentSyncLengths {
    /// The maximum number of lengths sent by `RecentSyncLengths`.
    ///
    /// Older lengths are dropped.
    ///
    /// Note: this maximum should be increased if Zebra starts tracking the
    /// lengths of each individual peer response.
    pub const MAX_RECENT_LENGTHS: usize = 3;

    /// Create a new instance of [`RecentSyncLengths`]
    /// and a [`watch::Receiver`] endpoint for receiving recent sync lengths.
    pub fn new() -> (Self, watch::Receiver<Vec<usize>>) {
        let (sender, receiver) = watch::channel(Vec::with_capacity(Self::MAX_RECENT_LENGTHS));

        (
            RecentSyncLengths {
                sender,
                recent_lengths: Vec::with_capacity(Self::MAX_RECENT_LENGTHS),
            },
            receiver,
        )
    }

    // We skip the genesis block download, because it just uses the genesis hash directly,
    // rather than asking peers for the next blocks in the chain.
    // (And if genesis downloads kept failing, we could accidentally activate the mempool.)

    /// Insert a sync length from [`ChainSync::obtain_tips`] at the front of the list.
    #[instrument(skip(self), fields(self.recent_lengths))]
    pub fn push_obtain_tips_length(&mut self, sync_length: usize) {
        // currently, we treat lengths from obtain and extend tips exactly the same
        self.update(sync_length)
    }

    /// Insert a sync length from [`ChainSync::extend_tips`] at the front of the list.
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

        // ignore dropped receivers
        let _ = self.sender.send(self.recent_lengths.clone());
    }
}
