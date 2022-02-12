//! Access to Zebra chain tip information.
//!
//! Zebra has 3 different interfaces for access to chain tip information:
//! * [zebra_state::Request](crate::request): [tower::Service] requests about chain state,
//! * [LatestChainTip] for efficient access to the current best tip, and
//! * [ChainTipChange] to `await` specific changes to the chain tip.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::watch;
use tracing::instrument;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

#[cfg(any(test, feature = "proptest-impl"))]
use zebra_chain::serialization::arbitrary::datetime_full;
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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct ChainTipBlock {
    /// The hash of the best chain tip block.
    pub hash: block::Hash,

    /// The height of the best chain tip block.
    pub height: block::Height,

    /// The network block time of the best chain tip block.
    #[cfg_attr(
        any(test, feature = "proptest-impl"),
        proptest(strategy = "datetime_full()")
    )]
    pub time: DateTime<Utc>,

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
    pub previous_block_hash: block::Hash,
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
            time: block.header.time,
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
            time: block.header.time,
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
    #[instrument(skip(initial_tip), fields(new_height, new_hash))]
    pub fn new(
        initial_tip: impl Into<Option<ChainTipBlock>>,
        network: Network,
    ) -> (Self, LatestChainTip, ChainTipChange) {
        let initial_tip = initial_tip.into();
        ChainTipSender::record_new_tip(&initial_tip);

        let (sender, receiver) = watch::channel(None);

        let mut sender = ChainTipSender {
            use_non_finalized_tip: false,
            sender,
            active_value: None,
        };

        let current = LatestChainTip::new(receiver);
        let change = ChainTipChange::new(current.clone(), network);

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
            new_height,
            new_hash,
        ))]
    pub fn set_finalized_tip(&mut self, new_tip: impl Into<Option<ChainTipBlock>> + Clone) {
        let new_tip = new_tip.into();
        ChainTipSender::record_new_tip(&new_tip);

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
            new_height,
            new_hash,
        ))]
    pub fn set_best_non_finalized_tip(
        &mut self,
        new_tip: impl Into<Option<ChainTipBlock>> + Clone,
    ) {
        let new_tip = new_tip.into();
        ChainTipSender::record_new_tip(&new_tip);

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
    fn update(&mut self, new_tip: Option<ChainTipBlock>) {
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

    /// Record `new_tip` in the current span.
    ///
    /// Callers should create a new span with empty `new_height` and `new_hash` fields.
    fn record_new_tip(new_tip: &Option<ChainTipBlock>) {
        let span = tracing::Span::current();

        let new_height = new_tip.as_ref().map(|block| block.height);
        let new_hash = new_tip.as_ref().map(|block| block.hash);

        span.record("new_height", &tracing::field::debug(new_height));
        span.record("new_hash", &tracing::field::debug(new_hash));
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

    /// Retrieve a result `R` from the current [`ChainTipBlock`], if it's available.
    ///
    /// This helper method is a shorter way to borrow the value from the [`watch::Receiver`] and
    /// extract some information from it, while also adding the current chain tip block's fields as
    /// records to the current span.
    ///
    /// A single read lock is kept during the execution of the method, and it is dropped at the end
    /// of it.
    ///
    /// # Correctness
    ///
    /// To prevent deadlocks:
    ///
    /// - `receiver.borrow()` should not be called before this method while in the same scope.
    /// - `receiver.borrow()` should not be called inside the `action` closure.
    ///
    /// It is important to avoid calling `borrow` more than once in the same scope, which
    /// effectively tries to acquire two read locks to the shared data in the watch channel. If
    /// that is done, there's a chance that the [`watch::Sender`] tries to send a value, which
    /// starts acquiring a write-lock, and prevents further read-locks from being acquired until
    /// the update is finished.
    ///
    /// What can happen in that scenario is:
    ///
    /// 1. The receiver manages to acquire a read-lock for the first `borrow`
    /// 2. The sender starts acquiring the write-lock
    /// 3. The receiver fails to acquire a read-lock for the second `borrow`
    ///
    /// Now both the sender and the receivers hang, because the sender won't release the lock until
    /// it can update the value, and the receiver won't release its first read-lock until it
    /// acquires the second read-lock and finishes what it's doing.
    fn with_chain_tip_block<R>(&self, action: impl FnOnce(&ChainTipBlock) -> R) -> Option<R> {
        let span = tracing::Span::current();
        let borrow_guard = self.receiver.borrow();
        let chain_tip_block = borrow_guard.as_ref();

        span.record(
            "height",
            &tracing::field::debug(chain_tip_block.map(|block| block.height)),
        );
        span.record(
            "hash",
            &tracing::field::debug(chain_tip_block.map(|block| block.hash)),
        );
        span.record(
            "transaction_count",
            &tracing::field::debug(chain_tip_block.map(|block| block.transaction_hashes.len())),
        );

        chain_tip_block.map(action)
    }
}

impl ChainTip for LatestChainTip {
    #[instrument(skip(self))]
    fn best_tip_height(&self) -> Option<block::Height> {
        self.with_chain_tip_block(|block| block.height)
    }

    #[instrument(skip(self))]
    fn best_tip_hash(&self) -> Option<block::Hash> {
        self.with_chain_tip_block(|block| block.hash)
    }

    #[instrument(skip(self))]
    fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
        self.with_chain_tip_block(|block| block.time)
    }

    #[instrument(skip(self))]
    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
        self.with_chain_tip_block(|block| (block.height, block.time))
    }

    #[instrument(skip(self))]
    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        self.with_chain_tip_block(|block| block.transaction_hashes.clone())
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
    latest_chain_tip: LatestChainTip,

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
            last_change_hash = ?self.last_change_hash,
            network = ?self.network,
        ))]
    pub fn last_tip_change(&mut self) -> Option<TipAction> {
        let block = self.latest_chain_tip.with_chain_tip_block(|block| {
            if Some(block.hash) != self.last_change_hash {
                Some(block.clone())
            } else {
                // Ignore an unchanged tip.
                None
            }
        })??;

        let block_hash = block.hash;
        let tip_action = self.action(block);

        self.last_change_hash = Some(block_hash);

        Some(tip_action)
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

    /// Create a new [`ChainTipChange`] from a [`LatestChainTip`] receiver and [`Network`].
    fn new(latest_chain_tip: LatestChainTip, network: Network) -> Self {
        Self {
            latest_chain_tip,
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
            self.latest_chain_tip.receiver.changed().await?;

            // Wait until we have a new block
            //
            // last_tip_change() updates last_change_hash, but it doesn't call receiver.changed().
            // So code that uses both sync and async methods can have spurious pending changes.
            //
            // TODO: use `receiver.borrow_and_update()` in `with_chain_tip_block()`,
            //       once we upgrade to tokio 1.0 (#2200)
            //       and remove this extra check
            let new_block = self
                .latest_chain_tip
                .with_chain_tip_block(|block| {
                    if Some(block.hash) != self.last_change_hash {
                        Some(block.clone())
                    } else {
                        None
                    }
                })
                .flatten();

            if let Some(block) = new_block {
                return Ok(block);
            }
        }
    }
}

impl Clone for ChainTipChange {
    fn clone(&self) -> Self {
        Self {
            latest_chain_tip: self.latest_chain_tip.clone(),

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

    /// Returns the block height of this tip action,
    /// regardless of the underlying variant.
    pub fn best_tip_height(&self) -> block::Height {
        match self {
            Grow { block } => block.height,
            Reset { height, .. } => *height,
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
