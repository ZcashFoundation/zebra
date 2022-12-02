//! Parameter types for the `getblocktemplate` RPC.

use super::hex_data::HexData;

/// Defines whether the RPC method should generate a block template or attempt to validate a block proposal.
/// `Proposal` mode is currently unsupported and will return an error.
#[derive(Debug, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GetBlockTemplateRequestMode {
    /// Indicates a request for a block template.
    Template,

    /// Indicates a request to validate block data.
    /// Currently unsupported and will return an error.
    Proposal,
}

impl Default for GetBlockTemplateRequestMode {
    fn default() -> Self {
        Self::Template
    }
}

/// Valid `capabilities` values that indicate client-side support.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
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
    #[serde(other)]
    UnknownCapability,
}

/// Optional argument `jsonrequestobject` for `getblocktemplate` RPC request.
///
/// The `data` field must be provided in `proposal` mode, and must be omitted in `template` mode.
/// All other fields are optional.
#[derive(Debug, serde::Deserialize, Default)]
pub struct JsonParameters {
    /// Must be set to "template" or omitted, as "proposal" mode is currently unsupported.
    ///
    /// Defines whether the RPC method should generate a block template or attempt to
    /// validate block data, checking against all of the server's usual acceptance rules
    /// (excluding the check for a valid proof-of-work).
    // TODO: Support `proposal` mode.
    #[serde(default)]
    pub mode: GetBlockTemplateRequestMode,

    /// Must be omitted as "proposal" mode is currently unsupported.
    ///
    /// Hex-encoded block data to be validated and checked against the server's usual acceptance rules
    /// (excluding the check for a valid proof-of-work) when `mode` is set to `proposal`.
    pub data: Option<HexData>,

    /// A list of client-side supported capability features
    // TODO: Fill out valid mutations capabilities.
    #[serde(default)]
    pub capabilities: Vec<GetBlockTemplateCapability>,

    /// An id to wait for, in zcashd this is the tip hash and an internal counter.
    ///
    /// If provided, the RPC response is delayed until the mempool or chain tip block changes.
    ///
    /// Currently unsupported and ignored by Zebra.
    pub longpollid: Option<String>,
}
