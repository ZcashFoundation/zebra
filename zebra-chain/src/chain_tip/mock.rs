//! Mock [`ChainTip`]s for use in tests.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use crate::{block, chain_tip::ChainTip, parameters::Network, transaction};

/// A sender to sets the values read by a [`MockChainTip`].
pub struct MockChainTipSender {
    /// A sender that sets the `best_tip_height` of a [`MockChainTip`].
    best_tip_height: watch::Sender<Option<block::Height>>,

    /// A sender that sets the `best_tip_hash` of a [`MockChainTip`].
    best_tip_hash: watch::Sender<Option<block::Hash>>,

    /// A sender that sets the `best_tip_block_time` of a [`MockChainTip`].
    best_tip_block_time: watch::Sender<Option<DateTime<Utc>>>,

    /// A sender that sets the `estimate_distance_to_network_chain_tip` of a [`MockChainTip`].
    estimated_distance_to_network_chain_tip: watch::Sender<Option<i32>>,
}

/// A mock [`ChainTip`] implementation that allows setting the `best_tip_height` externally.
#[derive(Clone, Debug)]
pub struct MockChainTip {
    /// A mocked `best_tip_height` value set by the [`MockChainTipSender`].
    best_tip_height: watch::Receiver<Option<block::Height>>,

    /// A mocked `best_tip_hash` value set by the [`MockChainTipSender`].
    best_tip_hash: watch::Receiver<Option<block::Hash>>,

    /// A mocked `best_tip_height` value set by the [`MockChainTipSender`].
    best_tip_block_time: watch::Receiver<Option<DateTime<Utc>>>,

    /// A mocked `estimate_distance_to_network_chain_tip` value set by the [`MockChainTipSender`].
    estimated_distance_to_network_chain_tip: watch::Receiver<Option<i32>>,
}

impl MockChainTip {
    /// Create a new [`MockChainTip`].
    ///
    /// Returns the [`MockChainTip`] instance and the endpoint to modiy the current best tip
    /// height.
    ///
    /// Initially, the best tip height is [`None`].
    pub fn new() -> (Self, MockChainTipSender) {
        let (height_sender, height_receiver) = watch::channel(None);
        let (hash_sender, hash_receiver) = watch::channel(None);
        let (time_sender, time_receiver) = watch::channel(None);
        let (estimated_distance_to_tip_sender, estimated_distance_to_tip_receiver) =
            watch::channel(None);

        let mock_chain_tip = MockChainTip {
            best_tip_height: height_receiver,
            best_tip_hash: hash_receiver,
            best_tip_block_time: time_receiver,
            estimated_distance_to_network_chain_tip: estimated_distance_to_tip_receiver,
        };

        let mock_chain_tip_sender = MockChainTipSender {
            best_tip_height: height_sender,
            best_tip_hash: hash_sender,
            best_tip_block_time: time_sender,
            estimated_distance_to_network_chain_tip: estimated_distance_to_tip_sender,
        };

        (mock_chain_tip, mock_chain_tip_sender)
    }
}

impl ChainTip for MockChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        *self.best_tip_height.borrow()
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        *self.best_tip_hash.borrow()
    }

    fn best_tip_height_and_hash(&self) -> Option<(block::Height, block::Hash)> {
        let height = (*self.best_tip_height.borrow())?;
        let hash = (*self.best_tip_hash.borrow())?;

        Some((height, hash))
    }

    fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
        *self.best_tip_block_time.borrow()
    }

    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
        let height = (*self.best_tip_height.borrow())?;
        let block_time = (*self.best_tip_block_time.borrow())?;

        Some((height, block_time))
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        unreachable!("Method not used in tests");
    }

    fn estimate_distance_to_network_chain_tip(
        &self,
        _network: Network,
    ) -> Option<(i32, block::Height)> {
        self.estimated_distance_to_network_chain_tip
            .borrow()
            .and_then(|estimated_distance| {
                self.best_tip_height()
                    .map(|tip_height| (estimated_distance, tip_height))
            })
    }
}

impl MockChainTipSender {
    /// Send a new best tip height to the [`MockChainTip`].
    pub fn send_best_tip_height(&self, height: impl Into<Option<block::Height>>) {
        self.best_tip_height
            .send(height.into())
            .expect("attempt to send a best tip height to a dropped `MockChainTip`");
    }

    /// Send a new best tip hash to the [`MockChainTip`].
    pub fn send_best_tip_hash(&self, hash: impl Into<Option<block::Hash>>) {
        self.best_tip_hash
            .send(hash.into())
            .expect("attempt to send a best tip hash to a dropped `MockChainTip`");
    }

    /// Send a new best tip block time to the [`MockChainTip`].
    pub fn send_best_tip_block_time(&self, block_time: impl Into<Option<DateTime<Utc>>>) {
        self.best_tip_block_time
            .send(block_time.into())
            .expect("attempt to send a best tip block time to a dropped `MockChainTip`");
    }

    /// Send a new estimated distance to network chain tip to the [`MockChainTip`].
    pub fn send_estimated_distance_to_network_chain_tip(&self, distance: impl Into<Option<i32>>) {
        self.estimated_distance_to_network_chain_tip
            .send(distance.into())
            .expect("attempt to send a best tip height to a dropped `MockChainTip`");
    }
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
}
