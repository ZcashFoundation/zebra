//! getblocktemplate proposal mode implementation.
//!
//! `ProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use std::{num::ParseIntError, str::FromStr, sync::Arc};

use chrono::Utc;
use zebra_chain::{
    block::{self, Block, Height},
    serialization::{DateTime32, SerializationError, ZcashDeserializeInto},
    work::equihash::Solution,
};

use crate::methods::{
    get_block_template_rpcs::types::{
        default_roots::DefaultRoots, get_block_template::GetBlockTemplate,
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

/// The source of the time in the block proposal header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TimeSource {
    /// The `curtime` field in the template.
    CurTime,

    /// The `mintime` field in the template.
    MinTime,

    /// The `maxtime` field in the template.
    MaxTime,

    /// The supplied time, clamped within the template's `[mintime, maxtime]`.
    Clamped(DateTime32),

    /// The raw supplied time, ignoring the `mintime` and `maxtime` in the template.
    ///
    /// Warning: this can create an invalid block proposal.
    Raw(DateTime32),
}

impl FromStr for TimeSource {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use TimeSource::*;

        match s.to_lowercase().as_str() {
            "curtime" => Ok(CurTime),
            "mintime" => Ok(MinTime),
            "maxtime" => Ok(MaxTime),
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
pub fn proposal_block_from_template(
    GetBlockTemplate {
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
        coinbase_txn,
        transactions: tx_templates,
        ..
    }: GetBlockTemplate,
) -> Result<Block, SerializationError> {
    if Height(height) > Height::MAX {
        Err(SerializationError::Parse(
            "height field must be lower than Height::MAX",
        ))?;
    };

    let mut transactions = vec![coinbase_txn.data.as_ref().zcash_deserialize_into()?];

    for tx_template in tx_templates {
        transactions.push(tx_template.data.as_ref().zcash_deserialize_into()?);
    }

    Ok(Block {
        header: Arc::new(block::Header {
            version,
            previous_block_hash,
            merkle_root,
            commitment_bytes: block_commitments_hash.into(),
            time: Utc::now(),
            difficulty_threshold,
            nonce: [0; 32],
            solution: Solution::default(),
        }),
        transactions,
    })
}
