//! Parameter and response types for the `submitblock` RPC.

use std::time::Duration;

use tokio::sync::mpsc;

use zebra_chain::block;

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::GetBlockTemplateHandler;

/// How long `submitblock` waits for a block to be verified and committed, before it returns
/// `inconclusive`.
///
/// A block is verified and committed in well under a second. But the block verifier waits for a
/// block's parent, and for the transparent outputs it spends. So a block whose parent never
/// arrives isn't answered until the state drops it from its queue, which takes about a hundred
/// blocks. A miner can find a block like that on early work, if the block it builds on is then
/// rejected.
pub(crate) const SUBMIT_BLOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the verification of a submitted block carries on in the background, after
/// `submitblock` has returned `inconclusive` for it.
///
/// This is the time that Zebra's syncer gives the block verifier for a block, which is longer
/// than any wait inside the verifier. A block that is in the state's queue by then stays there.
pub(crate) const SUBMIT_BLOCK_VERIFY_TIMEOUT: Duration = Duration::from_secs(8 * 60);

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
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SubmitBlockErrorResponse {
    /// Block was already committed to the non-finalized or finalized state
    Duplicate,
    /// Block was already added to the state queue or channel, but not yet committed to the non-finalized state
    DuplicateInconclusive,
    /// Block was already committed to the non-finalized state, but not on the best chain, or
    /// the block is still being verified
    Inconclusive,
    /// Block rejected as invalid
    Rejected,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(untagged)]
pub enum SubmitBlockResponse {
    /// Block was not successfully submitted, return error
    ErrorResponse(SubmitBlockErrorResponse),
    /// Block successfully submitted, returns null
    Accepted,
}

impl SubmitBlockResponse {
    /// Counts this response in `rpc.submitblock.outcomes.total`, by its zcashd-compatible result.
    ///
    /// Operational RPC errors return before a response exists, so they aren't counted as
    /// rejections.
    pub(in crate::methods) fn with_metrics(self) -> Self {
        let outcome = match &self {
            Self::Accepted => "accepted",
            Self::ErrorResponse(SubmitBlockErrorResponse::Duplicate) => "duplicate",
            Self::ErrorResponse(SubmitBlockErrorResponse::DuplicateInconclusive) => {
                "duplicate-inconclusive"
            }
            Self::ErrorResponse(SubmitBlockErrorResponse::Inconclusive) => "inconclusive",
            Self::ErrorResponse(SubmitBlockErrorResponse::Rejected) => "rejected",
        };
        metrics::counter!("rpc.submitblock.outcomes.total", "outcome" => outcome).increment(1);
        self
    }
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
