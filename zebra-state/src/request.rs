//! State [`tower::Service`] request types.

use std::{
    collections::{HashMap, HashSet},
    ops::{Add, Deref, DerefMut, RangeInclusive},
    sync::Arc,
};

use tower::{BoxError, Service, ServiceExt};
use zebra_chain::{
    amount::{Amount, DeferredPoolBalanceChange, NegativeAllowed, NonNegative},
    block::{self, Block, HeightDiff},
    history_tree::HistoryTree,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    sapling,
    serialization::SerializationError,
    sprout,
    subtree::{NoteCommitmentSubtree, NoteCommitmentSubtreeIndex},
    transaction::{self, UnminedTx},
    transparent::{self, utxos_from_ordered_utxos},
    value_balance::{ValueBalance, ValueBalanceError},
};

/// Allow *only* these unused imports, so that rustdoc link resolution
/// will work with inline links.
#[allow(unused_imports)]
use crate::{
    constants::{MAX_FIND_BLOCK_HASHES_RESULTS, MAX_FIND_BLOCK_HEADERS_RESULTS},
    ReadResponse, Response,
};
use crate::{
    error::{CommitCheckpointVerifiedError, InvalidateError, LayeredStateError, ReconsiderError},
    CommitSemanticallyVerifiedError,
};

/// Identify a spend by a transparent outpoint or revealed nullifier.
///
/// This enum implements `From` for [`transparent::OutPoint`], [`sprout::Nullifier`],
/// [`sapling::Nullifier`], and [`orchard::Nullifier`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg(feature = "indexer")]
pub enum Spend {
    /// A spend identified by a [`transparent::OutPoint`].
    OutPoint(transparent::OutPoint),
    /// A spend identified by a [`sprout::Nullifier`].
    Sprout(sprout::Nullifier),
    /// A spend identified by a [`sapling::Nullifier`].
    Sapling(sapling::Nullifier),
    /// A spend identified by a [`orchard::Nullifier`].
    Orchard(orchard::Nullifier),
}

#[cfg(feature = "indexer")]
impl From<transparent::OutPoint> for Spend {
    fn from(outpoint: transparent::OutPoint) -> Self {
        Self::OutPoint(outpoint)
    }
}

#[cfg(feature = "indexer")]
impl From<sprout::Nullifier> for Spend {
    fn from(sprout_nullifier: sprout::Nullifier) -> Self {
        Self::Sprout(sprout_nullifier)
    }
}

#[cfg(feature = "indexer")]
impl From<sapling::Nullifier> for Spend {
    fn from(sapling_nullifier: sapling::Nullifier) -> Self {
        Self::Sapling(sapling_nullifier)
    }
}

#[cfg(feature = "indexer")]
impl From<orchard::Nullifier> for Spend {
    fn from(orchard_nullifier: orchard::Nullifier) -> Self {
        Self::Orchard(orchard_nullifier)
    }
}

/// Identify a block by hash or height.
///
/// This enum implements `From` for [`block::Hash`] and [`block::Height`],
/// so it can be created using `hash.into()` or `height.into()`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HashOrHeight {
    /// A block identified by hash.
    Hash(block::Hash),
    /// A block identified by height.
    Height(block::Height),
}

impl HashOrHeight {
    /// Unwrap the inner height or attempt to retrieve the height for a given
    /// hash if one exists.
    pub fn height_or_else<F>(self, op: F) -> Option<block::Height>
    where
        F: FnOnce(block::Hash) -> Option<block::Height>,
    {
        match self {
            HashOrHeight::Hash(hash) => op(hash),
            HashOrHeight::Height(height) => Some(height),
        }
    }

    /// Unwrap the inner hash or attempt to retrieve the hash for a given
    /// height if one exists.
    ///
    /// # Consensus
    ///
    /// In the non-finalized state, a height can have multiple valid hashes.
    /// We typically use the hash that is currently on the best chain.
    pub fn hash_or_else<F>(self, op: F) -> Option<block::Hash>
    where
        F: FnOnce(block::Height) -> Option<block::Hash>,
    {
        match self {
            HashOrHeight::Hash(hash) => Some(hash),
            HashOrHeight::Height(height) => op(height),
        }
    }

    /// Returns the hash if this is a [`HashOrHeight::Hash`].
    pub fn hash(&self) -> Option<block::Hash> {
        if let HashOrHeight::Hash(hash) = self {
            Some(*hash)
        } else {
            None
        }
    }

    /// Returns the height if this is a [`HashOrHeight::Height`].
    pub fn height(&self) -> Option<block::Height> {
        if let HashOrHeight::Height(height) = self {
            Some(*height)
        } else {
            None
        }
    }

    /// Constructs a new [`HashOrHeight`] from a string containing a hash or a positive or negative
    /// height.
    ///
    /// When the provided `hash_or_height` contains a negative height, the `tip_height` parameter
    /// needs to be `Some` since height `-1` points to the tip.
    pub fn new(hash_or_height: &str, tip_height: Option<block::Height>) -> Result<Self, String> {
        hash_or_height
            .parse()
            .map(Self::Hash)
            .or_else(|_| hash_or_height.parse().map(Self::Height))
            .or_else(|_| {
                hash_or_height
                    .parse()
                    .map_err(|_| "could not parse negative height")
                    .and_then(|d: HeightDiff| {
                        if d.is_negative() {
                            {
                                Ok(HashOrHeight::Height(
                                    tip_height
                                        .ok_or("missing tip height")?
                                        .add(d)
                                        .ok_or("underflow when adding negative height to tip")?
                                        .next()
                                        .map_err(|_| "height -1 needs to point to tip")?,
                                ))
                            }
                        } else {
                            Err("height was not negative")
                        }
                    })
            })
            .map_err(|_| {
                "parse error: could not convert the input string to a hash or height".to_string()
            })
    }
}

impl std::fmt::Display for HashOrHeight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HashOrHeight::Hash(hash) => write!(f, "{hash}"),
            HashOrHeight::Height(height) => write!(f, "{}", height.0),
        }
    }
}

impl From<block::Hash> for HashOrHeight {
    fn from(hash: block::Hash) -> Self {
        Self::Hash(hash)
    }
}

impl From<block::Height> for HashOrHeight {
    fn from(height: block::Height) -> Self {
        Self::Height(height)
    }
}

impl From<(block::Height, block::Hash)> for HashOrHeight {
    fn from((_height, hash): (block::Height, block::Hash)) -> Self {
        // Hash is more specific than height for the non-finalized state
        hash.into()
    }
}

impl From<(block::Hash, block::Height)> for HashOrHeight {
    fn from((hash, _height): (block::Hash, block::Height)) -> Self {
        hash.into()
    }
}

impl std::str::FromStr for HashOrHeight {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse()
            .map(Self::Hash)
            .or_else(|_| s.parse().map(Self::Height))
            .map_err(|_| {
                SerializationError::Parse("could not convert the input string to a hash or height")
            })
    }
}

/// A block which has undergone semantic validation and has been prepared for
/// contextual validation.
///
/// It is the constructor's responsibility to perform semantic validation and to
/// ensure that all fields are consistent.
///
/// This structure contains data from contextual validation, which is computed in
/// the *service caller*'s task, not inside the service call itself. This allows
/// moving work out of the single-threaded state service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticallyVerifiedBlock {
    /// The block to commit to the state.
    pub block: Arc<Block>,
    /// The hash of the block.
    pub hash: block::Hash,
    /// The height of the block.
    pub height: block::Height,
    /// New transparent outputs created in this block, indexed by
    /// [`OutPoint`](transparent::OutPoint).
    ///
    /// Each output is tagged with its transaction index in the block.
    /// (The outputs of earlier transactions in a block can be spent by later
    /// transactions.)
    ///
    /// Note: although these transparent outputs are newly created, they may not
    /// be unspent, since a later transaction in a block can spend outputs of an
    /// earlier transaction.
    ///
    /// This field can also contain unrelated outputs, which are ignored.
    pub new_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// A precomputed list of the hashes of the transactions in this block,
    /// in the same order as `block.transactions`.
    pub transaction_hashes: Arc<[transaction::Hash]>,
    /// This block's miner fees
    pub block_miner_fees: Option<Amount<NonNegative>>,
}

/// A block ready to be committed directly to the finalized state with
/// a small number of checks if compared with a `ContextuallyVerifiedBlock`.
///
/// This is exposed for use in checkpointing.
///
/// Note: The difference between a `CheckpointVerifiedBlock` and a `ContextuallyVerifiedBlock` is
/// that the `CheckpointVerifier` doesn't bind the transaction authorizing data to the
/// `ChainHistoryBlockTxAuthCommitmentHash`, but the `NonFinalizedState` and `FinalizedState` do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointVerifiedBlock {
    /// The semantically verified block.
    pub(crate) block: SemanticallyVerifiedBlock,
    /// This block's deferred pool value balance change.
    pub(crate) deferred_pool_balance_change: Option<DeferredPoolBalanceChange>,
}

// Some fields are pub(crate), so we can add whatever db-format-dependent
// precomputation we want here without leaking internal details.

/// A contextually verified block, ready to be committed directly to the finalized state with no
/// checks, if it becomes the root of the best non-finalized chain.
///
/// Used by the state service and non-finalized `Chain`.
///
/// Note: The difference between a `CheckpointVerifiedBlock` and a `ContextuallyVerifiedBlock` is
/// that the `CheckpointVerifier` doesn't bind the transaction authorizing data to the
/// `ChainHistoryBlockTxAuthCommitmentHash`, but the `NonFinalizedState` and `FinalizedState` do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextuallyVerifiedBlock {
    /// The block to commit to the state.
    pub(crate) block: Arc<Block>,

    /// The hash of the block.
    pub(crate) hash: block::Hash,

    /// The height of the block.
    pub(crate) height: block::Height,

    /// New transparent outputs created in this block, indexed by
    /// [`OutPoint`](transparent::OutPoint).
    ///
    /// Note: although these transparent outputs are newly created, they may not
    /// be unspent, since a later transaction in a block can spend outputs of an
    /// earlier transaction.
    ///
    /// This field can also contain unrelated outputs, which are ignored.
    pub(crate) new_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,

    /// The outputs spent by this block, indexed by the [`transparent::Input`]'s
    /// [`OutPoint`](transparent::OutPoint).
    ///
    /// Note: these inputs can come from earlier transactions in this block,
    /// or earlier blocks in the chain.
    ///
    /// This field can also contain unrelated outputs, which are ignored.
    pub(crate) spent_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,

    /// A precomputed list of the hashes of the transactions in this block,
    /// in the same order as `block.transactions`.
    pub(crate) transaction_hashes: Arc<[transaction::Hash]>,

    /// The sum of the chain value pool changes of all transactions in this block.
    pub(crate) chain_value_pool_change: ValueBalance<NegativeAllowed>,
}

/// Wraps note commitment trees and the history tree together.
///
/// The default instance represents the treestate that corresponds to the genesis block.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Treestate {
    /// Note commitment trees.
    pub note_commitment_trees: NoteCommitmentTrees,
    /// History tree.
    pub history_tree: Arc<HistoryTree>,
}

impl Treestate {
    #[allow(missing_docs)]
    pub(crate) fn new(
        sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sapling: Arc<sapling::tree::NoteCommitmentTree>,
        orchard: Arc<orchard::tree::NoteCommitmentTree>,
        sapling_subtree: Option<NoteCommitmentSubtree<sapling_crypto::Node>>,
        orchard_subtree: Option<NoteCommitmentSubtree<orchard::tree::Node>>,
        history_tree: Arc<HistoryTree>,
    ) -> Self {
        Self {
            note_commitment_trees: NoteCommitmentTrees {
                sprout,
                sapling,
                sapling_subtree,
                orchard,
                orchard_subtree,
            },
            history_tree,
        }
    }
}

/// Contains a block ready to be committed.
///
/// Zebra's state service passes this `enum` over to the finalized state
/// when committing a block.
#[allow(missing_docs)]
pub enum FinalizableBlock {
    Checkpoint {
        checkpoint_verified: CheckpointVerifiedBlock,
    },
    Contextual {
        contextually_verified: ContextuallyVerifiedBlock,
        treestate: Treestate,
    },
}

/// Contains a block with all its associated data that the finalized state can commit to its
/// database.
///
/// Note that it's the constructor's responsibility to ensure that all data is valid and verified.
pub struct FinalizedBlock {
    /// The block to commit to the state.
    pub(super) block: Arc<Block>,
    /// The hash of the block.
    pub(super) hash: block::Hash,
    /// The height of the block.
    pub(super) height: block::Height,
    /// New transparent outputs created in this block, indexed by
    /// [`OutPoint`](transparent::OutPoint).
    pub(super) new_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// A precomputed list of the hashes of the transactions in this block, in the same order as
    /// `block.transactions`.
    pub(super) transaction_hashes: Arc<[transaction::Hash]>,
    /// The tresstate associated with the block.
    pub(super) treestate: Treestate,
    /// This block's deferred pool value balance change.
    pub(super) deferred_pool_balance_change: Option<DeferredPoolBalanceChange>,
}

impl FinalizedBlock {
    /// Constructs [`FinalizedBlock`] from [`CheckpointVerifiedBlock`] and its [`Treestate`].
    pub fn from_checkpoint_verified(block: CheckpointVerifiedBlock, treestate: Treestate) -> Self {
        Self::from_semantically_verified(block.block, treestate, block.deferred_pool_balance_change)
    }

    /// Constructs [`FinalizedBlock`] from [`ContextuallyVerifiedBlock`] and its [`Treestate`].
    pub fn from_contextually_verified(
        block: ContextuallyVerifiedBlock,
        treestate: Treestate,
    ) -> Self {
        Self::from_semantically_verified(
            SemanticallyVerifiedBlock::from(block.clone()),
            treestate,
            Some(DeferredPoolBalanceChange::new(
                block.chain_value_pool_change.deferred_amount(),
            )),
        )
    }

    /// Constructs [`FinalizedBlock`] from [`SemanticallyVerifiedBlock`] and its [`Treestate`].
    fn from_semantically_verified(
        block: SemanticallyVerifiedBlock,
        treestate: Treestate,
        deferred_pool_balance_change: Option<DeferredPoolBalanceChange>,
    ) -> Self {
        Self {
            block: block.block,
            hash: block.hash,
            height: block.height,
            new_outputs: block.new_outputs,
            transaction_hashes: block.transaction_hashes,
            treestate,
            deferred_pool_balance_change,
        }
    }
}

impl FinalizableBlock {
    /// Create a new [`FinalizableBlock`] given a [`ContextuallyVerifiedBlock`].
    pub fn new(contextually_verified: ContextuallyVerifiedBlock, treestate: Treestate) -> Self {
        Self::Contextual {
            contextually_verified,
            treestate,
        }
    }

    #[cfg(test)]
    /// Extract a [`Block`] from a [`FinalizableBlock`] variant.
    pub fn inner_block(&self) -> Arc<Block> {
        match self {
            FinalizableBlock::Checkpoint {
                checkpoint_verified,
            } => checkpoint_verified.block.block.clone(),
            FinalizableBlock::Contextual {
                contextually_verified,
                ..
            } => contextually_verified.block.clone(),
        }
    }
}

impl From<CheckpointVerifiedBlock> for FinalizableBlock {
    fn from(checkpoint_verified: CheckpointVerifiedBlock) -> Self {
        Self::Checkpoint {
            checkpoint_verified,
        }
    }
}

impl From<Arc<Block>> for FinalizableBlock {
    fn from(block: Arc<Block>) -> Self {
        Self::from(CheckpointVerifiedBlock::from(block))
    }
}

impl From<&SemanticallyVerifiedBlock> for SemanticallyVerifiedBlock {
    fn from(semantically_verified: &SemanticallyVerifiedBlock) -> Self {
        semantically_verified.clone()
    }
}

// Doing precomputation in these impls means that it will be done in
// the *service caller*'s task, not inside the service call itself.
// This allows moving work out of the single-threaded state service.

impl ContextuallyVerifiedBlock {
    /// Create a block that's ready for non-finalized `Chain` contextual validation,
    /// using a [`SemanticallyVerifiedBlock`] and the UTXOs it spends.
    ///
    /// When combined, `semantically_verified.new_outputs` and `spent_utxos` must contain
    /// the [`Utxo`](transparent::Utxo)s spent by every transparent input in this block,
    /// including UTXOs created by earlier transactions in this block.
    ///
    /// Note: a [`ContextuallyVerifiedBlock`] isn't actually contextually valid until
    /// [`Chain::push()`](crate::service::non_finalized_state::Chain::push) returns success.
    pub fn with_block_and_spent_utxos(
        semantically_verified: SemanticallyVerifiedBlock,
        mut spent_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
        deferred_pool_balance_change: Option<DeferredPoolBalanceChange>,
    ) -> Result<Self, ValueBalanceError> {
        let SemanticallyVerifiedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
            block_miner_fees: _,
        } = semantically_verified;

        // This is redundant for the non-finalized state,
        // but useful to make some tests pass more easily.
        //
        // TODO: fix the tests, and stop adding unrelated outputs.
        spent_outputs.extend(new_outputs.clone());

        Ok(Self {
            block: block.clone(),
            hash,
            height,
            new_outputs,
            spent_outputs: spent_outputs.clone(),
            transaction_hashes,
            chain_value_pool_change: block.chain_value_pool_change(
                &utxos_from_ordered_utxos(spent_outputs),
                deferred_pool_balance_change,
            )?,
        })
    }
}

impl CheckpointVerifiedBlock {
    /// Creates a [`CheckpointVerifiedBlock`] from [`Block`] with optional deferred balance and
    /// optional pre-computed hash.
    pub fn new(
        block: Arc<Block>,
        hash: Option<block::Hash>,
        deferred_pool_balance_change: Option<DeferredPoolBalanceChange>,
    ) -> Self {
        let block =
            SemanticallyVerifiedBlock::with_hash(block.clone(), hash.unwrap_or(block.hash()));
        Self {
            block,
            deferred_pool_balance_change,
        }
    }
    /// Creates a block that's ready to be committed to the finalized state,
    /// using a precalculated [`block::Hash`].
    ///
    /// Note: a [`CheckpointVerifiedBlock`] isn't actually finalized
    /// until [`Request::CommitCheckpointVerifiedBlock`] returns success.
    pub fn with_hash(block: Arc<Block>, hash: block::Hash, deferred_pool_balance_change: Option<DeferredPoolBalanceChange>) -> Self {
        Self {
            block: SemanticallyVerifiedBlock::with_hash(block, hash),
            deferred_pool_balance_change,
        }
    }
}

impl SemanticallyVerifiedBlock {
    /// Creates [`SemanticallyVerifiedBlock`] from [`Block`] and [`block::Hash`].
    pub fn with_hash(block: Arc<Block>, hash: block::Hash) -> Self {
        let height = block
            .coinbase_height()
            .expect("semantically verified block should have a coinbase height");
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs = transparent::new_ordered_outputs(&block, &transaction_hashes);

        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
            block_miner_fees: None,
        }
    }
}

impl From<Arc<Block>> for CheckpointVerifiedBlock {
    fn from(block: Arc<Block>) -> Self {
        CheckpointVerifiedBlock::with_hash(block.clone(), block.hash(), None)
    }
}

impl From<Arc<Block>> for SemanticallyVerifiedBlock {
    fn from(block: Arc<Block>) -> Self {
        let hash = block.hash();
        let height = block
            .coinbase_height()
            .expect("semantically verified block should have a coinbase height");
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs = transparent::new_ordered_outputs(&block, &transaction_hashes);

        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
            block_miner_fees: None,
        }
    }
}

impl From<ContextuallyVerifiedBlock> for SemanticallyVerifiedBlock {
    fn from(valid: ContextuallyVerifiedBlock) -> Self {
        Self {
            block: valid.block,
            hash: valid.hash,
            height: valid.height,
            new_outputs: valid.new_outputs,
            transaction_hashes: valid.transaction_hashes,
            block_miner_fees: None,
        }
    }
}

impl From<FinalizedBlock> for SemanticallyVerifiedBlock {
    fn from(finalized: FinalizedBlock) -> Self {
        Self {
            block: finalized.block,
            hash: finalized.hash,
            height: finalized.height,
            new_outputs: finalized.new_outputs,
            transaction_hashes: finalized.transaction_hashes,
            block_miner_fees: None,
        }
    }
}

impl From<CheckpointVerifiedBlock> for SemanticallyVerifiedBlock {
    fn from(checkpoint_verified: CheckpointVerifiedBlock) -> Self {
        checkpoint_verified.block
    }
}

impl Deref for CheckpointVerifiedBlock {
    type Target = SemanticallyVerifiedBlock;

    fn deref(&self) -> &Self::Target {
        &self.block
    }
}
impl DerefMut for CheckpointVerifiedBlock {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.block
    }
}

/// Helper trait for convenient access to expected response and error types.
pub trait MappedRequest: Sized + Send + 'static {
    /// Expected response type for this state request.
    type MappedResponse;
    /// Expected error type for this state request.
    type Error: std::error::Error + std::fmt::Display + 'static;

    /// Maps the request type to a [`Request`].
    fn map_request(self) -> Request;

    /// Maps the expected [`Response`] variant for this request to the mapped response type.
    fn map_response(response: Response) -> Self::MappedResponse;

    /// Accepts a state service to call, maps this request to a [`Request`], waits for the state to be ready,
    /// calls the state with the mapped request, then maps the success or error response to the expected response
    /// or error type for this request.
    ///
    /// Returns a [`Result<MappedResponse, LayeredServicesError<RequestError>>`].
    #[allow(async_fn_in_trait)]
    async fn mapped_oneshot<State>(
        self,
        state: &mut State,
    ) -> Result<Self::MappedResponse, LayeredStateError<Self::Error>>
    where
        State: Service<Request, Response = Response, Error = BoxError>,
        State::Future: Send,
    {
        let response = state.ready().await?.call(self.map_request()).await?;
        Ok(Self::map_response(response))
    }
}

/// Performs contextual validation of the given semantically verified block,
/// committing it to the state if successful.
///
/// See the [`crate`] documentation and [`Request::CommitSemanticallyVerifiedBlock`] for details.
pub struct CommitSemanticallyVerifiedBlockRequest(pub SemanticallyVerifiedBlock);

impl MappedRequest for CommitSemanticallyVerifiedBlockRequest {
    type MappedResponse = block::Hash;
    type Error = CommitSemanticallyVerifiedError;

    fn map_request(self) -> Request {
        Request::CommitSemanticallyVerifiedBlock(self.0)
    }

    fn map_response(response: Response) -> Self::MappedResponse {
        match response {
            Response::Committed(hash) => hash,
            _ => unreachable!("wrong response variant for request"),
        }
    }
}

/// Commit a checkpointed block to the state
///
/// See the [`crate`] documentation and [`Request::CommitCheckpointVerifiedBlock`] for details.
#[allow(dead_code)]
pub struct CommitCheckpointVerifiedBlockRequest(pub CheckpointVerifiedBlock);

impl MappedRequest for CommitCheckpointVerifiedBlockRequest {
    type MappedResponse = block::Hash;
    type Error = CommitCheckpointVerifiedError;

    fn map_request(self) -> Request {
        Request::CommitCheckpointVerifiedBlock(self.0)
    }

    fn map_response(response: Response) -> Self::MappedResponse {
        match response {
            Response::Committed(hash) => hash,
            _ => unreachable!("wrong response variant for request"),
        }
    }
}

/// Request to invalidate a block in the state.
///
/// See the [`crate`] documentation and [`Request::InvalidateBlock`] for details.
#[allow(dead_code)]
pub struct InvalidateBlockRequest(pub block::Hash);

impl MappedRequest for InvalidateBlockRequest {
    type MappedResponse = block::Hash;
    type Error = InvalidateError;

    fn map_request(self) -> Request {
        Request::InvalidateBlock(self.0)
    }

    fn map_response(response: Response) -> Self::MappedResponse {
        match response {
            Response::Invalidated(hash) => hash,
            _ => unreachable!("wrong response variant for request"),
        }
    }
}

/// Request to reconsider a previously invalidated block and re-commit it to the state.
///
/// See the [`crate`] documentation and [`Request::ReconsiderBlock`] for details.
#[allow(dead_code)]
pub struct ReconsiderBlockRequest(pub block::Hash);

impl MappedRequest for ReconsiderBlockRequest {
    type MappedResponse = Vec<block::Hash>;
    type Error = ReconsiderError;

    fn map_request(self) -> Request {
        Request::ReconsiderBlock(self.0)
    }

    fn map_response(response: Response) -> Self::MappedResponse {
        match response {
            Response::Reconsidered(hashes) => hashes,
            _ => unreachable!("wrong response variant for request"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A query about or modification to the chain state, via the
/// [`StateService`](crate::service::StateService).
pub enum Request {
    /// Performs contextual validation of the given semantically verified block,
    /// committing it to the state if successful.
    ///
    /// This request can be made out-of-order; the state service will queue it
    /// until its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the block when it is
    /// committed to the state, or a [`CommitSemanticallyVerifiedError`][0] if
    /// the block fails contextual validation or otherwise could not be committed.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed. A
    /// request to commit a block which has been queued internally but not yet
    /// committed will fail the older request and replace it with the newer request.
    ///
    /// # Correctness
    ///
    /// Block commit requests should be wrapped in a timeout, so that
    /// out-of-order and invalid requests do not hang indefinitely. See the [`crate`]
    /// documentation for details.
    ///
    /// [0]: (crate::error::CommitSemanticallyVerifiedError)
    CommitSemanticallyVerifiedBlock(SemanticallyVerifiedBlock),

    /// Commit a checkpointed block to the state, skipping most but not all
    /// contextual validation.
    ///
    /// This is exposed for use in checkpointing, which produces checkpoint vefified
    /// blocks. This request can be made out-of-order; the state service will queue
    /// it until its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly committed
    /// block, or a [`CommitCheckpointVerifiedError`][0] if the block could not be
    /// committed to the state.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed.
    /// Duplicate requests will replace the older duplicate, and return an error
    /// in its response future.
    ///
    /// # Note
    ///
    /// [`SemanticallyVerifiedBlock`], [`ContextuallyVerifiedBlock`] and
    /// [`CheckpointVerifiedBlock`] are an internal Zebra implementation detail.
    /// There is no difference between these blocks on the Zcash network, or in Zebra's
    /// network or syncer implementations.
    ///
    /// # Consensus
    ///
    /// Checkpointing is allowed under the Zcash "social consensus" rules.
    /// Zebra checkpoints both settled network upgrades, and blocks past the rollback limit.
    /// (By the time Zebra release is tagged, its final checkpoint is typically hours or days old.)
    ///
    /// > A network upgrade is settled on a given network when there is a social consensus
    /// > that it has activated with a given activation block hash. A full validator that
    /// > potentially risks Mainnet funds or displays Mainnet transaction information to a user
    /// > MUST do so only for a block chain that includes the activation block of the most
    /// > recent settled network upgrade, with the corresponding activation block hash.
    /// > ...
    /// > A full validator MAY impose a limit on the number of blocks it will “roll back”
    /// > when switching from one best valid block chain to another that is not a descendent.
    /// > For `zcashd` and `zebra` this limit is 100 blocks.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#blockchain>
    ///
    /// # Correctness
    ///
    /// Block commit requests should be wrapped in a timeout, so that
    /// out-of-order and invalid requests do not hang indefinitely. See the [`crate`]
    /// documentation for details.
    ///
    /// [0]: (crate::error::CommitCheckpointVerifiedError)
    CommitCheckpointVerifiedBlock(CheckpointVerifiedBlock),

    /// Computes the depth in the current best chain of the block identified by the given hash.
    ///
    /// Returns
    ///
    /// * [`Response::Depth(Some(depth))`](Response::Depth) if the block is in the best chain;
    /// * [`Response::Depth(None)`](Response::Depth) otherwise.
    Depth(block::Hash),

    /// Returns [`Response::Tip(Option<(Height, block::Hash)>)`](Response::Tip)
    /// with the current best chain tip.
    Tip,

    #[cfg(zcash_unstable = "zip234")]
    TipPoolValues,

    /// Computes a block locator object based on the current best chain.
    ///
    /// Returns [`Response::BlockLocator`] with hashes starting
    /// from the best chain tip, and following the chain of previous
    /// hashes. The first hash is the best chain tip. The last hash is
    /// the tip of the finalized portion of the state. Block locators
    /// are not continuous - some intermediate hashes might be skipped.
    ///
    /// If the state is empty, the block locator is also empty.
    BlockLocator,

    /// Looks up a transaction by hash in the current best chain.
    ///
    /// Returns
    ///
    /// * [`Response::Transaction(Some(Arc<Transaction>))`](Response::Transaction) if the transaction is in the best chain;
    /// * [`Response::Transaction(None)`](Response::Transaction) otherwise.
    Transaction(transaction::Hash),

    /// Looks up a transaction by hash in any chain.
    ///
    /// Returns
    ///
    /// * [`Response::AnyChainTransaction(Some(AnyTx))`](Response::AnyChainTransaction)
    ///   if the transaction is in any chain;
    /// * [`Response::AnyChainTransaction(None)`](Response::AnyChainTransaction)
    ///   otherwise.
    AnyChainTransaction(transaction::Hash),

    /// Looks up a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// returning `None` immediately if it is unknown.
    ///
    /// Checks verified blocks in the finalized chain and the _best_ non-finalized chain.
    UnspentBestChainUtxo(transparent::OutPoint),

    /// Looks up a block by hash or height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`Response::Block(Some(Arc<Block>))`](Response::Block) if the block is in the best chain;
    /// * [`Response::Block(None)`](Response::Block) otherwise.
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    Block(HashOrHeight),

    //// Same as Block, but also returns serialized block size.
    ////
    /// Returns
    ///
    /// * [`ReadResponse::BlockAndSize(Some((Arc<Block>, usize)))`](ReadResponse::BlockAndSize) if the block is in the best chain;
    /// * [`ReadResponse::BlockAndSize(None)`](ReadResponse::BlockAndSize) otherwise.
    BlockAndSize(HashOrHeight),

    /// Looks up a block header by hash or height in the current best chain.
    ///
    /// Returns
    ///
    /// [`Response::BlockHeader(block::Header)`](Response::BlockHeader).
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    BlockHeader(HashOrHeight),

    /// Request a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// waiting until it becomes available if it is unknown.
    ///
    /// Checks the finalized chain, all non-finalized chains, queued unverified blocks,
    /// and any blocks that arrive at the state after the request future has been created.
    ///
    /// This request is purely informational, and there are no guarantees about
    /// whether the UTXO remains unspent or is on the best chain, or any chain.
    /// Its purpose is to allow asynchronous script verification or to wait until
    /// the UTXO arrives in the state before validating dependent transactions.
    ///
    /// # Correctness
    ///
    /// UTXO requests should be wrapped in a timeout, so that
    /// out-of-order and invalid requests do not hang indefinitely. See the [`crate`]
    /// documentation for details.
    ///
    /// Outdated requests are pruned on a regular basis.
    AwaitUtxo(transparent::OutPoint),

    /// Finds the first hash that's in the peer's `known_blocks` and the local best chain.
    /// Returns a list of hashes that follow that intersection, from the best chain.
    ///
    /// If there is no matching hash in the best chain, starts from the genesis hash.
    ///
    /// Stops the list of hashes after:
    ///   * adding the best tip,
    ///   * adding the `stop` hash to the list, if it is in the best chain, or
    ///   * adding 500 hashes to the list.
    ///
    /// Returns an empty list if the state is empty.
    ///
    /// Returns
    ///
    /// [`Response::BlockHashes(Vec<block::Hash>)`](Response::BlockHashes).
    /// See <https://en.bitcoin.it/wiki/Protocol_documentation#getblocks>
    FindBlockHashes {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last block hash to request.
        stop: Option<block::Hash>,
    },

    /// Finds the first hash that's in the peer's `known_blocks` and the local best chain.
    /// Returns a list of headers that follow that intersection, from the best chain.
    ///
    /// If there is no matching hash in the best chain, starts from the genesis header.
    ///
    /// Stops the list of headers after:
    ///   * adding the best tip,
    ///   * adding the header matching the `stop` hash to the list, if it is in the best chain, or
    ///   * adding [`MAX_FIND_BLOCK_HEADERS_RESULTS`] headers to the list.
    ///
    /// Returns an empty list if the state is empty.
    ///
    /// Returns
    ///
    /// [`Response::BlockHeaders(Vec<block::Header>)`](Response::BlockHeaders).
    /// See <https://en.bitcoin.it/wiki/Protocol_documentation#getheaders>
    FindBlockHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the hash of the last header to request.
        stop: Option<block::Hash>,
    },

    /// Contextually validates anchors and nullifiers of a transaction on the best chain
    ///
    /// Returns [`Response::ValidBestChainTipNullifiersAndAnchors`]
    CheckBestChainTipNullifiersAndAnchors(UnminedTx),

    /// Calculates the median-time-past for the *next* block on the best chain.
    ///
    /// Returns [`Response::BestChainNextMedianTimePast`] when successful.
    BestChainNextMedianTimePast,

    /// Looks up a block hash by height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`Response::BlockHash(Some(hash))`](Response::BlockHash) if the block is in the best chain;
    /// * [`Response::BlockHash(None)`](Response::BlockHash) otherwise.
    BestChainBlockHash(block::Height),

    /// Checks if a block is present anywhere in the state service.
    /// Looks up `hash` in block queues as well as the finalized chain and all non-finalized chains.
    ///
    /// Returns [`Response::KnownBlock(Some(Location))`](Response::KnownBlock) if the block is in the best state service.
    /// Returns [`Response::KnownBlock(None)`](Response::KnownBlock) otherwise.
    KnownBlock(block::Hash),

    /// Invalidates a block in the non-finalized state with the provided hash if one is present, removing it and
    /// its child blocks, and rejecting it during contextual validation if it's resubmitted to the state.
    ///
    /// Returns [`Response::Invalidated`] with the hash of the invalidated block,
    /// or a [`InvalidateError`][0] if the block was not found, the state is still
    /// committing checkpointed blocks, or the request could not be processed.
    ///
    /// [0]: (crate::error::InvalidateError)
    InvalidateBlock(block::Hash),

    /// Reconsiders a previously invalidated block in the non-finalized state with the provided hash if one is present.
    ///
    /// Returns [`Response::Reconsidered`] with the hash of the reconsidered block,
    /// or a [`ReconsiderError`][0] if the block was not previously invalidated,
    /// its parent chain is missing, or the state is not ready to process the request.
    ///
    /// [0]: (crate::error::ReconsiderError)
    ReconsiderBlock(block::Hash),

    /// Performs contextual validation of the given block, but does not commit it to the state.
    ///
    /// Returns [`Response::ValidBlockProposal`] when successful.
    /// See `[ReadRequest::CheckBlockProposalValidity]` for details.
    CheckBlockProposalValidity(SemanticallyVerifiedBlock),
}

impl Request {
    fn variant_name(&self) -> &'static str {
        match self {
            Request::CommitSemanticallyVerifiedBlock(_) => "commit_semantically_verified_block",
            Request::CommitCheckpointVerifiedBlock(_) => "commit_checkpoint_verified_block",

            Request::AwaitUtxo(_) => "await_utxo",
            Request::Depth(_) => "depth",
            Request::Tip => "tip",
            #[cfg(zcash_unstable = "zip234")]
            Request::TipPoolValues => "tip_pool_values",
            Request::BlockLocator => "block_locator",
            Request::Transaction(_) => "transaction",
            Request::AnyChainTransaction(_) => "any_chain_transaction",
            Request::UnspentBestChainUtxo { .. } => "unspent_best_chain_utxo",
            Request::Block(_) => "block",
            Request::BlockAndSize(_) => "block_and_size",
            Request::BlockHeader(_) => "block_header",
            Request::FindBlockHashes { .. } => "find_block_hashes",
            Request::FindBlockHeaders { .. } => "find_block_headers",
            Request::CheckBestChainTipNullifiersAndAnchors(_) => {
                "best_chain_tip_nullifiers_anchors"
            }
            Request::BestChainNextMedianTimePast => "best_chain_next_median_time_past",
            Request::BestChainBlockHash(_) => "best_chain_block_hash",
            Request::KnownBlock(_) => "known_block",
            Request::InvalidateBlock(_) => "invalidate_block",
            Request::ReconsiderBlock(_) => "reconsider_block",
            Request::CheckBlockProposalValidity(_) => "check_block_proposal_validity",
        }
    }

    /// Counts metric for StateService call
    pub fn count_metric(&self) {
        metrics::counter!(
            "state.requests",
            "service" => "state",
            "type" => self.variant_name()
        )
        .increment(1);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A read-only query about the chain state, via the
/// [`ReadStateService`](crate::service::ReadStateService).
pub enum ReadRequest {
    /// Returns [`ReadResponse::UsageInfo(num_bytes: u64)`](ReadResponse::UsageInfo)
    /// with the current disk space usage in bytes.
    UsageInfo,

    /// Returns [`ReadResponse::Tip(Option<(Height, block::Hash)>)`](ReadResponse::Tip)
    /// with the current best chain tip.
    Tip,

    /// Returns [`ReadResponse::TipPoolValues(Option<(Height, block::Hash, ValueBalance)>)`](ReadResponse::TipPoolValues)
    /// with the pool values of the current best chain tip.
    TipPoolValues,

    /// Looks up the block info after a block by hash or height in the current best chain.
    ///
    /// * [`ReadResponse::BlockInfo(Some(pool_values))`](ReadResponse::BlockInfo) if the block is in the best chain;
    /// * [`ReadResponse::BlockInfo(None)`](ReadResponse::BlockInfo) otherwise.
    BlockInfo(HashOrHeight),

    /// Computes the depth in the current best chain of the block identified by the given hash.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::Depth(Some(depth))`](ReadResponse::Depth) if the block is in the best chain;
    /// * [`ReadResponse::Depth(None)`](ReadResponse::Depth) otherwise.
    Depth(block::Hash),

    /// Looks up a block by hash or height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::Block(Some(Arc<Block>))`](ReadResponse::Block) if the block is in the best chain;
    /// * [`ReadResponse::Block(None)`](ReadResponse::Block) otherwise.
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    Block(HashOrHeight),

    //// Same as Block, but also returns serialized block size.
    ////
    /// Returns
    ///
    /// * [`ReadResponse::BlockAndSize(Some((Arc<Block>, usize)))`](ReadResponse::BlockAndSize) if the block is in the best chain;
    /// * [`ReadResponse::BlockAndSize(None)`](ReadResponse::BlockAndSize) otherwise.
    BlockAndSize(HashOrHeight),

    /// Looks up a block header by hash or height in the current best chain.
    ///
    /// Returns
    ///
    /// [`Response::BlockHeader(block::Header)`](Response::BlockHeader).
    ///
    /// Note: the [`HashOrHeight`] can be constructed from a [`block::Hash`] or
    /// [`block::Height`] using `.into()`.
    BlockHeader(HashOrHeight),

    /// Looks up a transaction by hash in the current best chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::Transaction(Some(Arc<Transaction>))`](ReadResponse::Transaction) if the transaction is in the best chain;
    /// * [`ReadResponse::Transaction(None)`](ReadResponse::Transaction) otherwise.
    Transaction(transaction::Hash),

    /// Looks up a transaction by hash in any chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::AnyChainTransaction(Some(AnyTx))`](ReadResponse::AnyChainTransaction)
    ///   if the transaction is in any chain;
    /// * [`ReadResponse::AnyChainTransaction(None)`](ReadResponse::AnyChainTransaction)
    ///   otherwise.
    AnyChainTransaction(transaction::Hash),

    /// Looks up the transaction IDs for a block, using a block hash or height.
    ///
    /// Returns
    ///
    /// * An ordered list of transaction hashes, or
    /// * `None` if the block was not found.
    ///
    /// Note: Each block has at least one transaction: the coinbase transaction.
    ///
    /// Returned txids are in the order they appear in the block.
    TransactionIdsForBlock(HashOrHeight),

    /// Looks up the transaction IDs for a block, using a block hash or height,
    /// for any chain.
    ///
    /// Returns
    ///
    /// * An ordered list of transaction hashes and a flag indicating whether
    ///   the block is in the best chain, or
    /// * `None` if the block was not found.
    ///
    /// Note: Each block has at least one transaction: the coinbase transaction.
    ///
    /// Returned txids are in the order they appear in the block.
    AnyChainTransactionIdsForBlock(HashOrHeight),

    /// Looks up a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// returning `None` immediately if it is unknown.
    ///
    /// Checks verified blocks in the finalized chain and the _best_ non-finalized chain.
    UnspentBestChainUtxo(transparent::OutPoint),

    /// Looks up a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// returning `None` immediately if it is unknown.
    ///
    /// Checks verified blocks in the finalized chain and _all_ non-finalized chains.
    ///
    /// This request is purely informational, there is no guarantee that
    /// the UTXO remains unspent in the best chain.
    AnyChainUtxo(transparent::OutPoint),

    /// Computes a block locator object based on the current best chain.
    ///
    /// Returns [`ReadResponse::BlockLocator`] with hashes starting
    /// from the best chain tip, and following the chain of previous
    /// hashes. The first hash is the best chain tip. The last hash is
    /// the tip of the finalized portion of the state. Block locators
    /// are not continuous - some intermediate hashes might be skipped.
    ///
    /// If the state is empty, the block locator is also empty.
    BlockLocator,

    /// Finds the first hash that's in the peer's `known_blocks` and the local best chain.
    /// Returns a list of hashes that follow that intersection, from the best chain.
    ///
    /// If there is no matching hash in the best chain, starts from the genesis hash.
    ///
    /// Stops the list of hashes after:
    ///   * adding the best tip,
    ///   * adding the `stop` hash to the list, if it is in the best chain, or
    ///   * adding [`MAX_FIND_BLOCK_HASHES_RESULTS`] hashes to the list.
    ///
    /// Returns an empty list if the state is empty.
    ///
    /// Returns
    ///
    /// [`ReadResponse::BlockHashes(Vec<block::Hash>)`](ReadResponse::BlockHashes).
    /// See <https://en.bitcoin.it/wiki/Protocol_documentation#getblocks>
    FindBlockHashes {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the last block hash to request.
        stop: Option<block::Hash>,
    },

    /// Finds the first hash that's in the peer's `known_blocks` and the local best chain.
    /// Returns a list of headers that follow that intersection, from the best chain.
    ///
    /// If there is no matching hash in the best chain, starts from the genesis header.
    ///
    /// Stops the list of headers after:
    ///   * adding the best tip,
    ///   * adding the header matching the `stop` hash to the list, if it is in the best chain, or
    ///   * adding [`MAX_FIND_BLOCK_HEADERS_RESULTS`] headers to the list.
    ///
    /// Returns an empty list if the state is empty.
    ///
    /// Returns
    ///
    /// [`ReadResponse::BlockHeaders(Vec<block::Header>)`](ReadResponse::BlockHeaders).
    /// See <https://en.bitcoin.it/wiki/Protocol_documentation#getheaders>
    FindBlockHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the hash of the last header to request.
        stop: Option<block::Hash>,
    },

    /// Looks up a Sapling note commitment tree either by a hash or height.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::SaplingTree(Some(Arc<NoteCommitmentTree>))`](crate::ReadResponse::SaplingTree)
    ///   if the corresponding block contains a Sapling note commitment tree.
    /// * [`ReadResponse::SaplingTree(None)`](crate::ReadResponse::SaplingTree) otherwise.
    SaplingTree(HashOrHeight),

    /// Looks up an Orchard note commitment tree either by a hash or height.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::OrchardTree(Some(Arc<NoteCommitmentTree>))`](crate::ReadResponse::OrchardTree)
    ///   if the corresponding block contains a Sapling note commitment tree.
    /// * [`ReadResponse::OrchardTree(None)`](crate::ReadResponse::OrchardTree) otherwise.
    OrchardTree(HashOrHeight),

    /// Returns a list of Sapling note commitment subtrees by their indexes, starting at
    /// `start_index`, and returning up to `limit` subtrees.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::SaplingSubtree(BTreeMap<_, NoteCommitmentSubtreeData<_>>))`](crate::ReadResponse::SaplingSubtrees)
    /// * An empty list if there is no subtree at `start_index`.
    SaplingSubtrees {
        /// The index of the first 2^16-leaf subtree to return.
        start_index: NoteCommitmentSubtreeIndex,
        /// The maximum number of subtree values to return.
        limit: Option<NoteCommitmentSubtreeIndex>,
    },

    /// Returns a list of Orchard note commitment subtrees by their indexes, starting at
    /// `start_index`, and returning up to `limit` subtrees.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::OrchardSubtree(BTreeMap<_, NoteCommitmentSubtreeData<_>>))`](crate::ReadResponse::OrchardSubtrees)
    /// * An empty list if there is no subtree at `start_index`.
    OrchardSubtrees {
        /// The index of the first 2^16-leaf subtree to return.
        start_index: NoteCommitmentSubtreeIndex,
        /// The maximum number of subtree values to return.
        limit: Option<NoteCommitmentSubtreeIndex>,
    },

    /// Looks up the balance of a set of transparent addresses.
    ///
    /// Returns an [`Amount`] with the total
    /// balance of the set of addresses.
    AddressBalance(HashSet<transparent::Address>),

    /// Looks up transaction hashes that were sent or received from addresses,
    /// in an inclusive blockchain height range.
    ///
    /// Returns
    ///
    /// * An ordered, unique map of transaction locations and hashes.
    /// * An empty map if no transactions were found for the given arguments.
    ///
    /// Returned txids are in the order they appear in blocks,
    /// which ensures that they are topologically sorted
    /// (i.e. parent txids will appear before child txids).
    TransactionIdsByAddresses {
        /// The requested addresses.
        addresses: HashSet<transparent::Address>,

        /// The blocks to be queried for transactions.
        height_range: RangeInclusive<block::Height>,
    },

    /// Looks up a spending transaction id by its spent transparent input.
    ///
    /// Returns [`ReadResponse::TransactionId`] with the hash of the transaction
    /// that spent the output at the provided [`transparent::OutPoint`].
    #[cfg(feature = "indexer")]
    SpendingTransactionId(Spend),

    /// Looks up utxos for the provided addresses.
    ///
    /// Returns a type with found utxos and transaction information.
    UtxosByAddresses(HashSet<transparent::Address>),

    /// Contextually validates anchors and nullifiers of a transaction on the best chain
    ///
    /// Returns [`ReadResponse::ValidBestChainTipNullifiersAndAnchors`].
    CheckBestChainTipNullifiersAndAnchors(UnminedTx),

    /// Calculates the median-time-past for the *next* block on the best chain.
    ///
    /// Returns [`ReadResponse::BestChainNextMedianTimePast`] when successful.
    BestChainNextMedianTimePast,

    /// Looks up a block hash by height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::BlockHash(Some(hash))`](ReadResponse::BlockHash) if the block is in the best chain;
    /// * [`ReadResponse::BlockHash(None)`](ReadResponse::BlockHash) otherwise.
    BestChainBlockHash(block::Height),

    /// Get state information from the best block chain.
    ///
    /// Returns [`ReadResponse::ChainInfo(info)`](ReadResponse::ChainInfo) where `info` is a
    /// [`zebra-state::GetBlockTemplateChainInfo`](zebra-state::GetBlockTemplateChainInfo)` structure containing
    /// best chain state information.
    ChainInfo,

    /// Get the average solution rate in the best chain.
    ///
    /// Returns [`ReadResponse::SolutionRate`]
    SolutionRate {
        /// The number of blocks to calculate the average difficulty for.
        num_blocks: usize,
        /// Optionally estimate the network solution rate at the time when this height was mined.
        /// Otherwise, estimate at the current tip height.
        height: Option<block::Height>,
    },

    /// Performs contextual validation of the given block, but does not commit it to the state.
    ///
    /// It is the caller's responsibility to perform semantic validation.
    /// (The caller does not need to check proof of work for block proposals.)
    ///
    /// Returns [`ReadResponse::ValidBlockProposal`] when successful, or an error if
    /// the block fails contextual validation.
    CheckBlockProposalValidity(SemanticallyVerifiedBlock),

    /// Returns [`ReadResponse::TipBlockSize(usize)`](ReadResponse::TipBlockSize)
    /// with the current best chain tip block size in bytes.
    TipBlockSize,

    /// Returns [`ReadResponse::NonFinalizedBlocksListener`] with a channel receiver
    /// allowing the caller to listen for new blocks in the non-finalized state.
    NonFinalizedBlocksListener,
}

impl ReadRequest {
    fn variant_name(&self) -> &'static str {
        match self {
            ReadRequest::UsageInfo => "usage_info",
            ReadRequest::Tip => "tip",
            ReadRequest::TipPoolValues => "tip_pool_values",
            ReadRequest::BlockInfo(_) => "block_info",
            ReadRequest::Depth(_) => "depth",
            ReadRequest::Block(_) => "block",
            ReadRequest::BlockAndSize(_) => "block_and_size",
            ReadRequest::BlockHeader(_) => "block_header",
            ReadRequest::Transaction(_) => "transaction",
            ReadRequest::AnyChainTransaction(_) => "any_chain_transaction",
            ReadRequest::TransactionIdsForBlock(_) => "transaction_ids_for_block",
            ReadRequest::AnyChainTransactionIdsForBlock(_) => "any_chain_transaction_ids_for_block",
            ReadRequest::UnspentBestChainUtxo { .. } => "unspent_best_chain_utxo",
            ReadRequest::AnyChainUtxo { .. } => "any_chain_utxo",
            ReadRequest::BlockLocator => "block_locator",
            ReadRequest::FindBlockHashes { .. } => "find_block_hashes",
            ReadRequest::FindBlockHeaders { .. } => "find_block_headers",
            ReadRequest::SaplingTree { .. } => "sapling_tree",
            ReadRequest::OrchardTree { .. } => "orchard_tree",
            ReadRequest::SaplingSubtrees { .. } => "sapling_subtrees",
            ReadRequest::OrchardSubtrees { .. } => "orchard_subtrees",
            ReadRequest::AddressBalance { .. } => "address_balance",
            ReadRequest::TransactionIdsByAddresses { .. } => "transaction_ids_by_addresses",
            ReadRequest::UtxosByAddresses(_) => "utxos_by_addresses",
            ReadRequest::CheckBestChainTipNullifiersAndAnchors(_) => {
                "best_chain_tip_nullifiers_anchors"
            }
            ReadRequest::BestChainNextMedianTimePast => "best_chain_next_median_time_past",
            ReadRequest::BestChainBlockHash(_) => "best_chain_block_hash",
            #[cfg(feature = "indexer")]
            ReadRequest::SpendingTransactionId(_) => "spending_transaction_id",
            ReadRequest::ChainInfo => "chain_info",
            ReadRequest::SolutionRate { .. } => "solution_rate",
            ReadRequest::CheckBlockProposalValidity(_) => "check_block_proposal_validity",
            ReadRequest::TipBlockSize => "tip_block_size",
            ReadRequest::NonFinalizedBlocksListener => "non_finalized_blocks_listener",
        }
    }

    /// Counts metric for ReadStateService call
    pub fn count_metric(&self) {
        metrics::counter!(
            "state.requests",
            "service" => "read_state",
            "type" => self.variant_name()
        )
        .increment(1);
    }
}

/// Conversion from read-write [`Request`]s to read-only [`ReadRequest`]s.
///
/// Used to dispatch read requests concurrently from the [`StateService`](crate::service::StateService).
impl TryFrom<Request> for ReadRequest {
    type Error = &'static str;

    fn try_from(request: Request) -> Result<ReadRequest, Self::Error> {
        match request {
            Request::Tip => Ok(ReadRequest::Tip),
            #[cfg(zcash_unstable = "zip234")]
            Request::TipPoolValues => Ok(ReadRequest::TipPoolValues),
            Request::Depth(hash) => Ok(ReadRequest::Depth(hash)),
            Request::BestChainNextMedianTimePast => Ok(ReadRequest::BestChainNextMedianTimePast),
            Request::BestChainBlockHash(hash) => Ok(ReadRequest::BestChainBlockHash(hash)),

            Request::Block(hash_or_height) => Ok(ReadRequest::Block(hash_or_height)),
            Request::BlockAndSize(hash_or_height) => Ok(ReadRequest::BlockAndSize(hash_or_height)),
            Request::BlockHeader(hash_or_height) => Ok(ReadRequest::BlockHeader(hash_or_height)),
            Request::Transaction(tx_hash) => Ok(ReadRequest::Transaction(tx_hash)),
            Request::AnyChainTransaction(tx_hash) => Ok(ReadRequest::AnyChainTransaction(tx_hash)),
            Request::UnspentBestChainUtxo(outpoint) => {
                Ok(ReadRequest::UnspentBestChainUtxo(outpoint))
            }

            Request::BlockLocator => Ok(ReadRequest::BlockLocator),
            Request::FindBlockHashes { known_blocks, stop } => {
                Ok(ReadRequest::FindBlockHashes { known_blocks, stop })
            }
            Request::FindBlockHeaders { known_blocks, stop } => {
                Ok(ReadRequest::FindBlockHeaders { known_blocks, stop })
            }

            Request::CheckBestChainTipNullifiersAndAnchors(tx) => {
                Ok(ReadRequest::CheckBestChainTipNullifiersAndAnchors(tx))
            }

            Request::CommitSemanticallyVerifiedBlock(_)
            | Request::CommitCheckpointVerifiedBlock(_)
            | Request::InvalidateBlock(_)
            | Request::ReconsiderBlock(_) => Err("ReadService does not write blocks"),

            Request::AwaitUtxo(_) => Err("ReadService does not track pending UTXOs. \
                     Manually convert the request to ReadRequest::AnyChainUtxo, \
                     and handle pending UTXOs"),

            Request::KnownBlock(_) => Err("ReadService does not track queued blocks"),

            Request::CheckBlockProposalValidity(semantically_verified) => Ok(
                ReadRequest::CheckBlockProposalValidity(semantically_verified),
            ),
        }
    }
}
