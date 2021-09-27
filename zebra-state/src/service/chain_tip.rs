//! Access to Zebra chain tip information.
//!
//! Zebra has 3 different interfaces for access to chain tip information:
//! * [zebra_state::Request](crate::request): [tower::Service] requests about chain state,
//! * [LatestChainTip] for efficient access to the current best tip, and
//! * [ChainTipChange] to `await` specific changes to the chain tip.

use std::sync::Arc;

use tokio::sync::watch;
use tracing::instrument;

use zebra_chain::{
    block,
    chain_tip::ChainTip,
    parameters::{Network, NetworkUpgrade},
    transaction,
};

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

    /// The hash of the previous block in the best chain.
    /// This block is immediately behind the best chain tip.
    ///
    /// ## Note
    ///
    /// If the best chain fork has changed, or some blocks have been skipped,
    /// this hash will be different to the last returned `ChainTipBlock.hash`.
    pub(crate) previous_block_hash: block::Hash,
}

impl From<ContextuallyValidBlock> for ChainTipBlock {
    fn from(contextually_valid: ContextuallyValidBlock) -> Self {
        let ContextuallyValidBlock {
            block,
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
            previous_block_hash: block.header.previous_block_hash,
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
            transaction_hashes,
        } = finalized;
        Self {
            hash,
            height,
            transaction_hashes,
            previous_block_hash: block.header.previous_block_hash,
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
    use_non_finalized_tip: bool,

    /// The sender channel for chain tip data.
    sender: watch::Sender<ChainTipData>,

    /// A copy of the data in `sender`.
    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    active_value: ChainTipData,
}

impl ChainTipSender {
    /// Create new linked instances of [`ChainTipSender`], [`LatestChainTip`], and [`ChainTipChange`],
    /// using an `initial_tip` and a [`Network`].
    pub fn new(
        initial_tip: impl Into<Option<ChainTipBlock>>,
        network: Network,
    ) -> (Self, LatestChainTip, ChainTipChange) {
        let (sender, receiver) = watch::channel(None);

        let mut sender = ChainTipSender {
            use_non_finalized_tip: false,
            sender,
            active_value: None,
        };

        let current = LatestChainTip::new(receiver.clone());
        let change = ChainTipChange::new(receiver, network);

        sender.update(initial_tip);

        (sender, current, change)
    }

    /// Update the latest finalized tip.
    ///
    /// May trigger an update to the best tip.
    #[instrument(
        skip(self, new_tip),
        fields(
            old_use_non_finalized_tip = ?self.use_non_finalized_tip,
            old_height = ?self.active_value.as_ref().map(|block| block.height),
            old_hash = ?self.active_value.as_ref().map(|block| block.hash),
            new_height = ?new_tip.clone().into().map(|block| block.height),
            new_hash = ?new_tip.clone().into().map(|block| block.hash),
        ))]
    pub fn set_finalized_tip(&mut self, new_tip: impl Into<Option<ChainTipBlock>> + Clone) {
        if !self.use_non_finalized_tip {
            self.update(new_tip);
        }
    }

    /// Update the latest non-finalized tip.
    ///
    /// May trigger an update to the best tip.
    #[instrument(
        skip(self, new_tip),
        fields(
            old_use_non_finalized_tip = ?self.use_non_finalized_tip,
            old_height = ?self.active_value.as_ref().map(|block| block.height),
            old_hash = ?self.active_value.as_ref().map(|block| block.hash),
            new_height = ?new_tip.clone().into().map(|block| block.height),
            new_hash = ?new_tip.clone().into().map(|block| block.hash),
        ))]
    pub fn set_best_non_finalized_tip(
        &mut self,
        new_tip: impl Into<Option<ChainTipBlock>> + Clone,
    ) {
        let new_tip = new_tip.into();

        // once the non-finalized state becomes active, it is always populated
        // but ignoring `None`s makes the tests easier
        if new_tip.is_some() {
            self.use_non_finalized_tip = true;
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
/// Each method returns data from the latest tip,
/// regardless of how many times you call it.
///
/// Cloned instances provide identical tip data.
///
/// The chain tip data is based on:
/// * the best non-finalized chain tip, if available, or
/// * the finalized tip.
///
/// ## Note
///
/// If a lot of blocks are committed at the same time,
/// the latest tip will skip some blocks in the chain.
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
    #[instrument(
        skip(self),
        fields(
            height = ?self.receiver.borrow().as_ref().map(|block| block.height),
            hash = ?self.receiver.borrow().as_ref().map(|block| block.hash),
        ))]
    fn best_tip_height(&self) -> Option<block::Height> {
        self.receiver.borrow().as_ref().map(|block| block.height)
    }

    /// Return the block hash of the best chain tip.
    #[instrument(
        skip(self),
        fields(
            height = ?self.receiver.borrow().as_ref().map(|block| block.height),
            hash = ?self.receiver.borrow().as_ref().map(|block| block.hash),
        ))]
    fn best_tip_hash(&self) -> Option<block::Hash> {
        self.receiver.borrow().as_ref().map(|block| block.hash)
    }

    /// Return the mined transaction IDs of the transactions in the best chain tip block.
    ///
    /// All transactions with these mined IDs should be rejected from the mempool,
    /// even if their authorizing data is different.
    #[instrument(
        skip(self),
        fields(
            height = ?self.receiver.borrow().as_ref().map(|block| block.height),
            hash = ?self.receiver.borrow().as_ref().map(|block| block.hash),
            transaction_count = ?self.receiver.borrow().as_ref().map(|block| block.transaction_hashes.len()),
        ))]
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        self.receiver
            .borrow()
            .as_ref()
            .map(|block| block.transaction_hashes.clone())
            .unwrap_or_else(|| Arc::new([]))
    }
}

/// A chain tip change monitor.
///
/// Awaits changes and resets of the state's best chain tip,
/// returning the latest [`TipAction`] once the state is updated.
///
/// Each cloned instance separately tracks the last block data it provided.
/// If the best chain fork has changed since the last [`tip_change`] on that instance,
/// it returns a [`Reset`].
///
/// The chain tip data is based on:
/// * the best non-finalized chain tip, if available, or
/// * the finalized tip.
#[derive(Debug)]
pub struct ChainTipChange {
    /// The receiver for the current chain tip's data.
    receiver: watch::Receiver<ChainTipData>,

    /// The most recent [`block::Hash`] provided by this instance.
    ///
    /// ## Note
    ///
    /// If the best chain fork has changed, or some blocks have been skipped,
    /// this hash will be different to the last returned `ChainTipBlock.hash`.
    last_change_hash: Option<block::Hash>,

    /// The network for the chain tip.
    network: Network,
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
    /// Wait until the tip has changed, then return the corresponding [`TipAction`].
    ///
    /// The returned action describes how the tip has changed
    /// since the last call to this method.
    ///
    /// If there have been no changes since the last time this method was called,
    /// it waits for the next tip change before returning.
    ///
    /// If there have been multiple changes since the last time this method was called,
    /// they are combined into a single [`TipAction::Reset`].
    ///
    /// Returns an error if communication with the state is lost.
    ///
    /// ## Note
    ///
    /// If a lot of blocks are committed at the same time,
    /// the change will skip some blocks, and return a [`Reset`].
    #[instrument(
        skip(self),
        fields(
            current_height = ?self.receiver.borrow().as_ref().map(|block| block.height),
            current_hash = ?self.receiver.borrow().as_ref().map(|block| block.hash),
            last_change_hash = ?self.last_change_hash,
            network = ?self.network,
        ))]
    pub async fn wait_for_tip_change(&mut self) -> Result<TipAction, watch::error::RecvError> {
        let block = self.tip_block_change().await?;

        let action = self.action(block.clone());

        self.last_change_hash = Some(block.hash);

        Ok(action)
    }

    /// Returns:
    /// - `Some(`[`TipAction`]`)` if there has been a change since the last time the method was called.
    /// - `None` if there has been no change.
    ///
    /// See [`wait_for_tip_change`] for details.
    #[instrument(
        skip(self),
        fields(
            current_height = ?self.receiver.borrow().as_ref().map(|block| block.height),
            current_hash = ?self.receiver.borrow().as_ref().map(|block| block.hash),
            last_change_hash = ?self.last_change_hash,
            network = ?self.network,
        ))]
    pub fn last_tip_change(&mut self) -> Option<TipAction> {
        // Obtain the tip block.
        let block = self.best_tip_block()?;

        // Ignore an unchanged tip.
        if Some(block.hash) == self.last_change_hash {
            return None;
        }

        let action = self.action(block.clone());

        self.last_change_hash = Some(block.hash);

        Some(action)
    }

    /// Return an action based on `block` and the last change we returned.
    fn action(&self, block: ChainTipBlock) -> TipAction {
        // check for an edge case that's dealt with by other code
        assert!(
            Some(block.hash) != self.last_change_hash,
            "ChainTipSender and ChainTipChange ignore unchanged tips"
        );

        // If the previous block hash doesn't match, reset.
        // We've either:
        // - just initialized this instance,
        // - changed the best chain to another fork (a rollback), or
        // - skipped some blocks in the best chain.
        //
        // Consensus rules:
        //
        // > It is possible for a reorganization to occur
        // > that rolls back from after the activation height, to before that height.
        // > This can handled in the same way as any regular chain orphaning or reorganization,
        // > as long as the new chain is valid.
        //
        // https://zips.z.cash/zip-0200#chain-reorganization

        // If we're at a network upgrade activation block, reset.
        //
        // Consensus rules:
        //
        // > When the current chain tip height reaches ACTIVATION_HEIGHT,
        // > the node's local transaction memory pool SHOULD be cleared of transactions
        // > that will never be valid on the post-upgrade consensus branch.
        //
        // https://zips.z.cash/zip-0200#memory-pool
        //
        // Skipped blocks can include network upgrade activation blocks.
        // Fork changes can activate or deactivate a network upgrade.
        // So we must perform the same actions for network upgrades and skipped blocks.
        if Some(block.previous_block_hash) != self.last_change_hash
            || NetworkUpgrade::is_activation_height(self.network, block.height)
        {
            TipAction::reset_with(block)
        } else {
            TipAction::grow_with(block)
        }
    }

    /// Create a new [`ChainTipChange`] from a watch channel receiver and [`Network`].
    fn new(receiver: watch::Receiver<ChainTipData>, network: Network) -> Self {
        Self {
            receiver,
            last_change_hash: None,
            network,
        }
    }

    /// Wait until the next chain tip change, then return the corresponding [`ChainTipBlock`].
    ///
    /// Returns an error if communication with the state is lost.
    async fn tip_block_change(&mut self) -> Result<ChainTipBlock, watch::error::RecvError> {
        loop {
            // If there are multiple changes while this code is executing,
            // we don't rely on getting the first block or the latest block
            // after the change notification.
            // Any block update after the change will do,
            // we'll catch up with the tip after the next change.
            self.receiver.changed().await?;

            // Wait until there is actually Some block,
            // so we don't have `Option`s inside `TipAction`s.
            if let Some(block) = self.best_tip_block() {
                // Wait until we have a new block
                //
                // last_tip_change() updates last_change_hash, but it doesn't call receiver.changed().
                // So code that uses both sync and async methods can have spurious pending changes.
                //
                // TODO: use `receiver.borrow_and_update()` in `best_tip_block()`,
                //       once we upgrade to tokio 1.0
                //       and remove this extra check
                if Some(block.hash) != self.last_change_hash {
                    return Ok(block);
                }
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
            last_change_hash: None,

            network: self.network,
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

    /// Returns a [`Grow`] based on `block`.
    pub(crate) fn grow_with(block: ChainTipBlock) -> Self {
        Grow { block }
    }

    /// Returns a [`Reset`] based on `block`.
    pub(crate) fn reset_with(block: ChainTipBlock) -> Self {
        Reset {
            height: block.height,
            hash: block.hash,
        }
    }

    /// Converts this [`TipAction`] into a [`Reset`].
    ///
    /// Designed for use in tests.
    #[cfg(test)]
    pub(crate) fn into_reset(self) -> Self {
        match self {
            Grow { block } => Reset {
                height: block.height,
                hash: block.hash,
            },
            reset @ Reset { .. } => reset,
        }
    }
}
