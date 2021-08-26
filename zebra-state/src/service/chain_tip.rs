use tokio::sync::watch;

use zebra_chain::{block, chain_tip::ChainTip};

#[cfg(test)]
mod tests;

/// The internal watch channel data type for [`ChainTipSender`] and [`ChainTipReceiver`].
type ChainTipData = Option<block::Height>;

/// A sender for recent changes to the non-finalized and finalized chain tips.
#[derive(Debug)]
pub struct ChainTipSender {
    /// Have we got any chain tips from the non-finalized state?
    ///
    /// Once this flag is set, we ignore the finalized state.
    non_finalized_tip: bool,

    /// The sender channel for chain tip data.
    sender: watch::Sender<ChainTipData>,

    /// A copy of the data in `sender`.
    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    active_value: ChainTipData,
}

impl ChainTipSender {
    /// Create new linked instances of [`ChainTipSender`] and [`ChainTipReceiver`].
    pub fn new(initial_tip_height: impl Into<Option<block::Height>>) -> (Self, ChainTipReceiver) {
        let (sender, receiver) = watch::channel(None);
        let mut sender = ChainTipSender {
            non_finalized_tip: false,
            sender,
            active_value: None,
        };
        let receiver = ChainTipReceiver::new(receiver);

        sender.update(initial_tip_height);

        (sender, receiver)
    }

    /// Update the current finalized block height.
    ///
    /// May trigger an update to best tip height.
    pub fn set_finalized_height(&mut self, new_height: impl Into<Option<block::Height>>) {
        if !self.non_finalized_tip {
            self.update(new_height);
        }
    }

    /// Update the current non-finalized block height.
    ///
    /// May trigger an update to the best tip height.
    pub fn set_best_non_finalized_height(&mut self, new_height: impl Into<Option<block::Height>>) {
        self.non_finalized_tip = true;

        self.update(new_height)
    }

    /// Possibly send an update to listeners.
    ///
    /// An update is only sent if the current best tip height is different from the last best tip
    /// height that was sent.
    fn update(&mut self, new_height: impl Into<Option<block::Height>>) {
        let new_height = new_height.into();

        if new_height != self.active_value {
            let _ = self.sender.send(new_height);
            self.active_value = new_height;
        }
    }
}

/// A receiver for recent changes to the non-finalized and finalized chain tips.
///
/// The latest changes are available from all cloned instances of this type.
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
    ///
    /// The returned block height comes from:
    /// * the best non-finalized chain tip, if available, or
    /// * the finalized tip.
    fn best_tip_height(&self) -> Option<block::Height> {
        *self.receiver.borrow()
    }
}
