//! State [`tower::Service`] response types.

use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, ChainHistoryMmrRootHash},
    block_info::BlockInfo,
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

use crate::{
    service::read::AddressUtxos, ContextuallyVerifiedBlock, NonFinalizedState, TransactionLocation,
    WatchReceiver, MAX_BLOCK_REORG_HEIGHT,
};

#[cfg(test)]
mod tests;

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

    /// The response to a `AwaitUtxo` request, from any non-finalized chains, finalized chain,
    /// pending unverified blocks, or blocks received after the request was sent.
    Utxo(transparent::Utxo),

    /// Response to [`Request::KnownBlock`].
    KnownBlock(Option<KnownBlock>),

    /// Response to [`Request::Read`], answered concurrently by the
    /// [`ReadStateService`](crate::service::ReadStateService).
    Read(ReadResponse),
}

impl From<ReadResponse> for Response {
    fn from(read_response: ReadResponse) -> Self {
        Self::Read(read_response)
    }
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

    /// The best-chain tip hash captured in the same state snapshot used to
    /// compute `confirmations`.
    ///
    /// Callers that combine this response with other state queries should
    /// pin those follow-up queries to this hash (or to the resolved block
    /// hash for the transaction) rather than issuing a separate `Tip` /
    /// `BestChainBlockHash` request, which would re-sample the chain and
    /// can race with reorgs or new blocks. See issue #10550.
    pub best_chain_tip_hash: block::Hash,
}

impl MinedTx {
    /// Creates a new [`MinedTx`]
    pub fn new(
        tx: Arc<Transaction>,
        height: block::Height,
        confirmations: u32,
        block_time: DateTime<Utc>,
        best_chain_tip_hash: block::Hash,
    ) -> Self {
        Self {
            tx,
            height,
            confirmations,
            block_time,
            best_chain_tip_hash,
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
/// If the buffer does fill, sends apply backpressure (the sender awaits a free slot) rather than
/// dropping blocks, so the listener still receives every block once the consumer catches up.
// `MAX_BLOCK_REORG_HEIGHT` is a small `u32` constant (the reorg limit), so widening it to `usize`
// and doubling it cannot overflow on any supported platform.
const NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE: usize = 2 * MAX_BLOCK_REORG_HEIGHT as usize;

/// A listener for changes in the non-finalized state.
#[derive(Clone, Debug)]
pub struct NonFinalizedBlocksListener(
    pub Arc<tokio::sync::mpsc::Receiver<(zebra_chain::block::Hash, Arc<zebra_chain::block::Block>)>>,
);

impl NonFinalizedBlocksListener {
    /// Sends the blocks in `non_finalized_state` that satisfy `take_cond` to `sender`, in
    /// ascending height order.
    ///
    /// Walks each chain from its tip downwards, taking blocks while `take_cond` holds and stopping
    /// at the first block that fails it, so it sends the blocks a listener hasn't been sent yet by
    /// stopping at the first block it already has.
    ///
    /// Blocks below a fork point belong to every chain that forked there, so each block is only
    /// sent once, as part of the highest-work chain that contains it.
    ///
    /// Returns an error if the receiver has been dropped.
    async fn take_and_send_blocks<'a>(
        sender: &tokio::sync::mpsc::Sender<(block::Hash, Arc<Block>)>,
        non_finalized_state: &'a NonFinalizedState,
        take_cond: impl Fn(&&ContextuallyVerifiedBlock) -> bool + Copy + 'a,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<(block::Hash, Arc<Block>)>> {
        let new_blocks = non_finalized_state.chain_iter().flat_map(move |chain| {
            // Take blocks from the chain in reverse height order until we reach a block the
            // listener already has, then restore ascending height order.
            let mut blocks: Vec<_> = chain.blocks.values().rev().take_while(take_cond).collect();
            blocks.reverse();
            blocks
        });

        // Chains share every block below their fork point, so without this the shared blocks would
        // be sent once per chain: up to `MAX_NON_FINALIZED_CHAIN_FORKS` copies of a chain that can
        // be `MAX_BLOCK_REORG_HEIGHT` blocks long, which overflows the channel buffer and makes the
        // listener wait on the consumer during an ordinary initial send. `chain_iter()` yields the
        // highest-work chain first, so a fork's remaining blocks still follow the shared ancestors
        // they build on.
        let mut sent_hashes = HashSet::new();

        for cv_block in new_blocks {
            if !sent_hashes.insert(cv_block.hash) {
                continue;
            }

            sender.send((cv_block.hash, cv_block.block.clone())).await?;
        }

        Ok(())
    }

    /// Spawns a task to listen for changes in the non-finalized state and sends any blocks in the non-finalized state
    /// to the caller that have not already been sent.
    ///
    /// `known_chain_tips` holds the hashes of chain tips the caller already has. On the first send
    /// only, each non-finalized chain is walked from its tip downwards and blocks are sent until a
    /// hash in this set is reached, so any block at or below a known tip on the same chain is
    /// skipped. If it is empty, every block currently in the non-finalized state is sent. After the
    /// first send, those blocks are already tracked as sent, so later sends only forward blocks that
    /// weren't in the previously seen non-finalized state.
    ///
    /// Returns a new instance of [`NonFinalizedBlocksListener`] for the caller to listen for new blocks in the non-finalized state.
    pub fn spawn(
        mut non_finalized_state_receiver: WatchReceiver<NonFinalizedState>,
        known_chain_tips: HashSet<block::Hash>,
    ) -> Self {
        let (sender, receiver) = tokio::sync::mpsc::channel(NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE);

        tokio::spawn(async move {
            // `prev_non_finalized_state` starts as the current non-finalized state. The first send
            // below skips blocks at or below the caller's known chain tips; afterwards those blocks
            // are already in `prev_non_finalized_state`, so later sends only need to check it.
            let mut prev_non_finalized_state = non_finalized_state_receiver.cloned_watch_data();

            // Send the blocks the caller is missing relative to its known chain tips. This checks
            // `known_chain_tips` once; from here on those blocks are covered by the state check.
            if Self::take_and_send_blocks(&sender, &prev_non_finalized_state, |b| {
                !known_chain_tips.contains(&b.hash)
            })
            .await
            .is_err()
            {
                tracing::debug!("non-finalized blocks receiver closed, ending task");
                return;
            }

            // # Correctness
            //
            // This loop should check that the non-finalized state receiver has changed sooner
            // than the non-finalized state could possibly have changed to avoid missing updates, so
            // the logic here should be quicker than the contextual verification logic that precedes
            // commits to the non-finalized state.
            //
            // See the `NON_FINALIZED_STATE_CHANGE_BUFFER_SIZE` documentation for more details.
            loop {
                let non_finalized_state = non_finalized_state_receiver.cloned_watch_data();

                // Send blocks that weren't in the last seen copy of the non-finalized state. The
                // caller's known tips are already covered by `prev_non_finalized_state`.
                if Self::take_and_send_blocks(&sender, &non_finalized_state, |b| {
                    !prev_non_finalized_state.any_chain_contains(&b.hash)
                })
                .await
                .is_err()
                {
                    tracing::debug!("non-finalized blocks receiver closed, ending task");
                    return;
                }

                prev_non_finalized_state = non_finalized_state;

                // Wait for the next update to the non-finalized state.
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

    /// Response to [`ReadRequest::BlockQuery`] with the requested block fields,
    /// or `None` if the block was not found.
    BlockQuery(Option<QueriedBlock>),

    /// Response to [`ReadRequest::TransactionQuery`] with the transaction and its
    /// location information, or `None` if it was not found.
    TransactionQuery(Option<AnyTx>),

    /// Response to [`ReadRequest::UtxoQuery`] with the UTXO, or `None` if it was
    /// not found. See [`UtxoQuery`](crate::request::UtxoQuery) for the exact
    /// semantics of each chain selector.
    UtxoQuery(Option<transparent::Utxo>),

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

    /// The response to a `FindForkPoint` request.
    /// Returns the height and hash of the fork point, or `None` if no locator entry is
    /// on the best chain.
    ForkPoint(Option<(block::Height, block::Hash)>),

    /// Response to [`ReadRequest::NoteCommitmentSubtrees`] with the subtrees of the
    /// requested tree kind.
    NoteCommitmentSubtrees(NoteCommitmentSubtrees),

    /// Response to [`ReadRequest::AddressQuery`] with the requested address fields.
    AddressQuery(QueriedAddresses),

    /// Response to [`ReadRequest::CheckBestChainTipNullifiersAndAnchors`].
    ///
    /// Does not check transparent UTXO inputs
    ValidBestChainTipNullifiersAndAnchors,

    /// Response to [`ReadRequest::BestChainNextMedianTimePast`].
    /// Contains the median-time-past for the *next* block on the best chain.
    BestChainNextMedianTimePast(DateTime32),

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

    /// Response to [`ReadRequest::IsTransparentOutputSpent`]
    IsTransparentOutputSpent(bool),
}

/// Selected fields of a block in the best chain, returned in response to a
/// [`ReadRequest::BlockQuery`].
///
/// Exactly the fields requested in the query's [`BlockField`](crate::request::BlockField)
/// set are `Some`; every other field is `None`. The field granularity matches
/// Zebra's database structure: fields that are stored separately from the full
/// transaction data are read without deserializing the whole block.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueriedBlock {
    /// The hash of the block, if [`BlockField::Hash`](crate::request::BlockField::Hash)
    /// was requested.
    pub hash: Option<block::Hash>,

    /// The height of the block, if [`BlockField::Height`](crate::request::BlockField::Height)
    /// was requested.
    pub height: Option<block::Height>,

    /// The header of the block, if [`BlockField::Header`](crate::request::BlockField::Header)
    /// was requested.
    pub header: Option<Arc<block::Header>>,

    /// The hash of the next block on the best chain, if
    /// [`BlockField::NextBlockHash`](crate::request::BlockField::NextBlockHash) was requested.
    /// `None` if the queried block is the best chain tip.
    pub next_block_hash: Option<block::Hash>,

    /// The number of confirmations of the block, if
    /// [`BlockField::Confirmations`](crate::request::BlockField::Confirmations) was requested.
    pub confirmations: Option<u32>,

    /// The hashes of the transactions in the block, in block order, if
    /// [`BlockField::TransactionIds`](crate::request::BlockField::TransactionIds) was requested.
    pub transaction_ids: Option<Arc<[transaction::Hash]>>,

    /// The [`BlockInfo`] after the block, if
    /// [`BlockField::BlockInfo`](crate::request::BlockField::BlockInfo) was requested.
    pub block_info: Option<BlockInfo>,

    /// The Sapling note commitment tree as of the block, if
    /// [`BlockField::SaplingTree`](crate::request::BlockField::SaplingTree) was requested.
    pub sapling_tree: Option<Arc<sapling::tree::NoteCommitmentTree>>,

    /// The Orchard note commitment tree as of the block, if
    /// [`BlockField::OrchardTree`](crate::request::BlockField::OrchardTree) was requested.
    pub orchard_tree: Option<Arc<orchard::tree::NoteCommitmentTree>>,

    /// The Ironwood note commitment tree as of the block, if
    /// [`BlockField::IronwoodTree`](crate::request::BlockField::IronwoodTree) was requested.
    /// Ironwood reuses the Orchard note type.
    pub ironwood_tree: Option<Arc<orchard::tree::NoteCommitmentTree>>,

    /// The full block with all its transactions, if
    /// [`BlockField::Block`](crate::request::BlockField::Block) was requested.
    pub block: Option<Arc<Block>>,

    /// The block's authorizing data Merkle root, if
    /// [`BlockField::AuthDataRoot`](crate::request::BlockField::AuthDataRoot) was requested.
    pub auth_data_root: Option<block::merkle::AuthDataRoot>,

    /// Whether the block is on the best chain. Always `Some` when the query used
    /// [`ChainSelector::Any`](crate::request::ChainSelector::Any), and `None`
    /// otherwise. Blocks in the finalized state are always on the best chain.
    pub in_best_chain: Option<bool>,
}

/// Note commitment subtrees of a single tree kind, returned in response to a
/// [`ReadRequest::NoteCommitmentSubtrees`].
///
/// Ironwood reuses the Orchard note type, so Ironwood subtrees are returned in
/// the `Orchard` variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoteCommitmentSubtrees {
    /// Sapling note commitment subtrees.
    Sapling(BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<sapling_crypto::Node>>),

    /// Orchard or Ironwood note commitment subtrees. Ironwood reuses the Orchard
    /// note type.
    Orchard(BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<orchard::tree::Node>>),
}

/// Selected data about a set of transparent addresses, returned in response to a
/// [`ReadRequest::AddressQuery`].
///
/// Exactly the fields requested in the query's [`AddressField`](crate::request::AddressField)
/// set are `Some`; every other field is `None`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QueriedAddresses {
    /// The total balance of the addresses, if
    /// [`AddressField::Balance`](crate::request::AddressField::Balance) was requested.
    pub balance: Option<Amount<NonNegative>>,

    /// The total received funds in zatoshis, including change, if
    /// [`AddressField::Balance`](crate::request::AddressField::Balance) was requested.
    pub received: Option<u64>,

    /// The hashes of transactions sent or received by the addresses, in the order
    /// they appear in blocks, if
    /// [`AddressField::TransactionIds`](crate::request::AddressField::TransactionIds)
    /// was requested.
    pub transaction_ids: Option<BTreeMap<TransactionLocation, transaction::Hash>>,

    /// The UTXOs of the addresses, with their transaction data, if
    /// [`AddressField::Utxos`](crate::request::AddressField::Utxos) was requested.
    pub utxos: Option<AddressUtxos>,
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
