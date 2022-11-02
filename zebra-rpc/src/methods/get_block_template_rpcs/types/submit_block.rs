//! Parameter and response types for the `submitblock` RPC.

// Allow doc links to these imports.
#[allow(unused_imports)]
use crate::methods::get_block_template_rpcs::GetBlockTemplateRpc;

/// Optional argument `jsonparametersobject` for `submitblock` RPC request
///
/// See notes for [`GetBlockTemplateRpc::submit_block`] method
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct JsonParameters {
    pub(crate) work_id: Option<String>,
}

/// Response to a `submitblock` RPC request.
///
/// Zebra never returns "duplicate-invalid", because it does not store invalid blocks.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Response {
    /// Block was already committed to the non-finalized or finalized state
    Duplicate,
    /// Block was already added to the state queue or channel, but not yet committed to the non-finalized state
    DuplicateInconclusive,
    /// Block was already committed to the non-finalized state, but not on the best chain
    Inconclusive,
    /// Block rejected as invalid
    Rejected,
    /// Block successfully submitted, return null
    #[serde(rename = "null")]
    Accepted,
}
