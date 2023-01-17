//! `ProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use super::{GetBlockTemplate, Response};

/// Error response to a `getblocktemplate` RPC request in proposal mode.
///
/// See <https://en.bitcoin.it/wiki/BIP_0022#Appendix:_Example_Rejection_Reasons>
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalRejectReason {
    /// Block proposal rejected as invalid.
    Rejected,
}

/// Response to a `getblocktemplate` RPC request in proposal mode.
///
/// See <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged, rename_all = "kebab-case")]
pub enum ProposalResponse {
    /// Block proposal was rejected as invalid, returns `reject-reason` and server `capabilities`.
    ErrorResponse {
        /// Reason the proposal was invalid as-is.
        reject_reason: ProposalRejectReason,

        /// The getblocktemplate RPC capabilities supported by Zebra.
        capabilities: Vec<String>,
    },

    /// Block proposal was successfully validated, returns null.
    Valid,
}

impl From<ProposalRejectReason> for ProposalResponse {
    fn from(reject_reason: ProposalRejectReason) -> Self {
        Self::ErrorResponse {
            reject_reason,
            capabilities: GetBlockTemplate::capabilities(),
        }
    }
}

impl From<ProposalRejectReason> for Response {
    fn from(error_response: ProposalRejectReason) -> Self {
        Self::ProposalMode(ProposalResponse::from(error_response))
    }
}

impl From<ProposalResponse> for Response {
    fn from(proposal_response: ProposalResponse) -> Self {
        Self::ProposalMode(proposal_response)
    }
}

impl From<GetBlockTemplate> for Response {
    fn from(template: GetBlockTemplate) -> Self {
        Self::TemplateMode(Box::new(template))
    }
}
