//! State [`tower::Service`] response types.

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block},
    block_data::BlockData,
    orchard, sapling,
    serialization::DateTime32,
    subtree::{NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex},
    transaction::{self, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use zebra_chain::work::difficulty::CompactDifficulty;

// Allow *only* these unused imports, so that rustdoc link resolution
// will work with inline links.
#[allow(unused_imports)]
use crate::{ReadRequest, Request};

use crate::{service::read::AddressUtxos, TransactionLocation};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a [`StateService`](crate::service::StateService) [`Request`].
pub enum Response {
    /// Response to [`Request::CommitSemanticallyVerifiedBlock`] indicating that a block was
    /// successfully committed to the state.
    Committed(block::Hash),

    /// Response to [`Request::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`Request::Tip`] with the current best chain tip.
    //
    // TODO: remove this request, and replace it with a call to
    //       `LatestChainTip::best_tip_height_and_hash()`
    Tip(Option<(block::Height, block::Hash)>),

    /// Response to [`Request::BlockLocator`] with a block locator object.
    BlockLocator(Vec<block::Hash>),

    /// Response to [`Request::Transaction`] with the specified transaction.
    Transaction(Option<Arc<Transaction>>),

    /// Response to [`Request::UnspentBestChainUtxo`] with the UTXO
    UnspentBestChainUtxo(Option<transparent::Utxo>),

    /// Response to [`Request::Block`] with the specified block.
    Block(Option<Arc<Block>>),

    /// Response to [`Request::BlockAndSize`] with the specified block and size.
    BlockAndSize(Option<(Arc<Block>, usize)>),

    /// The response to a `BlockHeader` request.
    BlockHeader {
        /// The header of the requested block
        header: Arc<block::Header>,
        /// The hash of the requested block
        hash: block::Hash,
        /// The height of the requested block
        height: block::Height,
        /// The hash of the next block after the requested block
        next_block_hash: Option<block::Hash>,
    },

    /// The response to a `AwaitUtxo` request, from any non-finalized chains, finalized chain,
    /// pending unverified blocks, or blocks received after the request was sent.
    Utxo(transparent::Utxo),

    /// The response to a `FindBlockHashes` request.
    BlockHashes(Vec<block::Hash>),

    /// The response to a `FindBlockHeaders` request.
    BlockHeaders(Vec<block::CountedHeader>),

    /// Response to [`Request::CheckBestChainTipNullifiersAndAnchors`].
    ///
    /// Does not check transparent UTXO inputs
    ValidBestChainTipNullifiersAndAnchors,

    /// Response to [`Request::BestChainNextMedianTimePast`].
    /// Contains the median-time-past for the *next* block on the best chain.
    BestChainNextMedianTimePast(DateTime32),

    /// Response to [`Request::BestChainBlockHash`] with the specified block hash.
    BlockHash(Option<block::Hash>),

    /// Response to [`Request::KnownBlock`].
    KnownBlock(Option<KnownBlock>),

    /// Response to [`Request::CheckBlockProposalValidity`]
    ValidBlockProposal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// An enum of block stores in the state where a block hash could be found.
pub enum KnownBlock {
    /// Block is in the best chain.
    BestChain,

    /// Block is in a side chain.
    SideChain,

    /// Block is queued to be validated and committed, or rejected and dropped.
    Queue,
}

/// Information about a transaction in the best chain
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinedTx {
    /// The transaction.
    pub tx: Arc<Transaction>,

    /// The transaction height.
    pub height: block::Height,

    /// The number of confirmations for this transaction
    /// (1 + depth of block the transaction was found in)
    pub confirmations: u32,
}

impl MinedTx {
    /// Creates a new [`MinedTx`]
    pub fn new(tx: Arc<Transaction>, height: block::Height, confirmations: u32) -> Self {
        Self {
            tx,
            height,
            confirmations,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a read-only
/// [`ReadStateService`](crate::service::ReadStateService)'s [`ReadRequest`].
pub enum ReadResponse {
    /// Response to [`ReadRequest::UsageInfo`] with the current best chain tip.
    UsageInfo(u64),

    /// Response to [`ReadRequest::Tip`] with the current best chain tip.
    Tip(Option<(block::Height, block::Hash)>),

    /// Response to [`ReadRequest::TipPoolValues`] with
    /// the current best chain tip and its [`ValueBalance`].
    TipPoolValues {
        /// The current best chain tip height.
        tip_height: block::Height,
        /// The current best chain tip hash.
        tip_hash: block::Hash,
        /// The value pool balance at the current best chain tip.
        value_balance: ValueBalance<NonNegative>,
    },

    /// Response to [`ReadRequest::BlockData`] with
    /// the block data after the specified block.
    BlockData(Option<BlockData>),

    /// Response to [`ReadRequest::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`ReadRequest::Block`] with the specified block.
    Block(Option<Arc<Block>>),

    /// Response to [`ReadRequest::BlockAndSize`] with the specified block and
    /// serialized size.
    BlockAndSize(Option<(Arc<Block>, usize)>),

    /// The response to a `BlockHeader` request.
    BlockHeader {
        /// The header of the requested block
        header: Arc<block::Header>,
        /// The hash of the requested block
        hash: block::Hash,
        /// The height of the requested block
        height: block::Height,
        /// The hash of the next block after the requested block
        next_block_hash: Option<block::Hash>,
    },

    /// Response to [`ReadRequest::Transaction`] with the specified transaction.
    Transaction(Option<MinedTx>),

    /// Response to [`ReadRequest::TransactionIdsForBlock`],
    /// with an list of transaction hashes in block order,
    /// or `None` if the block was not found.
    TransactionIdsForBlock(Option<Arc<[transaction::Hash]>>),

    /// Response to [`ReadRequest::SpendingTransactionId`],
    /// with an list of transaction hashes in block order,
    /// or `None` if the block was not found.
    #[cfg(feature = "indexer")]
    TransactionId(Option<transaction::Hash>),

    /// Response to [`ReadRequest::BlockLocator`] with a block locator object.
    BlockLocator(Vec<block::Hash>),

    /// The response to a `FindBlockHashes` request.
    BlockHashes(Vec<block::Hash>),

    /// The response to a `FindBlockHeaders` request.
    BlockHeaders(Vec<block::CountedHeader>),

    /// The response to a `UnspentBestChainUtxo` request, from verified blocks in the
    /// _best_ non-finalized chain, or the finalized chain.
    UnspentBestChainUtxo(Option<transparent::Utxo>),

    /// The response to an `AnyChainUtxo` request, from verified blocks in
    /// _any_ non-finalized chain, or the finalized chain.
    ///
    /// This response is purely informational, there is no guarantee that
    /// the UTXO remains unspent in the best chain.
    AnyChainUtxo(Option<transparent::Utxo>),

    /// Response to [`ReadRequest::SaplingTree`] with the specified Sapling note commitment tree.
    SaplingTree(Option<Arc<sapling::tree::NoteCommitmentTree>>),

    /// Response to [`ReadRequest::OrchardTree`] with the specified Orchard note commitment tree.
    OrchardTree(Option<Arc<orchard::tree::NoteCommitmentTree>>),

    /// Response to [`ReadRequest::SaplingSubtrees`] with the specified Sapling note commitment
    /// subtrees.
    SaplingSubtrees(
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling::tree::Node>>,
    ),

    /// Response to [`ReadRequest::OrchardSubtrees`] with the specified Orchard note commitment
    /// subtrees.
    OrchardSubtrees(
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>>,
    ),

    /// Response to [`ReadRequest::AddressBalance`] with the total balance of the addresses.
    AddressBalance(Amount<NonNegative>),

    /// Response to [`ReadRequest::TransactionIdsByAddresses`]
    /// with the obtained transaction ids, in the order they appear in blocks.
    AddressesTransactionIds(BTreeMap<TransactionLocation, transaction::Hash>),

    /// Response to [`ReadRequest::UtxosByAddresses`] with found utxos and transaction data.
    AddressUtxos(AddressUtxos),

    /// Response to [`ReadRequest::CheckBestChainTipNullifiersAndAnchors`].
    ///
    /// Does not check transparent UTXO inputs
    ValidBestChainTipNullifiersAndAnchors,

    /// Response to [`ReadRequest::BestChainNextMedianTimePast`].
    /// Contains the median-time-past for the *next* block on the best chain.
    BestChainNextMedianTimePast(DateTime32),

    /// Response to [`ReadRequest::BestChainBlockHash`] with the specified block hash.
    BlockHash(Option<block::Hash>),

    /// Response to [`ReadRequest::ChainInfo`] with the state
    /// information needed by the `getblocktemplate` RPC method.
    ChainInfo(GetBlockTemplateChainInfo),

    /// Response to [`ReadRequest::SolutionRate`]
    SolutionRate(Option<u128>),

    /// Response to [`ReadRequest::CheckBlockProposalValidity`]
    ValidBlockProposal,

    /// Response to [`ReadRequest::TipBlockSize`]
    TipBlockSize(Option<usize>),
}

/// A structure with the information needed from the state to build a `getblocktemplate` RPC response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetBlockTemplateChainInfo {
    // Data fetched directly from the state tip.
    //
    /// The current state tip height.
    /// The block template for the candidate block has this hash as the previous block hash.
    pub tip_hash: block::Hash,

    /// The current state tip height.
    /// The block template for the candidate block is the next block after this block.
    /// Depends on the `tip_hash`.
    pub tip_height: block::Height,

    /// The history tree of the current best chain.
    /// Depends on the `tip_hash`.
    pub history_tree: Arc<zebra_chain::history_tree::HistoryTree>,

    // Data derived from the state tip and recent blocks, and the current local clock.
    //
    /// The expected difficulty of the candidate block.
    /// Depends on the `tip_hash`, and the local clock on testnet.
    pub expected_difficulty: CompactDifficulty,

    /// The current system time, adjusted to fit within `min_time` and `max_time`.
    /// Always depends on the local clock and the `tip_hash`.
    pub cur_time: DateTime32,

    /// The mininimum time the miner can use in this block.
    /// Depends on the `tip_hash`, and the local clock on testnet.
    pub min_time: DateTime32,

    /// The maximum time the miner can use in this block.
    /// Depends on the `tip_hash`, and the local clock on testnet.
    pub max_time: DateTime32,
}

/// Conversion from read-only [`ReadResponse`]s to read-write [`Response`]s.
///
/// Used to return read requests concurrently from the [`StateService`](crate::service::StateService).
impl TryFrom<ReadResponse> for Response {
    type Error = &'static str;

    fn try_from(response: ReadResponse) -> Result<Response, Self::Error> {
        match response {
            ReadResponse::Tip(height_and_hash) => Ok(Response::Tip(height_and_hash)),
            ReadResponse::Depth(depth) => Ok(Response::Depth(depth)),
            ReadResponse::BestChainNextMedianTimePast(median_time_past) => Ok(Response::BestChainNextMedianTimePast(median_time_past)),
            ReadResponse::BlockHash(hash) => Ok(Response::BlockHash(hash)),

            ReadResponse::Block(block) => Ok(Response::Block(block)),
            ReadResponse::BlockAndSize(block) => Ok(Response::BlockAndSize(block)),
            ReadResponse::BlockHeader {
                header,
                hash,
                height,
                next_block_hash
            } => Ok(Response::BlockHeader {
                header,
                hash,
                height,
                next_block_hash
            }),
            ReadResponse::Transaction(tx_info) => {
                Ok(Response::Transaction(tx_info.map(|tx_info| tx_info.tx)))
            }
            ReadResponse::UnspentBestChainUtxo(utxo) => Ok(Response::UnspentBestChainUtxo(utxo)),


            ReadResponse::AnyChainUtxo(_) => Err("ReadService does not track pending UTXOs. \
                                                  Manually unwrap the response, and handle pending UTXOs."),

            ReadResponse::BlockLocator(hashes) => Ok(Response::BlockLocator(hashes)),
            ReadResponse::BlockHashes(hashes) => Ok(Response::BlockHashes(hashes)),
            ReadResponse::BlockHeaders(headers) => Ok(Response::BlockHeaders(headers)),

            ReadResponse::ValidBestChainTipNullifiersAndAnchors => Ok(Response::ValidBestChainTipNullifiersAndAnchors),

            ReadResponse::UsageInfo(_)
            | ReadResponse::TipPoolValues { .. }
            | ReadResponse::BlockData(_)
            | ReadResponse::TransactionIdsForBlock(_)
            | ReadResponse::SaplingTree(_)
            | ReadResponse::OrchardTree(_)
            | ReadResponse::SaplingSubtrees(_)
            | ReadResponse::OrchardSubtrees(_)
            | ReadResponse::AddressBalance(_)
            | ReadResponse::AddressesTransactionIds(_)
            | ReadResponse::AddressUtxos(_)
            | ReadResponse::ChainInfo(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }

            #[cfg(feature = "indexer")]
            ReadResponse::TransactionId(_) => Err("there is no corresponding Response for this ReadResponse"),

            ReadResponse::ValidBlockProposal => Ok(Response::ValidBlockProposal),

            ReadResponse::SolutionRate(_) | ReadResponse::TipBlockSize(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }
        }
    }
}
