use std::sync::Arc;

use tokio::sync::watch;

use zebra_chain::{block, chain_tip::ChainTip, transaction};

use crate::{request::ContextuallyValidBlock, FinalizedBlock};

#[cfg(test)]
mod tests;

/// The internal watch channel data type for [`ChainTipSender`] and [`CurrentChainTip`].
type ChainTipData = Option<ChainTipBlock>;

/// A chain tip block, with precalculated block data.
///
/// Used to efficiently update the [`ChainTipSender`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTipBlock {
    pub(crate) hash: block::Hash,
    pub(crate) height: block::Height,

    /// The mined transaction IDs of the transactions in `block`,
    /// in the same order as `block.transactions`.
    pub(crate) transaction_hashes: Arc<[transaction::Hash]>,
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
    /// Create new linked instances of [`ChainTipSender`] and [`CurrentChainTip`],
    /// using `initial_tip` as the tip.
    pub fn new(initial_tip: impl Into<Option<ChainTipBlock>>) -> (Self, CurrentChainTip) {
        let (sender, receiver) = watch::channel(None);
        let mut sender = ChainTipSender {
            non_finalized_tip: false,
            sender,
            active_value: None,
        };
        let receiver = CurrentChainTip::new(receiver);

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
pub struct CurrentChainTip {
    receiver: watch::Receiver<ChainTipData>,
}

impl CurrentChainTip {
    /// Create a new chain tip receiver from a watch channel receiver.
    fn new(receiver: watch::Receiver<ChainTipData>) -> Self {
        Self { receiver }
    }
}

impl ChainTip for CurrentChainTip {
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
