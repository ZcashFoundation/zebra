//! Parameter and response types for the `submitblock` RPC.

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::get_block_template_rpcs::GetBlockTemplateRpc;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See notes for [`GetBlockTemplateRpc::submit_block`] method
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct JsonParameters {
    /// The workid for the block template.
    ///
    /// > If the server provided a workid, it MUST be included with submissions,
    /// currently unused.
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
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
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
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(untagged)]
pub enum Response {
    /// Block was not successfully submitted, return error
    ErrorResponse(ErrorResponse),
    /// Block successfully submitted, returns null
    Accepted,
}

impl From<ErrorResponse> for Response {
    fn from(error_response: ErrorResponse) -> Self {
        Self::ErrorResponse(error_response)
    }
}
