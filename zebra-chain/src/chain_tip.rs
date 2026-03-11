//! Zebra interfaces for access to chain tip information.

use std::{future, sync::Arc};

use chrono::{DateTime, Utc};

use crate::{block, parameters::Network, transaction, BoxError};

mod network_chain_tip_height_estimator;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;
#[cfg(test)]
mod tests;

pub use network_chain_tip_height_estimator::NetworkChainTipHeightEstimator;

/// An interface for querying the chain tip.
///
/// This trait helps avoid dependencies between:
/// * `zebra-chain` and `tokio`
/// * `zebra-network` and `zebra-state`
pub trait ChainTip {
    /// Returns the height of the best chain tip.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_height(&self) -> Option<block::Height>;

    /// Returns the block hash of the best chain tip.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_hash(&self) -> Option<block::Hash>;

    /// Returns the height and the hash of the best chain tip.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)>;

    /// Returns the block time of the best chain tip.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_block_time(&self) -> Option<DateTime<Utc>>;

    /// Returns the height and the block time of the best chain tip.
    /// Returning both values at the same time guarantees that they refer to the same chain tip.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)>;

    /// Returns the mined transaction IDs of the transactions in the best chain tip block.
    ///
    /// All transactions with these mined IDs should be rejected from the mempool,
    /// even if their authorizing data is different.
    ///
    /// Does not mark the best tip as seen.
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]>;

    /// A future that returns when the best chain tip changes.
    /// Can return immediately if the latest value in this [`ChainTip`] has not been seen yet.
    ///
    /// Marks the best tip as seen.
    ///
    /// Returns an error if Zebra is shutting down, or the state has permanently failed.
    ///
    /// See [`tokio::watch::Receiver::changed()`](https://docs.rs/tokio/latest/tokio/sync/watch/struct.Receiver.html#method.changed) for details.
    fn best_tip_changed(
        &mut self,
    ) -> impl std::future::Future<Output = Result<(), BoxError>> + Send;

    /// Mark the current best tip as seen.
    ///
    /// Later calls to [`ChainTip::best_tip_changed()`] will wait for the next change
    /// before returning.
    fn mark_best_tip_seen(&mut self);

    // Provided methods
    //
    /// Return an estimate of the network chain tip's height.
    ///
    /// The estimate is calculated based on the current local time, the block time of the best tip
    /// and the height of the best tip.
    fn estimate_network_chain_tip_height(
        &self,
        network: &Network,
        now: DateTime<Utc>,
    ) -> Option<block::Height> {
        let (current_height, current_block_time) = self.best_tip_height_and_block_time()?;

        let estimator =
            NetworkChainTipHeightEstimator::new(current_block_time, current_height, network);

        Some(estimator.estimate_height_at(now))
    }

    /// Return an estimate of how many blocks there are ahead of Zebra's best chain tip until the
    /// network chain tip, and Zebra's best chain tip height.
    ///
    /// The first element in the returned tuple is the estimate.
    /// The second element in the returned tuple is the current best chain tip.
    ///
    /// The estimate is calculated based on the current local time, the block time of the best tip
    /// and the height of the best tip.
    ///
    /// This estimate may be negative if the current local time is behind the chain tip block's
    /// timestamp.
    ///
    /// Returns `None` if the state is empty.
    fn estimate_distance_to_network_chain_tip(
        &self,
        network: &Network,
    ) -> Option<(block::HeightDiff, block::Height)> {
        let (current_height, current_block_time) = self.best_tip_height_and_block_time()?;

        let estimator =
            NetworkChainTipHeightEstimator::new(current_block_time, current_height, network);

        let distance_to_tip = estimator.estimate_height_at(Utc::now()) - current_height;

        Some((distance_to_tip, current_height))
    }
}

/// A chain tip that is always empty and never changes.
///
/// Used in production for isolated network connections,
/// and as a mock chain tip in tests.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NoChainTip;

impl ChainTip for NoChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        None
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        None
    }

    fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
        None
    }

    fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
        None
    }

    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
        None
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        Arc::new([])
    }

    /// The [`NoChainTip`] best tip never changes, so this never returns.
    async fn best_tip_changed(&mut self) -> Result<(), BoxError> {
        future::pending().await
    }

    /// The [`NoChainTip`] best tip never changes, so this does nothing.
    fn mark_best_tip_seen(&mut self) {}
}
