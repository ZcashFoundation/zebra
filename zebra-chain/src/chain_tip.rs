//! Chain tip interfaces.

use crate::block;

/// An interface for querying the chain tip.
///
/// This trait helps avoid dependencies between:
/// * zebra-chain and tokio
/// * zebra-network and zebra-state
pub trait ChainTip {
    /// Return the height of the best chain tip.
    fn best_tip_height(&self) -> Option<block::Height>;
}

/// A chain tip that is always empty.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NoChainTip;

impl ChainTip for NoChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        None
    }
}
