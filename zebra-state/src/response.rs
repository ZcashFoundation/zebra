//! State [`tower::Service`] response types.

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block},
    orchard, sapling,
    transaction::{self, Transaction},
    transparent,
};

#[cfg(feature = "getblocktemplate-rpcs")]
use zebra_chain::work::difficulty::CompactDifficulty;

// Allow *only* these unused imports, so that rustdoc link resolution
// will work with inline links.
#[allow(unused_imports)]
use crate::{ReadRequest, Request};

use crate::{service::read::AddressUtxos, TransactionLocation};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a [`StateService`](crate::service::StateService) [`Request`].
pub enum Response {
    /// Response to [`Request::CommitBlock`] indicating that a block was
    /// successfully committed to the state.
    Committed(block::Hash),

    /// Response to [`Request::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`Request::Tip`] with the current best chain tip.
    Tip(Option<(block::Height, block::Hash)>),

    /// Response to [`Request::BlockLocator`] with a block locator object.
    BlockLocator(Vec<block::Hash>),

    /// Response to [`Request::Transaction`] with the specified transaction.
    Transaction(Option<Arc<Transaction>>),

    /// Response to [`Request::UnspentBestChainUtxo`] with the UTXO
    UnspentBestChainUtxo(Option<transparent::Utxo>),

    /// Response to [`Request::Block`] with the specified block.
    Block(Option<Arc<Block>>),

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a read-only
/// [`ReadStateService`](crate::service::ReadStateService)'s
/// [`ReadRequest`](crate::ReadRequest).
pub enum ReadResponse {
    /// Response to [`ReadRequest::Tip`] with the current best chain tip.
    Tip(Option<(block::Height, block::Hash)>),

    /// Response to [`ReadRequest::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`ReadRequest::Block`] with the specified block.
    Block(Option<Arc<Block>>),

    /// Response to [`ReadRequest::Transaction`] with the specified transaction.
    Transaction(Option<(Arc<Transaction>, block::Height)>),

    /// Response to [`ReadRequest::TransactionIdsForBlock`],
    /// with an list of transaction hashes in block order,
    /// or `None` if the block was not found.
    TransactionIdsForBlock(Option<Arc<[transaction::Hash]>>),

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

    #[cfg(feature = "getblocktemplate-rpcs")]
    /// Response to [`ReadRequest::BestChainBlockHash`](crate::ReadRequest::BestChainBlockHash) with the
    /// specified block hash.
    BlockHash(Option<block::Hash>),

    #[cfg(feature = "getblocktemplate-rpcs")]
    /// Response to [`ReadRequest::ChainInfo`](crate::ReadRequest::ChainInfo) with the state
    /// information needed by the `getblocktemplate` RPC method.
    ChainInfo(GetBlockTemplateChainInfo),
}

#[cfg(feature = "getblocktemplate-rpcs")]
/// A structure with the information needed from the state to build a `getblocktemplate` RPC response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GetBlockTemplateChainInfo {
    /// The current state tip height.
    /// The block template OAfor the candidate block is the next block after this block.
    pub tip_height: block::Height,

    /// The current state tip height.
    /// The block template for the candidate block has this hash as the previous block hash.
    pub tip_hash: block::Hash,

    /// The expected difficulty of the candidate block.
    pub expected_difficulty: CompactDifficulty,

    /// The current system time, adjusted to fit within `min_time` and `max_time`.
    pub cur_time: chrono::DateTime<chrono::Utc>,

    /// The mininimum time the miner can use in this block.
    pub min_time: chrono::DateTime<chrono::Utc>,

    /// The maximum time the miner can use in this block.
    pub max_time: chrono::DateTime<chrono::Utc>,

    /// The history tree of the current best chain.
    pub history_tree: Arc<zebra_chain::history_tree::HistoryTree>,
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

            ReadResponse::Block(block) => Ok(Response::Block(block)),
            ReadResponse::Transaction(tx_and_height) => {
                Ok(Response::Transaction(tx_and_height.map(|(tx, _height)| tx)))
            }
            ReadResponse::UnspentBestChainUtxo(utxo) => Ok(Response::UnspentBestChainUtxo(utxo)),


            ReadResponse::AnyChainUtxo(_) => Err("ReadService does not track pending UTXOs. \
                                                  Manually unwrap the response, and handle pending UTXOs."),

            ReadResponse::BlockLocator(hashes) => Ok(Response::BlockLocator(hashes)),
            ReadResponse::BlockHashes(hashes) => Ok(Response::BlockHashes(hashes)),
            ReadResponse::BlockHeaders(headers) => Ok(Response::BlockHeaders(headers)),

            ReadResponse::ValidBestChainTipNullifiersAndAnchors => Ok(Response::ValidBestChainTipNullifiersAndAnchors),

            ReadResponse::TransactionIdsForBlock(_)
            | ReadResponse::SaplingTree(_)
            | ReadResponse::OrchardTree(_)
            | ReadResponse::AddressBalance(_)
            | ReadResponse::AddressesTransactionIds(_)
            | ReadResponse::AddressUtxos(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }

            #[cfg(feature = "getblocktemplate-rpcs")]
            ReadResponse::BlockHash(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }
            #[cfg(feature = "getblocktemplate-rpcs")]
            ReadResponse::ChainInfo(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }
        }
    }
}
