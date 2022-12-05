//! Test-only mocks for [`ChainSyncStatus`].

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::ChainSyncStatus;

/// A mock [`ChainSyncStatus`] implementation that allows setting the status externally.
#[derive(Clone, Default)]
pub struct MockSyncStatus {
    is_close_to_tip: Arc<AtomicBool>,
}

impl MockSyncStatus {
    /// Sets mock sync status determining the return value of `is_close_to_tip()`
    pub fn set_is_close_to_tip(&mut self, is_close_to_tip: bool) {
        self.is_close_to_tip
            .store(is_close_to_tip, Ordering::SeqCst);
    }
}

impl ChainSyncStatus for MockSyncStatus {
    fn is_close_to_tip(&self) -> bool {
        self.is_close_to_tip.load(Ordering::SeqCst)
    }
}
