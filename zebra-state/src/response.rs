//! State [`tower::Service`] response types.

use std::{collections::BTreeMap, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, ChainHistoryMmrRootHash},
    block_info::BlockInfo,
    orchard,
    parameters::Network,
    sapling,
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

use crate::{service::read::AddressUtxos, NonFinalizedState, TransactionLocation, WatchReceiver};

#[derive(Clone, Debug, PartialEq, Eq)]
/// A response to a [`StateService`](crate::service::StateService) [`Request`].
pub enum Response {
    /// Response to [`Request::CommitSemanticallyVerifiedBlock`] and [`Request::CommitCheckpointVerifiedBlock`]
    /// indicating that a block was successfully committed to the state.
    Committed(block::Hash),

    /// Response to [`Request::InvalidateBlock`] indicating that a block was found and
    /// invalidated in the state.
    Invalidated(block::Hash),

    /// Response to [`Request::ReconsiderBlock`] indicating that a previously invalidated
    /// block was reconsidered and re-committed to the non-finalized state. Contains a list
    /// of block hashes that were reconsidered in the state and successfully re-committed.
    Reconsidered(Vec<block::Hash>),

    /// Response to [`Request::Depth`] with the depth of the specified block.
    Depth(Option<u32>),

    /// Response to [`Request::Tip`] with the current best chain tip.
    //
    // TODO: remove this request, and replace it with a call to
    //       `LatestChainTip::best_tip_height_and_hash()`
    Tip(Option<(block::Height, block::Hash)>),

    #[cfg(zcash_unstable = "zip234")]
    /// Response to [`Request::TipPoolValues`] with the current best chain tip values.
    TipPoolValues {
        /// The current best chain tip height.
        tip_height: block::Height,
        /// The current best chain tip hash.
        tip_hash: block::Hash,
        /// The value pool balance at the current best chain tip.
        value_balance: ValueBalance<NonNegative>,
    },

    /// Response to [`Request::BlockLocator`] with a block locator object.
    BlockLocator(Vec<block::Hash>),

    /// Response to [`Request::Transaction`] with the specified transaction.
    Transaction(Option<Arc<Transaction>>),

    /// Response to [`Request::AnyChainTransaction`] with the specified transaction.
    AnyChainTransaction(Option<AnyTx>),

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
    /// Block is in the finalized portion of the best chain.
    Finalized,

    /// Block is in the best chain.
    BestChain,

    /// Block is in a side chain.
    SideChain,

    /// Block is in a block write channel
    WriteChannel,

    /// Block is queued to be validated and committed, or rejected and dropped.
    Queue,
}

impl std::fmt::Display for KnownBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KnownBlock::Finalized => write!(f, "finalized state"),
            KnownBlock::BestChain => write!(f, "best chain"),
            KnownBlock::SideChain => write!(f, "side chain"),
            KnownBlock::WriteChannel => write!(f, "block write channel"),
            KnownBlock::Queue => write!(f, "validation/commit queue"),
        }
    }
}

/// Information about a transaction in any chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnyTx {
    /// A transaction in the best chain.
    Mined(MinedTx),
    /// A transaction in a side chain, and the hash of the block it is in.
    Side((Arc<Transaction>, block::Hash)),
}

impl From<AnyTx> for Arc<Transaction> {
    fn from(any_tx: AnyTx) -> Self {
        match any_tx {
            AnyTx::Mined(mined_tx) => mined_tx.tx,
            AnyTx::Side((tx, _)) => tx,
        }
    }
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

    /// The time of the block where the transaction was mined.
    pub block_time: DateTime<Utc>,
}

impl MinedTx {
    /// Creates a new [`MinedTx`]
    pub fn new(
        tx: Arc<Transaction>,
        height: block::Height,
        confirmations: u32,
        block_time: DateTime<Utc>,
    ) -> Self {
        Self {
            tx,
            height,
            confirmations,
            block_time,
        }
    }
}

/// How many non-finalized block references to buffer in [`NonFinalizedBlocksListener`] before blocking sends.
///
/// # Correctness
///
/// This should be large enough to typically avoid blocking the sender when the non-finalized state is full so
/// that the [`NonFinalizedBlocksListener`] reliably receives updates whenever the non-finalized state changes.
///
/// It's okay to occasionally miss updates when the buffer is full, as the new blocks in the missed change will be
/// sent to the listener on the next change to the non-finalized state.
const NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE: usize = 1_000;

/// A listener for changes in the non-finalized state.
#[derive(Clone, Debug)]
pub struct NonFinalizedBlocksListener(
    pub Arc<tokio::sync::mpsc::Receiver<(zebra_chain::block::Hash, Arc<zebra_chain::block::Block>)>>,
);

impl NonFinalizedBlocksListener {
    /// Spawns a task to listen for changes in the non-finalized state and sends any blocks in the non-finalized state
    /// to the caller that have not already been sent.
    ///
    /// Returns a new instance of [`NonFinalizedBlocksListener`] for the caller to listen for new blocks in the non-finalized state.
    pub fn spawn(
        network: Network,
        mut non_finalized_state_receiver: WatchReceiver<NonFinalizedState>,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE);

        tokio::spawn(async move {
            // Start with an empty non-finalized state with the expectation that the caller doesn't yet have
            // any blocks from the non-finalized state.
            let mut prev_non_finalized_state = NonFinalizedState::new(&network);

            loop {
                // # Correctness
                //
                // This loop should check that the non-finalized state receiver has changed sooner
                // than the non-finalized state could possibly have changed to avoid missing updates, so
                // the logic here should be quicker than the contextual verification logic that precedes
                // commits to the non-finalized state.
                //
                // See the `NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE` documentation for more details.
                let latest_non_finalized_state = non_finalized_state_receiver.cloned_watch_data();

                let new_blocks = latest_non_finalized_state
                    .chain_iter()
                    .flat_map(|chain| {
                        // Take blocks from the chain in reverse height order until we reach a block that was
                        // present in the last seen copy of the non-finalized state.
                        let mut new_blocks: Vec<_> = chain
                            .blocks
                            .values()
                            .rev()
                            .take_while(|cv_block| {
                                !prev_non_finalized_state.any_chain_contains(&cv_block.hash)
                            })
                            .collect();
                        new_blocks.reverse();
                        new_blocks
                    })
                    .map(|cv_block| (cv_block.hash, cv_block.block.clone()));

                for new_block_with_hash in new_blocks {
                    if sender.send(new_block_with_hash).await.is_err() {
                        tracing::debug!("non-finalized blocks receiver closed, ending task");
                        return;
                    }
                }

                prev_non_finalized_state = latest_non_finalized_state;

                // Wait for the next update to the non-finalized state
                if let Err(error) = non_finalized_state_receiver.changed().await {
                    warn!(
                        ?error,
                        "non-finalized state receiver closed, is Zebra shutting down?"
                    );
                    break;
                }
            }
        });

        Self(Arc::new(receiver))
    }

    /// Consumes `self`, unwrapping the inner [`Arc`] and returning the non-finalized state change channel receiver.
    ///
    /// # Panics
    ///
    /// If the `Arc` has more than one strong reference, this will panic.
    pub fn unwrap(
        self,
    ) -> tokio::sync::mpsc::Receiver<(zebra_chain::block::Hash, Arc<zebra_chain::block::Block>)>
    {
        Arc::try_unwrap(self.0).unwrap()
    }
}

impl PartialEq for NonFinalizedBlocksListener {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for NonFinalizedBlocksListener {}

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

    /// Response to [`ReadRequest::BlockInfo`] with
    /// the block info after the specified block.
    BlockInfo(Option<BlockInfo>),

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

    /// Response to [`Request::Transaction`] with the specified transaction.
    AnyChainTransaction(Option<AnyTx>),

    /// Response to [`ReadRequest::TransactionIdsForBlock`],
    /// with an list of transaction hashes in block order,
    /// or `None` if the block was not found.
    TransactionIdsForBlock(Option<Arc<[transaction::Hash]>>),

    /// Response to [`ReadRequest::AnyChainTransactionIdsForBlock`], with an list of
    /// transaction hashes in block order and a flag indicating if the block is
    /// in the best chain, or `None` if the block was not found.
    AnyChainTransactionIdsForBlock(Option<(Arc<[transaction::Hash]>, bool)>),

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
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling_crypto::Node>>,
    ),

    /// Response to [`ReadRequest::OrchardSubtrees`] with the specified Orchard note commitment
    /// subtrees.
    OrchardSubtrees(
        BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>>,
    ),

    /// Response to [`ReadRequest::AddressBalance`] with the total balance of the addresses,
    /// and the total received funds, including change.
    AddressBalance {
        /// The total balance of the addresses.
        balance: Amount<NonNegative>,
        /// The total received funds in zatoshis, including change.
        received: u64,
    },

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

    /// Response to [`ReadRequest::NonFinalizedBlocksListener`]
    NonFinalizedBlocksListener(NonFinalizedBlocksListener),
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

    /// The FlyClient chain history root as of the end of the chain tip block.
    /// Depends on the `tip_hash`.
    pub chain_history_root: Option<ChainHistoryMmrRootHash>,

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
            ReadResponse::AnyChainTransaction(tx) => Ok(Response::AnyChainTransaction(tx)),
            ReadResponse::UnspentBestChainUtxo(utxo) => Ok(Response::UnspentBestChainUtxo(utxo)),


            ReadResponse::AnyChainUtxo(_) => Err("ReadService does not track pending UTXOs. \
                                                  Manually unwrap the response, and handle pending UTXOs."),

            ReadResponse::BlockLocator(hashes) => Ok(Response::BlockLocator(hashes)),
            ReadResponse::BlockHashes(hashes) => Ok(Response::BlockHashes(hashes)),
            ReadResponse::BlockHeaders(headers) => Ok(Response::BlockHeaders(headers)),

            ReadResponse::ValidBestChainTipNullifiersAndAnchors => Ok(Response::ValidBestChainTipNullifiersAndAnchors),

            ReadResponse::UsageInfo(_)
            | ReadResponse::BlockInfo(_)
            | ReadResponse::TransactionIdsForBlock(_)
            | ReadResponse::AnyChainTransactionIdsForBlock(_)
            | ReadResponse::SaplingTree(_)
            | ReadResponse::OrchardTree(_)
            | ReadResponse::SaplingSubtrees(_)
            | ReadResponse::OrchardSubtrees(_)
            | ReadResponse::AddressBalance { .. }
            | ReadResponse::AddressesTransactionIds(_)
            | ReadResponse::AddressUtxos(_)
            | ReadResponse::ChainInfo(_)
            | ReadResponse::NonFinalizedBlocksListener(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }

            #[cfg(feature = "indexer")]
            ReadResponse::TransactionId(_) => Err("there is no corresponding Response for this ReadResponse"),

            ReadResponse::ValidBlockProposal => Ok(Response::ValidBlockProposal),

            ReadResponse::SolutionRate(_) | ReadResponse::TipBlockSize(_) => {
                Err("there is no corresponding Response for this ReadResponse")
            }
            #[cfg(zcash_unstable = "zip234")]
            ReadResponse::TipPoolValues { tip_height, tip_hash, value_balance } => Ok(Response::TipPoolValues { tip_height, tip_hash, value_balance }),

            #[cfg(not(zcash_unstable = "zip234"))]
            ReadResponse::TipPoolValues { .. } => {
                Err("there is no corresponding Response for this ReadResponse")
            }
        }
    }
}
