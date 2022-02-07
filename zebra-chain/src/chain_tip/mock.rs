//! Mock [`ChainTip`]s for use in tests.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::watch;

use crate::{block, chain_tip::ChainTip, transaction};

/// A sender that sets the `best_tip_height` of a [`MockChainTip`].
pub struct MockChainTipSender {
    best_tip_height: watch::Sender<Option<block::Height>>,
}

/// A mock [`ChainTip`] implementation that allows setting the `best_tip_height` externally.
#[derive(Clone, Debug)]
pub struct MockChainTip {
    best_tip_height: watch::Receiver<Option<block::Height>>,
}

impl MockChainTip {
    /// Create a new [`MockChainTip`].
    ///
    /// Returns the [`MockChainTip`] instance and the endpoint to modiy the current best tip
    /// height.
    ///
    /// Initially, the best tip height is [`None`].
    pub fn new() -> (Self, MockChainTipSender) {
        let (sender, receiver) = watch::channel(None);

        let mock_chain_tip = MockChainTip {
            best_tip_height: receiver,
        };

        let mock_chain_tip_sender = MockChainTipSender {
            best_tip_height: sender,
        };

        (mock_chain_tip, mock_chain_tip_sender)
    }
}

impl ChainTip for MockChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        *self.best_tip_height.borrow()
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        unreachable!("Method not used in tests");
    }

    fn best_tip_block_time(&self) -> Option<DateTime<Utc>> {
        unreachable!("Method not used in tests");
    }

    fn best_tip_height_and_block_time(&self) -> Option<(block::Height, DateTime<Utc>)> {
        unreachable!("Method not used in tests");
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
}
