//! Real-time access to the current best non-finalized tip height and the finalized tip height.

use tokio::sync::watch;

use crate::block;

/// Receiver end to watch the current non-finalized best tip height and the finalized tip height.
#[derive(Clone, Debug)]
pub struct BestTipHeight {
    finalized: watch::Receiver<block::Height>,
    non_finalized: watch::Receiver<Option<block::Height>>,
}

impl BestTipHeight {
    /// Create the endpoints for the best tip height.
    ///
    /// Creates a [`BestTipHeight`] to act as the receiver endpoint, a
    /// [`watch::Sender<block::Height>`][watch::Sender] to act as the finalized tip sender endpoint,
    /// and a [`watch::Sender<Option<block::Height>>`][watch::Sender] to act as the best
    /// non-finalized tip sender endpoint.
    pub fn new() -> (
        Self,
        watch::Sender<block::Height>,
        watch::Sender<Option<block::Height>>,
    ) {
        let (finalized_sender, finalized_receiver) = watch::channel(block::Height(1));
        let (non_finalized_sender, non_finalized_receiver) = watch::channel(None);

        let best_tip_height = BestTipHeight {
            finalized: finalized_receiver,
            non_finalized: non_finalized_receiver,
        };

        (best_tip_height, finalized_sender, non_finalized_sender)
    }

    /// Retrieve the current finalized tip height.
    pub fn finalized(&self) -> block::Height {
        *self.finalized.borrow()
    }

    /// Retrieve the current best non-finalized tip height.
    ///
    /// This falls back to the finalized tip height if there are no non-finalized blocks.
    pub fn non_finalized(&self) -> block::Height {
        // Bind the borrow guard so that the non-finalized watch channel doesn't update while
        // reading from the finalized watch channel.
        let non_finalized = self.non_finalized.borrow();

        non_finalized.unwrap_or(*self.finalized.borrow())
    }
}
