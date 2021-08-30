//! Chain tip interfaces.

use std::sync::Arc;

use crate::{block, transaction};

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

    /// Return the mined transaction IDs of the transactions in the best chain tip block.
    ///
    /// All transactions with these mined IDs should be rejected from the mempool,
    /// even if their authorizing data is different.
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]>;
}

/// A chain tip that is always empty.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NoChainTip;

impl ChainTip for NoChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        None
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        None
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        Arc::new([])
    }
}
