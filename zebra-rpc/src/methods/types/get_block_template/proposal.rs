//! getblocktemplate proposal mode implementation.
//!
//! `BlockProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use std::{num::ParseIntError, str::FromStr, sync::Arc};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::{DateTime32, SerializationError, ZcashDeserializeInto, BytesInDisplayOrder},
    work::equihash::Solution,
};
use zebra_node_services::BoxError;

use crate::methods::types::{
    default_roots::DefaultRoots,
    get_block_template::{BlockTemplateResponse, GetBlockTemplateResponse},
};

/// Response to a `getblocktemplate` RPC request in proposal mode.
///
/// <https://en.bitcoin.it/wiki/BIP_0022#Appendix:_Example_Rejection_Reasons>
///
/// Note:
/// The error response specification at <https://en.bitcoin.it/wiki/BIP_0023#Block_Proposal>
/// seems to have a copy-paste issue, or it is under-specified. We follow the `zcashd`
/// implementation instead, which returns a single raw string.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged, rename_all = "kebab-case")]
pub enum BlockProposalResponse {
    /// Block proposal was rejected as invalid.
    /// Contains the reason that the proposal was invalid.
    ///
    /// TODO: turn this into a typed error enum?
    Rejected(String),

    /// Block proposal was successfully validated, returns null.
    Valid,
}

impl BlockProposalResponse {
    /// Return a rejected response containing an error kind and detailed error info.
    ///
    /// Note: Error kind is currently ignored to match zcashd error format (`kebab-case` string).
    pub fn rejected<S: ToString>(_kind: S, error: BoxError) -> Self {
        // Make error `kebab-case` to match zcashd format.
        let error_kebab1 = format!("{error:?}")
            .replace(|c: char| !c.is_alphanumeric(), "-")
            .to_ascii_lowercase();
        // Remove consecutive duplicated `-` characters.
        let mut error_v: Vec<char> = error_kebab1.chars().collect();
        error_v.dedup_by(|a, b| a == &'-' && b == &'-');
        let error_kebab2: String = error_v.into_iter().collect();
        // Trim any leading or trailing `-` characters.
        let final_error = error_kebab2.trim_matches('-');

        BlockProposalResponse::Rejected(final_error.to_string())
    }

    /// Returns true if self is [`BlockProposalResponse::Valid`]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl From<BlockProposalResponse> for GetBlockTemplateResponse {
    fn from(proposal_response: BlockProposalResponse) -> Self {
        Self::ProposalMode(proposal_response)
    }
}

impl From<BlockTemplateResponse> for GetBlockTemplateResponse {
    fn from(template: BlockTemplateResponse) -> Self {
        Self::TemplateMode(Box::new(template))
    }
}

/// The source of the time in the block proposal header.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum BlockTemplateTimeSource {
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

impl BlockTemplateTimeSource {
    /// Returns the time from `template` using this time source.
    pub fn time_from_template(&self, template: &BlockTemplateResponse) -> DateTime32 {
        use BlockTemplateTimeSource::*;

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

    /// Returns true if this time source uses `max_time` in any way, including clamping.
    pub fn uses_max_time(&self) -> bool {
        use BlockTemplateTimeSource::*;

        match self {
            CurTime | MinTime => false,
            MaxTime | Clamped(_) | ClampedNow => true,
            Raw(_) | RawNow => false,
        }
    }

    /// Returns an iterator of time sources that are valid according to the consensus rules.
    pub fn valid_sources() -> impl IntoIterator<Item = BlockTemplateTimeSource> {
        use BlockTemplateTimeSource::*;

        [CurTime, MinTime, MaxTime, ClampedNow].into_iter()
    }
}

impl FromStr for BlockTemplateTimeSource {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use BlockTemplateTimeSource::*;

        match s.to_lowercase().as_str() {
            "curtime" => Ok(CurTime),
            "mintime" => Ok(MinTime),
            "maxtime" => Ok(MaxTime),
            "clampednow" => Ok(ClampedNow),
            "rawnow" => Ok(RawNow),
            s => match s.strip_prefix("raw") {
                // "raw"u32
                Some(raw_value) => Ok(Raw(raw_value.parse()?)),
                // "clamped"u32 or just u32
                // this is the default if the argument is just a number
                None => Ok(Clamped(s.strip_prefix("clamped").unwrap_or(s).parse()?)),
            },
        }
    }
}

/// Returns a block proposal generated from a [`BlockTemplateResponse`] RPC response.
///
/// If `time_source` is not supplied, uses the current time from the template.
pub fn proposal_block_from_template(
    template: &BlockTemplateResponse,
    time_source: impl Into<Option<BlockTemplateTimeSource>>,
    net: &Network,
) -> Result<Block, SerializationError> {
    let BlockTemplateResponse {
        version,
        height,
        previous_block_hash,
        default_roots:
            DefaultRoots {
                merkle_root,
                block_commitments_hash,
                chain_history_root,
                ..
            },
        bits: difficulty_threshold,
        ref coinbase_txn,
        transactions: ref tx_templates,
        ..
    } = *template;

    let height = Height(height);

    // TODO: Refactor [`Height`] so that these checks lose relevance.
    if height > Height::MAX {
        Err(SerializationError::Parse(
            "height of coinbase transaction is {height}, which exceeds the maximum of {Height::MAX}",
        ))?;
    };

    let time = time_source
        .into()
        .unwrap_or_default()
        .time_from_template(template)
        .into();

    let mut transactions = vec![coinbase_txn.data.as_ref().zcash_deserialize_into()?];

    for tx_template in tx_templates {
        transactions.push(tx_template.data.as_ref().zcash_deserialize_into()?);
    }

    let commitment_bytes = match NetworkUpgrade::current(net, height) {
        NetworkUpgrade::Canopy => chain_history_root.bytes_in_serialized_order(),
        NetworkUpgrade::Nu5 | NetworkUpgrade::Nu6 | NetworkUpgrade::Nu6_1 | NetworkUpgrade::Nu7 => {
            block_commitments_hash.bytes_in_serialized_order()
        }
        _ => Err(SerializationError::Parse(
            "Zebra does not support generating pre-Canopy block templates",
        ))?,
    }
    .into();

    Ok(Block {
        header: Arc::new(block::Header {
            version,
            previous_block_hash,
            merkle_root,
            commitment_bytes,
            time,
            difficulty_threshold,
            nonce: [0; 32].into(),
            solution: Solution::for_proposal(),
        }),
        transactions,
    })
}
