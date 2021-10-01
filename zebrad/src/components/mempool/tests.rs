use std::pin::Pin;

use tower::ServiceExt;

use super::{storage::Storage, ActiveState, InboundTxDownloads, Mempool, Request};
use crate::components::sync::{RecentSyncLengths, SyncStatus};

mod prop;
mod vector;

impl Mempool {
    /// Get the storage field of the mempool for testing purposes.
    pub fn storage(&mut self) -> &mut Storage {
        match &mut self.active_state {
            ActiveState::Disabled => panic!("mempool must be enabled"),
            ActiveState::Enabled { storage, .. } => storage,
        }
    }

    /// Get the transaction downloader of the mempool for testing purposes.
    pub fn tx_downloads(&self) -> &Pin<Box<InboundTxDownloads>> {
        match &self.active_state {
            ActiveState::Disabled => panic!("mempool must be enabled"),
            ActiveState::Enabled { tx_downloads, .. } => tx_downloads,
        }
    }

    /// Enable the mempool by pretending the synchronization is close to the tip.
    pub async fn enable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        // Pretend we're close to tip
        SyncStatus::sync_close_to_tip(recent_syncs);
        // Make a dummy request to poll the mempool and make it enable itself
        self.dummy_call().await;
    }

    /// Disable the mempool by pretending the synchronization is far from the tip.
    pub async fn disable(&mut self, recent_syncs: &mut RecentSyncLengths) {
        // Pretend we're far from the tip
        SyncStatus::sync_far_from_tip(recent_syncs);
        // Make a dummy request to poll the mempool and make it disable itself
        self.dummy_call().await;
    }

    /// Perform a dummy service call so that `poll_ready` is called.
    pub async fn dummy_call(&mut self) {
        self.oneshot(Request::Queue(vec![]))
            .await
            .expect("Queuing no transactions shouldn't fail");
    }
}
