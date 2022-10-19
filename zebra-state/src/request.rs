//! State [`tower::Service`] request types.

use std::{
    collections::{HashMap, HashSet},
    ops::RangeInclusive,
    sync::Arc,
};

use zebra_chain::{
    amount::NegativeAllowed,
    block::{self, Block},
    history_tree::HistoryTree,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    sapling,
    serialization::SerializationError,
    sprout, transaction,
    transparent::{self, utxos_from_ordered_utxos},
    value_balance::{ValueBalance, ValueBalanceError},
};

/// Allow *only* these unused imports, so that rustdoc link resolution
/// will work with inline links.
#[allow(unused_imports)]
use crate::{
    constants::{MAX_FIND_BLOCK_HASHES_RESULTS, MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA},
    ReadResponse, Response,
};

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
}

impl From<block::Hash> for HashOrHeight {
    fn from(hash: block::Hash) -> Self {
        Self::Hash(hash)
    }
}

impl From<block::Height> for HashOrHeight {
    fn from(hash: block::Height) -> Self {
        Self::Height(hash)
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
pub struct PreparedBlock {
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
}

// Some fields are pub(crate), so we can add whatever db-format-dependent
// precomputation we want here without leaking internal details.

/// A contextually validated block, ready to be committed directly to the finalized state with
/// no checks, if it becomes the root of the best non-finalized chain.
///
/// Used by the state service and non-finalized `Chain`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextuallyValidBlock {
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

/// A finalized block, ready to be committed directly to the finalized state with
/// no checks.
///
/// This is exposed for use in checkpointing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedBlock {
    /// The block to commit to the state.
    pub block: Arc<Block>,
    /// The hash of the block.
    pub hash: block::Hash,
    /// The height of the block.
    pub height: block::Height,
    /// New transparent outputs created in this block, indexed by
    /// [`OutPoint`](transparent::OutPoint).
    ///
    /// Note: although these transparent outputs are newly created, they may not
    /// be unspent, since a later transaction in a block can spend outputs of an
    /// earlier transaction.
    ///
    /// This field can also contain unrelated outputs, which are ignored.
    pub(crate) new_outputs: HashMap<transparent::OutPoint, transparent::Utxo>,
    /// A precomputed list of the hashes of the transactions in this block,
    /// in the same order as `block.transactions`.
    pub transaction_hashes: Arc<[transaction::Hash]>,
}

/// Wraps note commitment trees and the history tree together.
pub struct Treestate {
    /// Note commitment trees.
    pub note_commitment_trees: NoteCommitmentTrees,
    /// History tree.
    pub history_tree: Arc<HistoryTree>,
}

impl Treestate {
    pub fn new(
        sprout: Arc<sprout::tree::NoteCommitmentTree>,
        sapling: Arc<sapling::tree::NoteCommitmentTree>,
        orchard: Arc<orchard::tree::NoteCommitmentTree>,
        history_tree: Arc<HistoryTree>,
    ) -> Self {
        Self {
            note_commitment_trees: NoteCommitmentTrees {
                sprout,
                sapling,
                orchard,
            },
            history_tree,
        }
    }
}

/// Contains a block ready to be committed together with its associated
/// treestate.
///
/// Zebra's non-finalized state passes this `struct` over to the finalized state
/// when committing a block. The associated treestate is passed so that the
/// finalized state does not have to retrieve the previous treestate from the
/// database and recompute the new one.
pub struct FinalizedWithTrees {
    /// A block ready to be committed.
    pub finalized: FinalizedBlock,
    /// The tresstate associated with the block.
    pub treestate: Option<Treestate>,
}

impl FinalizedWithTrees {
    pub fn new(block: ContextuallyValidBlock, treestate: Treestate) -> Self {
        let finalized = FinalizedBlock::from(block);

        Self {
            finalized,
            treestate: Some(treestate),
        }
    }
}

impl From<Arc<Block>> for FinalizedWithTrees {
    fn from(block: Arc<Block>) -> Self {
        Self::from(FinalizedBlock::from(block))
    }
}

impl From<FinalizedBlock> for FinalizedWithTrees {
    fn from(block: FinalizedBlock) -> Self {
        Self {
            finalized: block,
            treestate: None,
        }
    }
}

impl From<&PreparedBlock> for PreparedBlock {
    fn from(prepared: &PreparedBlock) -> Self {
        prepared.clone()
    }
}

// Doing precomputation in these impls means that it will be done in
// the *service caller*'s task, not inside the service call itself.
// This allows moving work out of the single-threaded state service.

impl ContextuallyValidBlock {
    /// Create a block that's ready for non-finalized `Chain` contextual validation,
    /// using a [`PreparedBlock`] and the UTXOs it spends.
    ///
    /// When combined, `prepared.new_outputs` and `spent_utxos` must contain
    /// the [`Utxo`](transparent::Utxo)s spent by every transparent input in this block,
    /// including UTXOs created by earlier transactions in this block.
    ///
    /// Note: a [`ContextuallyValidBlock`] isn't actually contextually valid until
    /// `Chain::update_chain_state_with` returns success.
    pub fn with_block_and_spent_utxos(
        prepared: PreparedBlock,
        mut spent_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    ) -> Result<Self, ValueBalanceError> {
        let PreparedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        } = prepared;

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
            chain_value_pool_change: block
                .chain_value_pool_change(&utxos_from_ordered_utxos(spent_outputs))?,
        })
    }
}

impl FinalizedBlock {
    /// Create a block that's ready to be committed to the finalized state,
    /// using a precalculated [`block::Hash`].
    ///
    /// Note: a [`FinalizedBlock`] isn't actually finalized
    /// until [`Request::CommitFinalizedBlock`] returns success.
    pub fn with_hash(block: Arc<Block>, hash: block::Hash) -> Self {
        let height = block
            .coinbase_height()
            .expect("coinbase height was already checked");
        let transaction_hashes: Arc<[_]> = block.transactions.iter().map(|tx| tx.hash()).collect();
        let new_outputs = transparent::new_outputs(&block, &transaction_hashes);

        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}

impl From<Arc<Block>> for FinalizedBlock {
    fn from(block: Arc<Block>) -> Self {
        let hash = block.hash();

        FinalizedBlock::with_hash(block, hash)
    }
}

impl From<ContextuallyValidBlock> for FinalizedBlock {
    fn from(contextually_valid: ContextuallyValidBlock) -> Self {
        let ContextuallyValidBlock {
            block,
            hash,
            height,
            new_outputs,
            spent_outputs: _,
            transaction_hashes,
            chain_value_pool_change: _,
        } = contextually_valid;

        Self {
            block,
            hash,
            height,
            new_outputs: utxos_from_ordered_utxos(new_outputs),
            transaction_hashes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A query about or modification to the chain state, via the
/// [`StateService`](crate::service::StateService).
pub enum Request {
    /// Performs contextual validation of the given block, committing it to the
    /// state if successful.
    ///
    /// It is the caller's responsibility to perform semantic validation. This
    /// request can be made out-of-order; the state service will queue it until
    /// its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the block when it is
    /// committed to the state, or an error if the block fails contextual
    /// validation or has already been committed to the state.
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
    CommitBlock(PreparedBlock),

    /// Commit a checkpointed block to the state, skipping most block validation.
    ///
    /// This is exposed for use in checkpointing, which produces finalized
    /// blocks. It is the caller's responsibility to ensure that the block is
    /// semantically valid and final. This request can be made out-of-order;
    /// the state service will queue it until its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly committed
    /// block, or an error.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed.
    /// Duplicate requests will replace the older duplicate, and return an error
    /// in its response future.
    ///
    /// # Note
    ///
    /// Finalized and non-finalized blocks are an internal Zebra implementation detail.
    /// There is no difference between these blocks on the network, or in Zebra's
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
    CommitFinalizedBlock(FinalizedBlock),

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

    /// Request a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// waiting until it becomes available if it is unknown.
    ///
    /// Checks the finalized chain, all non-finalized chains, queued unverified blocks,
    /// and any blocks that arrive at the state after the request future has been created.
    ///
    /// This request is purely informational, and there are no guarantees about
    /// whether the UTXO remains unspent or is on the best chain, or any chain.
    /// Its purpose is to allow asynchronous script verification.
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
    ///   * adding [`MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA`] headers to the list.
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
}

impl Request {
    fn variant_name(&self) -> &'static str {
        match self {
            Request::CommitBlock(_) => "commit_block",
            Request::CommitFinalizedBlock(_) => "commit_finalized_block",
            Request::AwaitUtxo(_) => "await_utxo",
            Request::Depth(_) => "depth",
            Request::Tip => "tip",
            Request::BlockLocator => "block_locator",
            Request::Transaction(_) => "transaction",
            Request::Block(_) => "block",
            Request::FindBlockHashes { .. } => "find_block_hashes",
            Request::FindBlockHeaders { .. } => "find_block_headers",
        }
    }

    /// Counts metric for StateService call
    pub fn count_metric(&self) {
        metrics::counter!(
            "state.requests",
            1,
            "service" => "state",
            "type" => self.variant_name()
        );
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A read-only query about the chain state, via the
/// [`ReadStateService`](crate::service::ReadStateService).
pub enum ReadRequest {
    /// Returns [`ReadResponse::Tip(Option<(Height, block::Hash)>)`](ReadResponse::Tip)
    /// with the current best chain tip.
    Tip,

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

    /// Looks up a transaction by hash in the current best chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::Transaction(Some(Arc<Transaction>))`](ReadResponse::Transaction) if the transaction is in the best chain;
    /// * [`ReadResponse::Transaction(None)`](ReadResponse::Transaction) otherwise.
    Transaction(transaction::Hash),

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

    /// Looks up a UTXO identified by the given [`OutPoint`](transparent::OutPoint),
    /// returning `None` immediately if it is unknown.
    ///
    /// Checks verified blocks in the finalized chain and the _best_ non-finalized chain.
    ///
    /// This request is purely informational, there is no guarantee that
    /// the UTXO remains unspent in the best chain.
    BestChainUtxo(transparent::OutPoint),

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
    ///   * adding [`MAX_FIND_BLOCK_HEADERS_RESULTS_FOR_ZEBRA`] headers to the list.
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

    /// Looks up the balance of a set of transparent addresses.
    ///
    /// Returns an [`Amount`](zebra_chain::amount::Amount) with the total
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

    /// Looks up utxos for the provided addresses.
    ///
    /// Returns a type with found utxos and transaction information.
    UtxosByAddresses(HashSet<transparent::Address>),

    #[cfg(feature = "getblocktemplate-rpcs")]
    /// Looks up a block hash by height in the current best chain.
    ///
    /// Returns
    ///
    /// * [`ReadResponse::BlockHash(Some(hash))`](ReadResponse::BlockHash) if the block is in the best chain;
    /// * [`ReadResponse::BlockHash(None)`](ReadResponse::BlockHash) otherwise.
    BestChainBlockHash(block::Height),
}

impl ReadRequest {
    fn variant_name(&self) -> &'static str {
        match self {
            ReadRequest::Tip => "tip",
            ReadRequest::Depth(_) => "depth",
            ReadRequest::Block(_) => "block",
            ReadRequest::Transaction(_) => "transaction",
            ReadRequest::TransactionIdsForBlock(_) => "transaction_ids_for_block",
            ReadRequest::BestChainUtxo { .. } => "best_chain_utxo",
            ReadRequest::AnyChainUtxo { .. } => "any_chain_utxo",
            ReadRequest::BlockLocator => "block_locator",
            ReadRequest::FindBlockHashes { .. } => "find_block_hashes",
            ReadRequest::FindBlockHeaders { .. } => "find_block_headers",
            ReadRequest::SaplingTree { .. } => "sapling_tree",
            ReadRequest::OrchardTree { .. } => "orchard_tree",
            ReadRequest::AddressBalance { .. } => "address_balance",
            ReadRequest::TransactionIdsByAddresses { .. } => "transaction_ids_by_addesses",
            ReadRequest::UtxosByAddresses(_) => "utxos_by_addesses",
            #[cfg(feature = "getblocktemplate-rpcs")]
            ReadRequest::BestChainBlockHash(_) => "best_chain_block_hash",
        }
    }

    /// Counts metric for ReadStateService call
    pub fn count_metric(&self) {
        metrics::counter!(
            "state.requests",
            1,
            "service" => "read_state",
            "type" => self.variant_name()
        );
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
            Request::Depth(hash) => Ok(ReadRequest::Depth(hash)),

            Request::Block(hash_or_height) => Ok(ReadRequest::Block(hash_or_height)),
            Request::Transaction(tx_hash) => Ok(ReadRequest::Transaction(tx_hash)),

            Request::BlockLocator => Ok(ReadRequest::BlockLocator),
            Request::FindBlockHashes { known_blocks, stop } => {
                Ok(ReadRequest::FindBlockHashes { known_blocks, stop })
            }
            Request::FindBlockHeaders { known_blocks, stop } => {
                Ok(ReadRequest::FindBlockHeaders { known_blocks, stop })
            }

            Request::CommitBlock(_) | Request::CommitFinalizedBlock(_) => {
                Err("ReadService does not write blocks")
            }

            Request::AwaitUtxo(_) => Err("ReadService does not track pending UTXOs. \
                     Manually convert the request to ReadRequest::AnyChainUtxo, \
                     and handle pending UTXOs"),
        }
    }
}
