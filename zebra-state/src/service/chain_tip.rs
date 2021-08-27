use std::sync::Arc;

use tokio::sync::watch;

use zebra_chain::{
    block::{self, Block},
    chain_tip::ChainTip,
};

use crate::{request::ContextuallyValidBlock, FinalizedBlock};

#[cfg(test)]
mod tests;

/// The internal watch channel data type for [`ChainTipSender`] and [`ChainTipReceiver`].
type ChainTipData = Option<ChainTipBlock>;

/// A chain tip block, with precalculated block data.
///
/// Used to efficiently update the [`ChainTipSender`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTipBlock {
    pub(crate) block: Arc<Block>,
    pub(crate) hash: block::Hash,
    pub(crate) height: block::Height,
}

impl From<ContextuallyValidBlock> for ChainTipBlock {
    fn from(contextually_valid: ContextuallyValidBlock) -> Self {
        let ContextuallyValidBlock {
            block,
            hash,
            height,
            new_outputs: _,
            transaction_hashes: _,
            chain_value_pool_change: _,
        } = contextually_valid;
        Self {
            block,
            hash,
            height,
        }
    }
}

impl From<FinalizedBlock> for ChainTipBlock {
    fn from(finalized: FinalizedBlock) -> Self {
        let FinalizedBlock {
            block,
            hash,
            height,
            new_outputs: _,
            transaction_hashes: _,
        } = finalized;
        Self {
            block,
            hash,
            height,
        }
    }
}

/// A sender for recent changes to the non-finalized and finalized chain tips.
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
    /// Create new linked instances of [`ChainTipSender`] and [`ChainTipReceiver`],
    /// using `initial_tip` as the tip.
    pub fn new(initial_tip: impl Into<Option<ChainTipBlock>>) -> (Self, ChainTipReceiver) {
        let (sender, receiver) = watch::channel(None);
        let mut sender = ChainTipSender {
            non_finalized_tip: false,
            sender,
            active_value: None,
        };
        let receiver = ChainTipReceiver::new(receiver);

        sender.update(initial_tip);

        (sender, receiver)
    }

    /// Update the current finalized tip.
    ///
    /// May trigger an update to the best tip.
    pub fn set_finalized_tip(&mut self, new_tip: impl Into<Option<ChainTipBlock>>) {
        if !self.non_finalized_tip {
            self.update(new_tip);
        }
    }

    /// Update the current non-finalized tip.
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

/// A receiver for recent changes to the non-finalized and finalized chain tips.
///
/// The latest changes are available from all cloned instances of this type.
///
/// The chain tip data is based on:
/// * the best non-finalized chain tip, if available, or
/// * the finalized tip.
#[derive(Clone, Debug)]
pub struct ChainTipReceiver {
    receiver: watch::Receiver<ChainTipData>,
}

impl ChainTipReceiver {
    /// Create a new chain tip receiver from a watch channel receiver.
    fn new(receiver: watch::Receiver<ChainTipData>) -> Self {
        Self { receiver }
    }
}

impl ChainTip for ChainTipReceiver {
    /// Return the height of the best chain tip.
    fn best_tip_height(&self) -> Option<block::Height> {
        self.receiver.borrow().as_ref().map(|block| block.height)
    }

    /// Return the block hash of the best chain tip.
    fn best_tip_hash(&self) -> Option<block::Hash> {
        self.receiver.borrow().as_ref().map(|block| block.hash)
    }
}
