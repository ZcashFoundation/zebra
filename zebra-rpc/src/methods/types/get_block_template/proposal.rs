//! getblocktemplate proposal mode implementation.
//!
//! `BlockProposalResponse` is the output of the `getblocktemplate` RPC method in 'proposal' mode.

use std::{collections::VecDeque, num::ParseIntError, str::FromStr, sync::Arc};

use tokio::sync::watch;

use zebra_chain::{
    block::{self, Block, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::{
        BytesInDisplayOrder, DateTime32, SerializationError, ZcashDeserialize, ZcashDeserializeInto,
    },
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
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
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

/// The number of validated block proposals whose transactions are kept.
///
/// This is the number of proposals that the block verifier keeps validation results for. Each
/// proposal can be as large as a block.
pub(crate) const MAX_PROPOSAL_TRANSACTIONS: usize = 8;

/// The decoded transactions of the latest block proposals that Zebra has validated, by their
/// serialized bytes.
///
/// A solved block has the same bytes after its header as the proposal it solves. So when the
/// transaction bytes of a submitted block are a validated proposal's, the block's transactions
/// are the proposal's, and they don't have to be decoded again. The block verifier then finds
/// the proposal's validation result for that block.
///
/// A refreshed block template often has the same transactions as the last one. Its proposal
/// takes them from here as well, so every validated proposal with those transactions has the
/// same transaction objects as the block that solves it. The block verifier only skips hashing
/// a solved block's transactions when they are the proposal's own objects.
///
/// Clones share the same proposals.
#[derive(Clone, Debug)]
pub(crate) struct ProposalTransactions(Arc<watch::Sender<VecDeque<(Arc<[u8]>, Arc<Block>)>>>);

impl Default for ProposalTransactions {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(VecDeque::new())))
    }
}

impl ProposalTransactions {
    /// Keeps the transactions of `proposal`, a validated block proposal that serializes to
    /// `proposal_bytes`.
    ///
    /// Transactions that are already kept become the latest ones again, like the proposal's
    /// validation result in the block verifier.
    pub(crate) fn insert(&self, proposal: Arc<Block>, proposal_bytes: &[u8]) {
        let Some((_header, transaction_bytes)) = split_header(proposal_bytes) else {
            return;
        };

        self.0.send_if_modified(|proposals| {
            let kept = proposals
                .iter()
                .position(|(bytes, _)| **bytes == *transaction_bytes)
                .and_then(|index| proposals.remove(index));

            if kept.is_none() && proposals.len() >= MAX_PROPOSAL_TRANSACTIONS {
                proposals.pop_front();
            }
            proposals.push_back(kept.unwrap_or_else(|| (transaction_bytes.into(), proposal)));

            // Nothing waits for changes to the proposals.
            false
        });
    }

    /// Returns the block that `block_bytes` decodes to, if its transaction bytes are a validated
    /// proposal's, without decoding the transactions. The block has that proposal's transactions.
    ///
    /// Returns `None` for any other bytes, including bytes that aren't a block. Those are for
    /// the block decoder.
    pub(crate) fn block_with_validated_transactions(&self, block_bytes: &[u8]) -> Option<Block> {
        // The decoder checks the block size limit as it reads, so check it for the bytes it
        // doesn't read.
        if u64::try_from(block_bytes.len()).ok()? > MAX_BLOCK_BYTES {
            return None;
        }

        let (header, transaction_bytes) = split_header(block_bytes)?;

        self.0
            .borrow()
            .iter()
            .find(|(bytes, _)| **bytes == *transaction_bytes)
            .map(|(_, proposal)| Block {
                header: Arc::new(header),
                transactions: proposal.transactions.clone(),
            })
    }
}

/// Decodes the header of the block in `block_bytes`, and returns it with the bytes that follow
/// it, which are the block's transactions.
fn split_header(mut block_bytes: &[u8]) -> Option<(block::Header, &[u8])> {
    let header = block::Header::zcash_deserialize(&mut block_bytes).ok()?;

    Some((header, block_bytes))
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
        NetworkUpgrade::Nu5
        | NetworkUpgrade::Nu6
        | NetworkUpgrade::Nu6_1
        | NetworkUpgrade::Nu6_2
        | NetworkUpgrade::Nu6_3
        | NetworkUpgrade::Nu7 => block_commitments_hash.bytes_in_serialized_order(),
        #[cfg(zcash_unstable = "zfuture")]
        NetworkUpgrade::ZFuture => block_commitments_hash.bytes_in_serialized_order(),
        NetworkUpgrade::Genesis
        | NetworkUpgrade::BeforeOverwinter
        | NetworkUpgrade::Overwinter
        | NetworkUpgrade::Sapling
        | NetworkUpgrade::Blossom
        | NetworkUpgrade::Heartwood => Err(SerializationError::Parse(
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
