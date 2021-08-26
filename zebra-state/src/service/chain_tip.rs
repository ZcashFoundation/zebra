use tokio::sync::watch;

use zebra_chain::block;

#[cfg(test)]
mod tests;

/// A sender for recent changes to the non-finalized and finalized chain tips.
#[derive(Debug)]
pub struct ChainTipSender {
    finalized: Option<block::Height>,
    non_finalized: Option<block::Height>,
    sender: watch::Sender<Option<block::Height>>,
    // TODO: Replace with calls to `watch::Sender::borrow` once Tokio is updated to 1.0.0 (#2573)
    active_value: Option<block::Height>,
}

impl ChainTipSender {
    /// Create new linked instances of [`ChainTipSender`] and [`ChainTipReceiver`].
    pub fn new() -> (Self, ChainTipReceiver) {
        let (sender, receiver) = watch::channel(None);

        (
            ChainTipSender {
                finalized: None,
                non_finalized: None,
                sender,
                active_value: None,
            },
            ChainTipReceiver::new(receiver),
        )
    }

    /// Update the current finalized block height.
    ///
    /// May trigger an update to best tip height.
    pub fn set_finalized_height(&mut self, new_height: block::Height) {
        if self.finalized != Some(new_height) {
            self.finalized = Some(new_height);
            self.update();
        }
    }

    /// Update the current non-finalized block height.
    ///
    /// May trigger an update to the best tip height.
    pub fn set_best_non_finalized_height(&mut self, new_height: Option<block::Height>) {
        if self.non_finalized != new_height {
            self.non_finalized = new_height;
            self.update();
        }
    }

    /// Possibly send an update to listeners.
    ///
    /// An update is only sent if the current best tip height is different from the last best tip
    /// height that was sent.
    fn update(&mut self) {
        let new_value = self.non_finalized.max(self.finalized);

        if new_value != self.active_value {
            let _ = self.sender.send(new_value);
            self.active_value = new_value;
        }
    }
}

/// A receiver for recent changes to the non-finalized and finalized chain tips.
///
/// The latest changes are available from all cloned instances of this type.
#[derive(Clone, Debug)]
pub struct ChainTipReceiver {
    receiver: watch::Receiver<Option<block::Height>>,
}

impl ChainTipReceiver {
    /// Create a new chain tip receiver from a watch channel receiver.
    fn new(receiver: watch::Receiver<Option<block::Height>>) -> Self {
        Self { receiver }
    }

    /// Return the height of the best chain tip.
    ///
    /// The returned block height comes from:
    /// * the best non-finalized chain tip, if available, or
    /// * the finalized tip.
    pub fn best_tip_height(&self) -> Option<block::Height> {
        *self.receiver.borrow()
    }
}
