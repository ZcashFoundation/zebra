//! Mock [`ChainTip`]s for use in tests.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use crate::{block, chain_tip::ChainTip, transaction};

/// A sender to sets the values read by a [`MockChainTip`].
pub struct MockChainTipSender {
    /// A sender that sets the `best_tip_height` of a [`MockChainTip`].
    best_tip_height: watch::Sender<Option<block::Height>>,

    /// A sender that sets the `best_tip_hash` of a [`MockChainTip`].
    best_tip_hash: watch::Sender<Option<block::Hash>>,

    /// A sender that sets the `best_tip_block_time` of a [`MockChainTip`].
    best_tip_block_time: watch::Sender<Option<DateTime<Utc>>>,
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

        let mock_chain_tip = MockChainTip {
            best_tip_height: height_receiver,
            best_tip_hash: hash_receiver,
            best_tip_block_time: time_receiver,
        };

        let mock_chain_tip_sender = MockChainTipSender {
            best_tip_height: height_sender,
            best_tip_hash: hash_sender,
            best_tip_block_time: time_sender,
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
}
