//! Parameter and response types for the `submitblock` RPC.

use tokio::sync::mpsc;

use zebra_chain::block;

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::GetBlockTemplateHandler;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See the notes for the [`submit_block`](crate::methods::RpcServer::submit_block) RPC.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
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
