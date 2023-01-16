//! getblocktemplate proposal mode implementation.
//!
//! `ProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use std::{num::ParseIntError, str::FromStr, sync::Arc};

use zebra_chain::{
    block::{self, Block, Height},
    serialization::{DateTime32, SerializationError, ZcashDeserializeInto},
    work::equihash::Solution,
};

use crate::methods::{
    get_block_template_rpcs::types::{
        default_roots::DefaultRoots,
        get_block_template::{GetBlockTemplate, Response},
    },
    GetBlockHash,
};

/// Error response to a `getblocktemplate` RPC request in proposal mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalRejectReason {
    /// Block proposal rejected as invalid.
    Rejected,
}

/// Response to a `getblocktemplate` RPC request in proposal mode.
///
/// See <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// The source of the time in the block proposal header.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum TimeSource {
    /// The `curtime` field in the template.
    /// This is the default time source.
    #[default]
    CurTime,

    /// The `mintime` field in the template.
    MinTime,

    /// The `maxtime` field in the template.
    MaxTime,

    /// The supplied time, clamped within the template's `[mintime, maxtime]`.
    Clamped(DateTime32),

    /// The current local clock time, clamped within the template's `[mintime, maxtime]`.
    ClampedNow,

    /// The raw supplied time, ignoring the `mintime` and `maxtime` in the template.
    ///
    /// Warning: this can create an invalid block proposal.
    Raw(DateTime32),

    /// The raw current local time, ignoring the `mintime` and `maxtime` in the template.
    ///
    /// Warning: this can create an invalid block proposal.
    RawNow,
}

impl TimeSource {
    /// Returns the time from `template` using this time source.
    pub fn time_from_template(&self, template: &GetBlockTemplate) -> DateTime32 {
        use TimeSource::*;

        match self {
            CurTime => template.cur_time,
            MinTime => template.min_time,
            MaxTime => template.max_time,
            Clamped(time) => (*time).clamp(template.min_time, template.max_time),
            ClampedNow => DateTime32::now().clamp(template.min_time, template.max_time),
            Raw(time) => *time,
            RawNow => DateTime32::now(),
        }
    }
}

impl FromStr for TimeSource {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use TimeSource::*;

        match s.to_lowercase().as_str() {
            "curtime" => Ok(CurTime),
            "mintime" => Ok(MinTime),
            "maxtime" => Ok(MaxTime),
            "clampednow" => Ok(ClampedNow),
            "rawnow" => Ok(RawNow),
            // "raw"u32
            s if s.strip_prefix("raw").is_some() => {
                Ok(Raw(s.strip_prefix("raw").unwrap().parse()?))
            }
            // "clamped"u32 or just u32
            _ => Ok(Clamped(s.strip_prefix("clamped").unwrap_or(s).parse()?)),
        }
    }
}

/// Returns a block proposal generated from a [`GetBlockTemplate`] RPC response.
///
/// If `time_source` is not supplied, uses the current time from the template.
pub fn proposal_block_from_template(
    template: GetBlockTemplate,
    time_source: impl Into<Option<TimeSource>>,
) -> Result<Block, SerializationError> {
    let GetBlockTemplate {
        version,
        height,
        previous_block_hash: GetBlockHash(previous_block_hash),
        default_roots:
            DefaultRoots {
                merkle_root,
                block_commitments_hash,
                ..
            },
        bits: difficulty_threshold,
        ref coinbase_txn,
        transactions: ref tx_templates,
        ..
    } = template;

    if Height(height) > Height::MAX {
        Err(SerializationError::Parse(
            "height field must be lower than Height::MAX",
        ))?;
    };

    let time = time_source
        .into()
        .unwrap_or_default()
        .time_from_template(&template);

    let mut transactions = vec![coinbase_txn.data.as_ref().zcash_deserialize_into()?];

    for tx_template in tx_templates {
        transactions.push(tx_template.data.as_ref().zcash_deserialize_into()?);
    }

    Ok(Block {
        header: Arc::new(block::Header {
            version,
            previous_block_hash,
            merkle_root,
            commitment_bytes: block_commitments_hash.bytes_in_serialized_order().into(),
            time: time.into(),
            difficulty_threshold,
            nonce: [0; 32].into(),
            solution: Solution::for_proposal(),
        }),
        transactions,
    })
}
