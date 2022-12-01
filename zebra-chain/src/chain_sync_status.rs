//! Defines method signatures for checking if the synchronizer is likely close to the network chain tip.

/// An interface for checking if the synchronization is likely close to the network chain tip.
pub trait ChainSyncStatus {
    /// Check if the synchronization is likely close to the network chain tip.
    fn is_close_to_tip(&self) -> bool;
}

/// A mock [`ChainSyncStatus`] implementation that allows setting the status externally.
#[derive(Clone, Default)]
pub struct MockSyncStatus {
    is_close_to_tip: bool,
}

impl MockSyncStatus {
    /// Sets mock sync status determining the return value of `is_close_to_tip()`
    pub fn set_is_close_to_tip(&mut self, is_close_to_tip: bool) {
        self.is_close_to_tip = is_close_to_tip;
    }
}

impl ChainSyncStatus for MockSyncStatus {
    fn is_close_to_tip(&self) -> bool {
        self.is_close_to_tip
    }
}
