//! Syncer chain tip status, based on recent block locator responses from peers.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::watch;
use zebra_chain::{block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip};
use zebra_network::PeerSetStatus;
use zebra_state as zs;

use super::RecentSyncLengths;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;
#[cfg(test)]
mod tests;

/// A helper type to determine if the synchronizer has likely reached the chain tip.
///
/// This type can be used as a handle, so cloning it is cheap.

#[derive(Clone, Debug)]
pub struct SyncStatus {
    /// Rolling window of recent sync batch sizes emitted by the syncer.
    latest_sync_length: watch::Receiver<Vec<usize>>,
    /// Snapshot of peer availability published by the peer set.
    peer_status: watch::Receiver<PeerSetStatus>,
    /// Minimum number of ready peers required before we consider ourselves near tip.
    min_ready_peers: usize,
    /// View into the latest committed tip so we can detect progress that bypasses the syncer.
    latest_chain_tip: zs::LatestChainTip,
    /// Tracks when we last saw meaningful chain activity (sync progress or tip advance).
    tip_progress: Arc<Mutex<TipProgress>>,
    /// Maximum time we consider "recent" progress even if batches are zero.
    /// Mirrors `health.ready_max_tip_age` so quiet network periods don't flip readiness.
    progress_grace: Duration,
}

#[derive(Debug)]
struct TipProgress {
    /// Last height observed when checking the chain tip.
    last_height: Option<block::Height>,
    /// When we last observed either a height change or a non-zero sync batch.
    last_updated: Instant,
    has_progress: bool,
}

impl TipProgress {
    fn new() -> Self {
        Self {
            last_height: None,
            last_updated: Instant::now(),
            has_progress: false,
        }
    }
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
    /// - `peer_status` feeds live peer availability so we can gate readiness on connectivity.
    /// - `latest_chain_tip` lets us observe progress that bypasses the syncer (e.g. gossip).
    /// - `min_ready_peers` encodes how many ready peers we require before enabling tip logic.
    /// - `progress_grace` matches the health config and bounds how long we treat the last
    ///   observed activity as "recent" when sync batches are zero.
    pub fn new(
        peer_status: watch::Receiver<PeerSetStatus>,
        latest_chain_tip: zs::LatestChainTip,
        min_ready_peers: usize,
        progress_grace: Duration,
    ) -> (Self, RecentSyncLengths) {
        let tip_progress = Arc::new(Mutex::new(TipProgress::new()));

        let progress_tip = Arc::clone(&tip_progress);
        let progress_callback: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let mut tip = progress_tip.lock().expect("tip progress mutex poisoned");
            tip.last_updated = Instant::now();
            tip.has_progress = true;
        });

        let (recent_sync_lengths, latest_sync_length) =
            RecentSyncLengths::new(Some(progress_callback));
        let status = SyncStatus {
            latest_sync_length,
            peer_status,
            min_ready_peers,
            latest_chain_tip,
            tip_progress,
            progress_grace,
        };

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

    fn has_enough_ready_peers(&self) -> bool {
        if self.min_ready_peers == 0 {
            return true;
        }

        let status = (*self.peer_status.borrow()).clone();
        status.ready_peers >= self.min_ready_peers
    }

    fn observe_tip_progress(&self) {
        if let Some((height, _)) = self.latest_chain_tip.best_tip_height_and_hash() {
            let mut tip = self
                .tip_progress
                .lock()
                .expect("tip progress mutex poisoned");

            if tip.last_height != Some(height) {
                tip.last_height = Some(height);
                tip.last_updated = Instant::now();
                tip.has_progress = true;
            }
        }
    }

    /// Returns `true` if we've observed any activity (either a non-zero batch or a
    /// tip height change) within the configured grace window.
    fn recent_progress_within_grace(&self) -> bool {
        let now = Instant::now();
        let tip = self
            .tip_progress
            .lock()
            .expect("tip progress mutex poisoned");
        tip.has_progress && now.duration_since(tip.last_updated) <= self.progress_grace
    }
}

impl ChainSyncStatus for SyncStatus {
    /// Check if the synchronization is likely close to the chain tip.
    fn is_close_to_tip(&self) -> bool {
        if !self.has_enough_ready_peers() {
            return false;
        }

        self.observe_tip_progress();

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

        if sum == 0 {
            return self.recent_progress_within_grace();
        }

        // Compute the average sync length.
        // This value effectively represents a simple moving average.
        let avg = sum / sync_lengths.len();

        // The synchronization process is close to the chain tip once the
        // average sync length falls below the threshold.
        avg < Self::MIN_DIST_FROM_TIP
    }
}
