use std::sync::Arc;

use tokio::sync::watch;

use zebra_chain::{block, chain_tip::ChainTip, transaction};

#[cfg(test)]
mod prop;

/// A mock [`ChainTip`] implementation that allows setting the `best_tip_height` externally.
#[derive(Clone)]
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
    pub fn new() -> (Self, watch::Sender<Option<block::Height>>) {
        let (sender, receiver) = watch::channel(None);

        let mock_chain_tip = MockChainTip {
            best_tip_height: receiver,
        };

        (mock_chain_tip, sender)
    }
}

impl ChainTip for MockChainTip {
    fn best_tip_height(&self) -> Option<block::Height> {
        *self.best_tip_height.borrow()
    }

    fn best_tip_hash(&self) -> Option<block::Hash> {
        unreachable!("Method not used in `MinimumPeerVersion` tests");
    }

    fn best_tip_mined_transaction_ids(&self) -> Arc<[transaction::Hash]> {
        unreachable!("Method not used in `MinimumPeerVersion` tests");
    }
}
