//! Zebra interfaces for access to chain tip information.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::{block, parameters::Network, transaction};

mod network_chain_tip_height_estimator;

#[cfg(any(test, feature = "proptest-impl"))]
pub mod mock;
#[cfg(test)]
mod tests;

use network_chain_tip_height_estimator::NetworkChainTipHeightEstimator;

#[cfg(any(test, feature = "proptest-impl"))]
pub use mock::NoChainTip;

/// An interface for querying the chain tip.
///
/// This trait helps avoid dependencies between:
/// * zebra-chain and tokio
/// * zebra-network and zebra-state
pub trait ChainTip {
    /// Return the height of the best chain tip.
    fn best_tip_height(&self) -> Option<block::Height>;

    /// Return the block hash of the best chain tip.
    fn best_tip_hash(&self) -> Option<block::Hash>;

    /// Return the height and the hash of the best chain tip.
    fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)>;

    /// Return the block time of the best chain tip.
    fn best_tip_block_time(&self) -> Option<DateTime<Utc>>;

    /// Return the height and the block time of the best chain tip.
    ///
    /// Returning both values at the same time guarantees that they refer to the same chain tip.
    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)>;

    /// Return the mined transaction IDs of the transactions in the best chain tip block.
    ///
    /// All transactions with these mined IDs should be rejected from the mempool,
    /// even if their authorizing data is different.
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]>;

    /// Return an estimate of the network chain tip's height.
    ///
    /// The estimate is calculated based on the current local time, the block time of the best tip
    /// and the height of the best tip.
    fn estimate_network_chain_tip_height(
        &self,
        network: Network,
        now: DateTime<Utc>,
    ) -> Option<block::Height> {
        let (current_height, current_block_time) = self.best_tip_height_and_block_time()?;

        let estimator =
            NetworkChainTipHeightEstimator::new(current_block_time, current_height, network);

        Some(estimator.estimate_height_at(now))
    }

    /// Return an estimate of how many blocks there are ahead of Zebra's best chain tip
    /// until the network chain tip, and Zebra's best chain tip height.
    ///
    /// The estimate is calculated based on the current local time, the block time of the best tip
    /// and the height of the best tip.
    ///
    /// This estimate may be negative if the current local time is behind the chain tip block's timestamp.
    fn estimate_distance_to_network_chain_tip(
        &self,
        network: Network,
    ) -> Option<(i32, block::Height)> {
        let (current_height, current_block_time) = self.best_tip_height_and_block_time()?;

        let estimator =
            NetworkChainTipHeightEstimator::new(current_block_time, current_height, network);

        Some((
            estimator.estimate_height_at(Utc::now()) - current_height,
            current_height,
        ))
    }
}
