//! Parameter types for the `getblocktemplate` RPC.

use crate::methods::get_block_template_rpcs::types::{hex_data::HexData, long_poll::LongPollId};

/// Defines whether the RPC method should generate a block template or attempt to validate a block proposal.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GetBlockTemplateRequestMode {
    /// Indicates a request for a block template.
    Template,

    /// Indicates a request to validate block data.
    Proposal,
}

impl Default for GetBlockTemplateRequestMode {
    fn default() -> Self {
        Self::Template
    }
}

/// Valid `capabilities` values that indicate client-side support.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GetBlockTemplateCapability {
    /// Long Polling support.
    /// Currently ignored by zebra.
    LongPoll,

    /// Information for coinbase transaction, default template data with the `coinbasetxn` field.
    /// Currently ignored by zebra.
    CoinbaseTxn,

    /// Coinbase value, template response provides a `coinbasevalue` field and omits `coinbasetxn` field.
    /// Currently ignored by zebra.
    CoinbaseValue,

    /// Components of the coinbase transaction.
    /// Currently ignored by zebra.
    CoinbaseAux,

    /// Currently ignored by zcashd and zebra.
    Proposal,

    /// Currently ignored by zcashd and zebra.
    ServerList,

    /// Currently ignored by zcashd and zebra.
    WorkId,

    /// Unknown capability to fill in for mutations.
    // TODO: Fill out valid mutations capabilities.
    //       The set of possible capabilities is open-ended, so we need to keep UnknownCapability.
    #[serde(other)]
    UnknownCapability,
}

/// Optional parameter `jsonrequestobject` for `getblocktemplate` RPC request.
///
/// The `data` field must be provided in `proposal` mode, and must be omitted in `template` mode.
/// All other fields are optional.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, Default)]
pub struct JsonParameters {
    /// Defines whether the RPC method should generate a block template or attempt to
    /// validate block data, checking against all of the server's usual acceptance rules
    /// (excluding the check for a valid proof-of-work).
    #[serde(default)]
    pub mode: GetBlockTemplateRequestMode,

    /// Must be omitted when `getblocktemplate` RPC is called in "template" mode (or when `mode` is omitted).
    /// Must be provided when `getblocktemplate` RPC is called in "proposal" mode.
    ///
    /// Hex-encoded block data to be validated and checked against the server's usual acceptance rules
    /// (excluding the check for a valid proof-of-work).
    pub data: Option<HexData>,

    /// A list of client-side supported capability features
    #[serde(default)]
    pub capabilities: Vec<GetBlockTemplateCapability>,

    /// An ID that delays the RPC response until the template changes.
    ///
    /// In Zebra, the ID represents the chain tip, max time, and mempool contents.
    #[serde(rename = "longpollid")]
    pub long_poll_id: Option<LongPollId>,

    /// The workid for the block template.
    ///
    /// currently unused.
    #[serde(rename = "workid")]
    pub _work_id: Option<String>,
}

impl JsonParameters {
    /// Returns Some(data) with the block proposal hexdata if in `Proposal` mode and `data` is provided.
    pub fn block_proposal_data(&self) -> Option<HexData> {
        match self {
            Self { data: None, .. }
            | Self {
                mode: GetBlockTemplateRequestMode::Template,
                ..
            } => None,

            Self {
                mode: GetBlockTemplateRequestMode::Proposal,
                data,
                ..
            } => data.clone(),
        }
    }
}
