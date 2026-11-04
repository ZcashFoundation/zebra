//! Block and treestate data types shared with state services.

use std::{
    collections::HashMap,
    ops::{Add, Deref},
    sync::Arc,
    time::Instant,
};

use zebra_chain::{
    amount::DeferredPoolBalanceChange,
    block::{self, merkle::AuthDataRoot, Block, HeightDiff},
    history_tree::HistoryTree,
    parallel::tree::NoteCommitmentTrees,
    serialization::SerializationError,
    transaction, transparent,
};

/// The per-pool nullifier types used by the indexer-only [`Spend`] enum, imported here rather than
/// in the shared import block because they are only referenced under the `indexer` feature.
#[cfg(feature = "indexer")]
use zebra_chain::{ironwood, orchard, sapling, sprout};

/// Identify a spend by a transparent outpoint or revealed nullifier.
///
/// This enum implements `From` for [`transparent::OutPoint`], [`sprout::Nullifier`],
/// [`sapling::Nullifier`], [`orchard::Nullifier`], and [`ironwood::Nullifier`].
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
    /// A spend identified by an [`ironwood::Nullifier`].
    Ironwood(ironwood::Nullifier),
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

#[cfg(feature = "indexer")]
impl From<ironwood::Nullifier> for Spend {
    fn from(ironwood_nullifier: ironwood::Nullifier) -> Self {
        Self::Ironwood(ironwood_nullifier)
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

    /// The local time at which the block verifier received this block, if it passed
    /// through the block verifier.
    ///
    /// Used by `Chain::cmp` to prefer the first-received chain when cumulative works
    /// are equal, per the protocol specification:
    ///
    /// > To break ties between leaf blocks, a node will prefer the block that it received first.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#blockchain>
    ///
    /// This is node-local, in-memory metadata, not consensus data. It is `None` for
    /// blocks constructed outside the block verifier (checkpoint sync, backup restore,
    /// tests); `Chain::cmp` treats unstamped blocks as received before any stamped
    /// block, like `zcashd`'s disk-loaded blocks, which all share `nSequenceId` 0.
    pub received_time: Option<Instant>,
}

/// A block ready to be committed directly to the finalized state with
/// a small number of checks if compared with a `ContextuallyVerifiedBlock`.
///
/// This is exposed for use in checkpointing.
///
/// Note: The difference between a `CheckpointVerifiedBlock` and a `ContextuallyVerifiedBlock` is
/// that the `CheckpointVerifier` doesn't bind the transaction authorizing data to the
/// `ChainHistoryBlockTxAuthCommitmentHash`, but the `NonFinalizedState` and `FinalizedState` do.
///
/// The wrapped block is immutable so a cached authorizing-data root cannot be paired with another
/// block body:
///
/// ```compile_fail
/// use std::sync::Arc;
/// use zebra_chain::block::Block;
/// use zebra_db::CheckpointVerifiedBlock;
///
/// fn replace_body(mut verified: CheckpointVerifiedBlock, block: Arc<Block>) {
///     verified.block = block;
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointVerifiedBlock {
    verified: SemanticallyVerifiedBlock,
    auth_data_root: Option<AuthDataRoot>,
    commit_aids: Option<Arc<CommitAids>>,
}

/// Per-block data a sync engine attaches to a [`CheckpointVerifiedBlock`] so
/// the finalized writer can skip work.
///
/// Always absent for now. The verified tree
/// roots PR adds `tree_roots`, adopted per pool and per block only after the
/// following header verifies (zips#1346 S:265-280).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommitAids {}

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
    pub fn new(note_commitment_trees: NoteCommitmentTrees, history_tree: Arc<HistoryTree>) -> Self {
        Self {
            note_commitment_trees,
            history_tree,
        }
    }
}

/// Contains a block with all its associated data that the finalized state can commit to its
/// database.
///
/// Note that it's the constructor's responsibility to ensure that all data is valid and verified.
pub struct FinalizedBlock {
    /// The block to commit to the state.
    pub block: Arc<Block>,
    /// The hash of the block.
    pub hash: block::Hash,
    /// The height of the block.
    pub height: block::Height,
    /// New transparent outputs created in this block, indexed by
    /// [`OutPoint`](transparent::OutPoint).
    pub new_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// A precomputed list of the hashes of the transactions in this block, in the same order as
    /// `block.transactions`.
    pub transaction_hashes: Arc<[transaction::Hash]>,
    /// The tresstate associated with the block.
    pub treestate: Treestate,
    /// This block's deferred pool value balance change.
    pub deferred_pool_balance_change: DeferredPoolBalanceChange,
    /// The block's authorizing data root, if the checkpoint verifier already
    /// computed it from this block body.
    pub auth_data_root: Option<AuthDataRoot>,
}

impl FinalizedBlock {
    /// Constructs [`FinalizedBlock`] from [`CheckpointVerifiedBlock`] and its [`Treestate`].
    pub fn from_checkpoint_verified(
        block: CheckpointVerifiedBlock,
        treestate: Treestate,
        deferred_pool_balance_change: DeferredPoolBalanceChange,
    ) -> Self {
        let auth_data_root = block.precomputed_auth_data_root();

        Self::from_semantically_verified(
            SemanticallyVerifiedBlock::from(block),
            treestate,
            deferred_pool_balance_change,
            auth_data_root,
        )
    }

    /// Constructs [`FinalizedBlock`] from [`SemanticallyVerifiedBlock`] and its [`Treestate`].
    pub fn from_semantically_verified(
        block: SemanticallyVerifiedBlock,
        treestate: Treestate,
        deferred_pool_balance_change: DeferredPoolBalanceChange,
        auth_data_root: Option<AuthDataRoot>,
    ) -> Self {
        Self {
            block: block.block,
            hash: block.hash,
            height: block.height,
            new_outputs: block.new_outputs,
            transaction_hashes: block.transaction_hashes,
            treestate,
            deferred_pool_balance_change,
            auth_data_root,
        }
    }
}

impl AsRef<FinalizedBlock> for FinalizedBlock {
    fn as_ref(&self) -> &FinalizedBlock {
        self
    }
}

impl From<&SemanticallyVerifiedBlock> for SemanticallyVerifiedBlock {
    fn from(semantically_verified: &SemanticallyVerifiedBlock) -> Self {
        semantically_verified.clone()
    }
}

impl CheckpointVerifiedBlock {
    /// Creates a [`CheckpointVerifiedBlock`] from [`Block`] with optional deferred balance and
    /// optional pre-computed hash.
    pub fn new(block: Arc<Block>, hash: Option<block::Hash>) -> Self {
        Self::with_hash(block.clone(), hash.unwrap_or(block.hash()))
    }
    /// Creates a block that's ready to be committed to the finalized state,
    /// using a precalculated [`block::Hash`].
    ///
    /// Note: a [`CheckpointVerifiedBlock`] isn't actually finalized
    /// until the state commit request returns success.
    pub fn with_hash(block: Arc<Block>, hash: block::Hash) -> Self {
        Self::from(SemanticallyVerifiedBlock::with_hash(block, hash))
    }

    /// Computes and caches the authorizing-data root from this immutable block body.
    pub fn with_precomputed_auth_data_root(mut self) -> Self {
        self.auth_data_root = Some(self.block.auth_data_root());
        self
    }

    /// Returns the cached root, if it was computed before the block reached the writer.
    pub fn precomputed_auth_data_root(&self) -> Option<AuthDataRoot> {
        self.auth_data_root
    }

    /// Attaches commit aids. No caller sets them yet: the verified tree
    /// roots change is the first.
    pub fn with_commit_aids(mut self, aids: Arc<CommitAids>) -> Self {
        self.commit_aids = Some(aids);
        self
    }

    /// The attached commit aids, if any; the finalized writer ignores them
    /// until the roots PR.
    pub fn commit_aids(&self) -> Option<&Arc<CommitAids>> {
        self.commit_aids.as_ref()
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
            received_time: None,
        }
    }
}

impl From<Arc<Block>> for CheckpointVerifiedBlock {
    fn from(block: Arc<Block>) -> Self {
        Self::from(SemanticallyVerifiedBlock::from(block))
    }
}

impl From<SemanticallyVerifiedBlock> for CheckpointVerifiedBlock {
    fn from(verified: SemanticallyVerifiedBlock) -> Self {
        Self {
            verified,
            auth_data_root: None,
            commit_aids: None,
        }
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
            received_time: None,
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
            // The finalized state does not track receipt times, so there is no stamp.
            received_time: None,
        }
    }
}

impl From<CheckpointVerifiedBlock> for SemanticallyVerifiedBlock {
    fn from(checkpoint_verified: CheckpointVerifiedBlock) -> Self {
        checkpoint_verified.verified
    }
}

impl Deref for CheckpointVerifiedBlock {
    type Target = SemanticallyVerifiedBlock;

    fn deref(&self) -> &Self::Target {
        &self.verified
    }
}
