//! Parameter and response types for the `submitblock` RPC.

use tokio::sync::watch;

use zebra_chain::{block, parameters::GENESIS_PREVIOUS_BLOCK_HASH};

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::get_block_template::GetBlockTemplateHandler;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See notes for [`crate::methods::GetBlockTemplateRpcServer::submit_block`] method
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct JsonParameters {
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
pub enum ErrorResponse {
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
pub enum Response {
    /// Block was not successfully submitted, return error
    ErrorResponse(ErrorResponse),
    /// Block successfully submitted, returns null
    Accepted,
}

impl Default for Response {
    fn default() -> Self {
        Self::ErrorResponse(ErrorResponse::Rejected)
    }
}

impl From<ErrorResponse> for Response {
    fn from(error_response: ErrorResponse) -> Self {
        Self::ErrorResponse(error_response)
    }
}

/// A submit block channel, used to inform the gossip task about mined blocks.
pub struct SubmitBlockChannel {
    /// The channel sender
    sender: watch::Sender<(block::Hash, block::Height)>,
    /// The channel receiver
    receiver: watch::Receiver<(block::Hash, block::Height)>,
}

impl SubmitBlockChannel {
    /// Create a new submit block channel
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel((GENESIS_PREVIOUS_BLOCK_HASH, block::Height::MIN));
        Self { sender, receiver }
    }

    /// Get the channel sender
    pub fn sender(&self) -> watch::Sender<(block::Hash, block::Height)> {
        self.sender.clone()
    }

    /// Get the channel receiver
    pub fn receiver(&self) -> watch::Receiver<(block::Hash, block::Height)> {
        self.receiver.clone()
    }
}

impl Default for SubmitBlockChannel {
    fn default() -> Self {
        Self::new()
    }
}
