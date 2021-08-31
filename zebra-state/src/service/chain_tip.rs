//! Access to Zebra chain tip information.
//!
//! Zebra has 3 different interfaces for access to chain tip information:
//! * [zebra_state::Request](crate::request): [tower::Service] requests about chain state,
//! * [LatestChainTip] for efficient access to the current best tip, and
//! * [ChainTipChange] to `await` specific changes to the chain tip.

use std::sync::Arc;

use tokio::sync::watch;

use zebra_chain::{block, chain_tip::ChainTip, transaction};

use crate::{request::ContextuallyValidBlock, FinalizedBlock};

use TipAction::*;

#[cfg(test)]
mod tests;

/// The internal watch channel data type for [`ChainTipSender`], [`LatestChainTip`],
/// and [`ChainTipChange`].
type ChainTipData = Option<ChainTipBlock>;

/// A chain tip block, with precalculated block data.
///
/// Used to efficiently update [`ChainTipSender`], [`LatestChainTip`],
/// and [`ChainTipChange`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTipBlock {
    /// The hash of the best chain tip block.
    pub hash: block::Hash,

    /// The height of the best chain tip block.
    pub height: block::Height,

    /// The mined transaction IDs of the transactions in `block`,
    /// in the same order as `block.transactions`.
    pub transaction_hashes: Arc<[transaction::Hash]>,
}

impl From<ContextuallyValidBlock> for ChainTipBlock {
    fn from(contextually_valid: ContextuallyValidBlock) -> Self {
        let ContextuallyValidBlock {
            block: _,
            hash,
            height,
            new_outputs: _,
            transaction_hashes,
            chain_value_pool_change: _,
        } = contextually_valid;
        Self {
            hash,
            height,
            transaction_hashes,
        }
    }
}

impl From<FinalizedBlock> for ChainTipBlock {
    fn from(finalized: FinalizedBlock) -> Self {
        let FinalizedBlock {
            block: _,
            hash,
            height,
            new_outputs: _,
            transaction_hashes,
        } = finalized;
        Self {
            hash,
            height,
            transaction_hashes,
        }
    }
}

/// A sender for changes to the non-finalized and finalized chain tips.
#[derive(Debug)]
pub struct ChainTipSender {
    /// Have we got any chain tips from the non-finalized state?
    ///
    /// Once this flag is set, we ignore the finalized state.
    /// `None` tips don't set this flag.
    non_finalized_tip: bool,

    /// The sender channel for chain tip data.
    sender: watch::Sender<ChainTipData>,

    /// A copy of the data in `sender`.
    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    active_value: ChainTipData,
}

impl ChainTipSender {
    /// Create new linked instances of [`ChainTipSender`], [`LatestChainTip`], and [`ChainTipChange`],
    /// using `initial_tip` as the tip.
    pub fn new(
        initial_tip: impl Into<Option<ChainTipBlock>>,
    ) -> (Self, LatestChainTip, ChainTipChange) {
        let (sender, receiver) = watch::channel(None);

        let mut sender = ChainTipSender {
            non_finalized_tip: false,
            sender,
            active_value: None,
        };

        let current = LatestChainTip::new(receiver.clone());
        let change = ChainTipChange::new(receiver);

        sender.update(initial_tip);

        (sender, current, change)
    }

    /// Update the latest finalized tip.
    ///
    /// May trigger an update to the best tip.
    pub fn set_finalized_tip(&mut self, new_tip: impl Into<Option<ChainTipBlock>>) {
        if !self.non_finalized_tip {
            self.update(new_tip);
        }
    }

    /// Update the latest non-finalized tip.
    ///
    /// May trigger an update to the best tip.
    pub fn set_best_non_finalized_tip(&mut self, new_tip: impl Into<Option<ChainTipBlock>>) {
        let new_tip = new_tip.into();

        // once the non-finalized state becomes active, it is always populated
        // but ignoring `None`s makes the tests easier
        if new_tip.is_some() {
            self.non_finalized_tip = true;
            self.update(new_tip)
        }
    }

    /// Possibly send an update to listeners.
    ///
    /// An update is only sent if the current best tip is different from the last best tip
    /// that was sent.
    fn update(&mut self, new_tip: impl Into<Option<ChainTipBlock>>) {
        let new_tip = new_tip.into();

        let needs_update = match (new_tip.as_ref(), self.active_value.as_ref()) {
            // since the blocks have been contextually validated,
            // we know their hashes cover all the block data
            (Some(new_tip), Some(active_value)) => new_tip.hash != active_value.hash,
            (Some(_new_tip), None) => true,
            (None, _active_value) => false,
        };

        if needs_update {
            let _ = self.sender.send(new_tip.clone());
            self.active_value = new_tip;
        }
    }
}

/// Efficient access to the state's current best chain tip.
///
/// The latest changes are available from all cloned instances of this type.
///
/// The chain tip data is based on:
/// * the best non-finalized chain tip, if available, or
/// * the finalized tip.
#[derive(Clone, Debug)]
pub struct LatestChainTip {
    /// The receiver for the current chain tip's data.
    receiver: watch::Receiver<ChainTipData>,
}

impl LatestChainTip {
    /// Create a new [`LatestChainTip`] from a watch channel receiver.
    fn new(receiver: watch::Receiver<ChainTipData>) -> Self {
        Self { receiver }
    }
}

impl ChainTip for LatestChainTip {
    /// Return the height of the best chain tip.
    fn best_tip_height(&self) -> Option<block::Height> {
        self.receiver.borrow().as_ref().map(|block| block.height)
    }

    /// Return the block hash of the best chain tip.
    fn best_tip_hash(&self) -> Option<block::Hash> {
        self.receiver.borrow().as_ref().map(|block| block.hash)
    }

    /// Return the mined transaction IDs of the transactions in the best chain tip block.
    ///
    /// All transactions with these mined IDs should be rejected from the mempool,
    /// even if their authorizing data is different.
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        self.receiver
            .borrow()
            .as_ref()
            .map(|block| block.transaction_hashes.clone())
            .unwrap_or_else(|| Arc::new([]))
    }
}

/// A chain tip change monitor.
/// Used to `await` changes and resets of the state's best chain tip.
///
/// The latest changes are available from all cloned instances of this type,
/// but each clone separately tracks the last change it provided,
/// so it can provide a [`Reset`] if the best chain fork changes.
///
/// The chain tip data is based on:
/// * the best non-finalized chain tip, if available, or
/// * the finalized tip.
#[derive(Debug)]
pub struct ChainTipChange {
    /// The receiver for the current chain tip's data.
    receiver: watch::Receiver<ChainTipData>,

    /// The most recent hash provided by this instance.
    previous_change_hash: Option<block::Hash>,
}

/// Actions that we can take in response to a [`ChainTipChange`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TipAction {
    /// The chain tip was updated continuously,
    /// using a child `block` of the previous block.
    ///
    /// The genesis block action is a `Grow`.
    Grow {
        /// Information about the block used to grow the chain.
        block: ChainTipBlock,
    },

    /// The chain tip was reset to a block with `height` and `hash`.
    ///
    /// Resets can happen for different reasons:
    /// * a newly created or cloned [`ChainTipChange`], which is behind the current tip,
    /// * extending the chain with a network upgrade activation block,
    /// * switching to a different best [`Chain`], also known as a rollback, and
    /// * receiving multiple blocks since the previous change.
    ///
    /// To keep the code and tests simple, Zebra performs the same reset actions,
    /// regardless of the reset reason.
    ///
    /// `Reset`s do not have the transaction hashes from the tip block,
    /// because all transactions should be cleared by a reset.
    Reset {
        /// The block height of the tip, after the chain reset.
        height: block::Height,

        /// The block hash of the tip, after the chain reset.
        ///
        /// Mainly useful for logging and debugging.
        hash: block::Hash,
    },
}

impl ChainTipChange {
    /// Wait until the next chain tip change, then return the corresponding [`TipAction`].
    ///
    /// Returns an error if communication with the state is lost.
    pub async fn next(&mut self) -> Result<TipAction, watch::error::RecvError> {
        let block = self.next_block().await?;

        // TODO: handle resets here

        self.previous_change_hash = Some(block.hash);

        Ok(Grow { block })
    }

    /// Create a new [`ChainTipChange`] from a watch channel receiver.
    fn new(receiver: watch::Receiver<ChainTipData>) -> Self {
        Self {
            receiver,
            previous_change_hash: None,
        }
    }

    /// Wait until the next chain tip change, then return the corresponding [`ChainTipBlock`].
    ///
    /// Returns an error if communication with the state is lost.
    async fn next_block(&mut self) -> Result<ChainTipBlock, watch::error::RecvError> {
        loop {
            self.receiver.changed().await?;

            // wait until there is actually Some block,
            // so we don't have `Option`s inside `TipAction`s
            if let Some(block) = self.best_tip_block() {
                assert!(
                    Some(block.hash) != self.previous_change_hash,
                    "ChainTipSender must ignore unchanged tips"
                );

                return Ok(block);
            }
        }
    }

    /// Return the current best [`ChainTipBlock`],
    /// or `None` if no block has been committed yet.
    fn best_tip_block(&self) -> Option<ChainTipBlock> {
        self.receiver.borrow().clone()
    }
}

impl Clone for ChainTipChange {
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.clone(),
            // clear the previous change hash, so the first action is a reset
            previous_change_hash: None,
        }
    }
}

impl TipAction {
    /// Is this tip action a [`Reset`]?
    pub fn is_reset(&self) -> bool {
        matches!(self, Reset { .. })
    }

    /// Returns the block hash of this tip action,
    /// regardless of the underlying variant.
    pub fn best_tip_hash(&self) -> block::Hash {
        match self {
            Grow { block } => block.hash,
            Reset { hash, .. } => *hash,
        }
    }
}
