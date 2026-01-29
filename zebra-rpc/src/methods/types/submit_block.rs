//! Parameter and response types for the `submitblock` RPC.

use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use zebra_chain::block;

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::GetBlockTemplateHandler;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See the notes for the [`submit_block`](crate::methods::RpcServer::submit_block) RPC.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct SubmitBlockParameters {
    /// The workid for the block template. Currently unused.
    ///
    /// > If the server provided a workid, it MUST be included with submissions,
    ///
    /// Rationale:
    ///
    /// > If servers allow all mutations, it may be hard to identify which job it is based on.
    /// > While it may be possible to verify the submission by its content, it is much easier
    /// > to compare it to the job issued. It is very easy for the miner to keep track of this.
    /// > Therefore, using a "workid" is a very cheap solution to enable more mutations.
    ///
    /// <https://en.bitcoin.it/wiki/BIP_0022#Rationale>
    #[serde(rename = "workid")]
    pub _work_id: Option<String>,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubmitBlockErrorResponse {
    /// Block was already committed to the non-finalized or finalized state
    Duplicate,
    /// Block was already added to the state queue or channel, but not yet committed to the non-finalized state
    DuplicateInconclusive,
    /// Block was already committed to the non-finalized state, but not on the best chain
    Inconclusive,
    /// Block rejected as invalid
    Rejected,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SubmitBlockResponse {
    /// Block was not successfully submitted, return error
    ErrorResponse(SubmitBlockErrorResponse),
    /// Block successfully submitted, returns null
    Accepted,
}

impl Default for SubmitBlockResponse {
    fn default() -> Self {
        Self::ErrorResponse(SubmitBlockErrorResponse::Rejected)
    }
}

impl From<SubmitBlockErrorResponse> for SubmitBlockResponse {
    fn from(error_response: SubmitBlockErrorResponse) -> Self {
        Self::ErrorResponse(error_response)
    }
}

/// A submit block channel, used to inform the gossip task about mined blocks.
pub struct SubmitBlockChannel {
    /// The channel sender
    sender: mpsc::Sender<(block::Hash, block::Height)>,
    /// The channel receiver
    receiver: mpsc::Receiver<(block::Hash, block::Height)>,
}

impl SubmitBlockChannel {
    /// Creates a new submit block channel
    pub fn new() -> Self {
        /// How many unread messages the submit block channel should buffer before rejecting sends.
        ///
        /// This should be large enough to usually avoid rejecting sends. This channel is used by
        /// the block hash gossip task, which waits for a ready peer in the peer set while
        /// processing messages from this channel and could be much slower to gossip block hashes
        /// than it is to commit blocks and produce new block templates.
        const SUBMIT_BLOCK_CHANNEL_CAPACITY: usize = 10_000;

        let (sender, receiver) = mpsc::channel(SUBMIT_BLOCK_CHANNEL_CAPACITY);
        Self { sender, receiver }
    }

    /// Get the channel sender
    pub fn sender(&self) -> mpsc::Sender<(block::Hash, block::Height)> {
        self.sender.clone()
    }

    /// Get the channel receiver
    pub fn receiver(self) -> mpsc::Receiver<(block::Hash, block::Height)> {
        self.receiver
    }
}

impl Default for SubmitBlockChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// A counter for tracking the number of blocks submitted to this node.
///
/// This counter is incremented each time a block is successfully submitted via this node's
/// `submit_block` RPC. This includes blocks from Zebra's internal miner and blocks from
/// external mining software that connects to this node.
///
/// The counter serves two purposes:
/// - Exposes the `mining.blocks_mined` Prometheus metric for monitoring dashboards
/// - Provides a watch channel so the progress bar can display "mined N blocks" when at chain tip
///
/// # Cloning
///
/// Cloned instances share the same underlying watch channel sender,
/// so increments from any clone are visible to all receivers.
#[derive(Clone)]
pub struct MinedBlocksCounter {
    /// A sender to notify the progress bar task of count changes.
    ///
    /// The watch channel stores the current count. The progress bar holds the
    /// corresponding receiver and updates its display when the count changes.
    sender: Arc<watch::Sender<u64>>,
}

impl MinedBlocksCounter {
    /// Creates a new `MinedBlocksCounter` initialized to zero.
    ///
    /// Returns both the counter and a receiver that can be used to watch for count changes.
    /// The receiver is typically passed to the progress bar task.
    pub fn new() -> (Self, watch::Receiver<u64>) {
        let (sender, receiver) = watch::channel(0);
        (
            Self {
                sender: Arc::new(sender),
            },
            receiver,
        )
    }

    /// Increments the mined blocks count by 1 and notifies subscribers.
    ///
    /// This method is called by [`GetBlockTemplateHandler::advertise_mined_block`]
    /// when a block is successfully submitted.
    pub fn increment(&self) {
        self.sender.send_modify(|count| *count += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_counter_starts_at_zero() {
        let (_counter, rx) = MinedBlocksCounter::new();
        assert_eq!(*rx.borrow(), 0);
    }

    #[test]
    fn increment_increases_count() {
        let (counter, rx) = MinedBlocksCounter::new();

        counter.increment();
        assert_eq!(*rx.borrow(), 1);

        counter.increment();
        assert_eq!(*rx.borrow(), 2);
    }

    #[test]
    fn cloned_counters_share_state() {
        let (counter1, rx) = MinedBlocksCounter::new();
        let counter2 = counter1.clone();

        counter1.increment();
        counter2.increment();

        // Both clones increment the same underlying counter
        assert_eq!(*rx.borrow(), 2);
    }

    #[test]
    fn multiple_receivers_see_updates() {
        let (counter, rx1) = MinedBlocksCounter::new();
        let rx2 = rx1.clone();

        counter.increment();

        assert_eq!(*rx1.borrow(), 1);
        assert_eq!(*rx2.borrow(), 1);
    }

    /// Tests that the `mining.blocks_mined` metric can be incremented.
    ///
    /// This verifies the metric name is valid and the metrics system accepts it.
    /// The actual metric emission happens in [`GetBlockTemplateHandler::advertise_mined_block`].
    #[test]
    fn mining_blocks_mined_metric_is_valid() {
        // Verify the metric can be incremented without panicking
        metrics::counter!("mining.blocks_mined").increment(1);
    }
}
