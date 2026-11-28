//! Reading the tips of the best chain and the side chains.

use std::sync::Arc;

use zebra_chain::block;

use crate::service::{finalized_state::ZebraDb, non_finalized_state::NonFinalizedState};

#[cfg(test)]
mod tests;

/// A chain tip in a [`ReadResponse::ChainTips`](crate::ReadResponse::ChainTips) response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTipInfo {
    /// The height of the tip block.
    pub height: block::Height,

    /// The hash of the tip block.
    pub hash: block::Hash,

    /// The number of blocks from the fork point with the best chain to the tip,
    /// which is 0 for the best chain tip.
    pub branch_len: usize,

    /// Whether the tip is the best chain tip or a side chain tip.
    pub status: ChainTipStatus,
}

/// The status of a [`ChainTipInfo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainTipStatus {
    /// The tip of the best chain.
    Active,

    /// The tip of a side chain.
    ValidFork,
}

/// Returns the tips of the best chain and of every side chain in `non_finalized_state`, with the
/// best chain tip first, then the side chain tips from most to least work.
///
/// If `non_finalized_state` has no chains, returns the finalized tip in `db` as the only tip,
/// or no tips if `db` is empty.
pub fn chain_tips(non_finalized_state: &NonFinalizedState, db: &ZebraDb) -> Vec<ChainTipInfo> {
    let Some(best_chain) = non_finalized_state.best_chain() else {
        return db
            .tip()
            .map(|(height, hash)| ChainTipInfo {
                height,
                hash,
                branch_len: 0,
                status: ChainTipStatus::Active,
            })
            .into_iter()
            .collect();
    };

    non_finalized_state
        .chain_iter()
        // `invalidate_block()` can leave a chain that ends at a block another chain extends.
        // That block isn't a tip, so skip the chain.
        .filter(|chain| {
            let tip_hash = chain.non_finalized_tip_hash();
            non_finalized_state
                .chain_iter()
                .filter(|other| other.contains_block_hash(tip_hash))
                .count()
                == 1
        })
        .map(|chain| {
            let (height, hash) = chain.non_finalized_tip();

            // Chains share every block below their fork point, so the branch is the blocks the
            // best chain doesn't have: the whole chain if it forked at the finalized tip.
            let branch_len = chain
                .blocks
                .values()
                .rev()
                .take_while(|block| !best_chain.contains_block_hash(block.hash))
                .count();

            let status = if Arc::ptr_eq(chain, best_chain) {
                ChainTipStatus::Active
            } else {
                ChainTipStatus::ValidFork
            };

            ChainTipInfo {
                height,
                hash,
                branch_len,
                status,
            }
        })
        .collect()
}
