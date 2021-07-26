use std::{collections::HashMap, sync::Arc};

use zebra_chain::{
    block::{self, Block},
    transaction,
    transparent::{self, CoinbaseSpendRestriction},
};

// Allow *only* this unused import, so that rustdoc link resolution
// will work with inline links.
#[allow(unused_imports)]
use crate::Response;

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
    /// [`Outpoint`](transparent::Outpoint).
    ///
    /// Each output is tagged with its transaction index in the block.
    /// (The outputs of earlier transactions in a block can be spent by later
    /// transactions.)
    ///
    /// Note: although these transparent outputs are newly created, they may not
    /// be unspent, since a later transaction in a block can spend outputs of an
    /// earlier transaction.
    pub new_outputs: HashMap<transparent::OutPoint, transparent::OrderedUtxo>,
    /// A precomputed list of the hashes of the transactions in this block.
    pub transaction_hashes: Vec<transaction::Hash>,
    // TODO: add these parameters when we can compute anchors.
    // sprout_anchor: sprout::tree::Root,
    // sapling_anchor: sapling::tree::Root,
}

/// A contextually validated block, ready to be committed directly to the finalized state with
/// no checks, if it becomes the root of the best non-finalized chain.
///
/// Used by the state service and non-finalized [`Chain`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextuallyValidBlock {
    pub(crate) block: Arc<Block>,
    pub(crate) hash: block::Hash,
    pub(crate) height: block::Height,
    pub(crate) new_outputs: HashMap<transparent::OutPoint, transparent::Utxo>,
    pub(crate) transaction_hashes: Vec<transaction::Hash>,
}

/// A finalized block, ready to be committed directly to the finalized state with
/// no checks.
///
/// This is exposed for use in checkpointing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizedBlock {
    // These are pub(crate) so we can add whatever db-format-dependent
    // precomputation we want here without leaking internal details.
    pub(crate) block: Arc<Block>,
    pub(crate) hash: block::Hash,
    pub(crate) height: block::Height,
    pub(crate) new_outputs: HashMap<transparent::OutPoint, transparent::Utxo>,
    pub(crate) transaction_hashes: Vec<transaction::Hash>,
}

// Doing precomputation in this From impl means that it will be done in
// the *service caller*'s task, not inside the service call itself.
// This allows moving work out of the single-threaded state service.
impl From<Arc<Block>> for FinalizedBlock {
    fn from(block: Arc<Block>) -> Self {
        let height = block
            .coinbase_height()
            .expect("finalized blocks must have a valid coinbase height");
        let hash = block.hash();
        let transaction_hashes = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();
        let new_outputs = transparent::new_outputs(&block, transaction_hashes.as_slice());

        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}

impl From<PreparedBlock> for ContextuallyValidBlock {
    fn from(prepared: PreparedBlock) -> Self {
        let PreparedBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        } = prepared;
        Self {
            block,
            hash,
            height,
            new_outputs: transparent::utxos_from_ordered_utxos(new_outputs),
            transaction_hashes,
        }
    }
}

impl From<ContextuallyValidBlock> for FinalizedBlock {
    fn from(contextually_valid: ContextuallyValidBlock) -> Self {
        let ContextuallyValidBlock {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        } = contextually_valid;
        Self {
            block,
            hash,
            height,
            new_outputs,
            transaction_hashes,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A query about or modification to the chain state.
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

    /// Commit a finalized block to the state, skipping all validation.
    ///
    /// This is exposed for use in checkpointing, which produces finalized
    /// blocks. It is the caller's responsibility to ensure that the block is
    /// valid and final. This request can be made out-of-order; the state service
    /// will queue it until its parent is ready.
    ///
    /// Returns [`Response::Committed`] with the hash of the newly committed
    /// block, or an error.
    ///
    /// This request cannot be cancelled once submitted; dropping the response
    /// future will have no effect on whether it is eventually processed.
    /// Duplicate requests should not be made, because it is the caller's
    /// responsibility to ensure that each block is valid and final.
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

    /// Returns [`Response::Tip`] with the current best chain tip.
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

    /// Requests a spendable [`Utxo`] identified by the given Outpoint.
    /// If the UTXO is unknown or unspendable, waits until it becomes available
    /// and spendable.
    ///
    /// This request is purely informational, and there are no guarantees about
    /// whether the UTXO remains unspent or is on the best chain, or any chain.
    /// Its purpose is to allow asynchronous script verification.
    ///
    /// However, the spendability of a UTXO can not change, because
    /// [coinbase transaction IDs commit to the relevant data](https://github.com/ZcashFoundation/zebra/blob/main/book/src/dev/rfcs/0004-asynchronous-script-verification.md#parallel-coinbase-checks).
    ///
    /// # Correctness
    ///
    /// UTXO requests should be wrapped in a timeout, so that
    /// out-of-order and invalid requests do not hang indefinitely. See the [`crate`]
    /// documentation for details.
    AwaitSpendableUtxo {
        /// The output pointer used to look up the [`Utxo`].
        outpoint: transparent::OutPoint,

        /// The spend restriction which applies to the [`Utxo`].
        ///
        /// Only relevant if the [`Utxo`] is a coinbase transaction output.
        spend_restriction: CoinbaseSpendRestriction,
    },

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
    /// See https://en.bitcoin.it/wiki/Protocol_documentation#getblocks
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
    ///   * adding 160 headers to the list.
    ///
    /// Returns an empty list if the state is empty.
    ///
    /// Returns
    ///
    /// [`Response::BlockHeaders(Vec<block::Header>)`](Response::BlockHeaders).
    /// See https://en.bitcoin.it/wiki/Protocol_documentation#getheaders
    FindBlockHeaders {
        /// Hashes of known blocks, ordered from highest height to lowest height.
        known_blocks: Vec<block::Hash>,
        /// Optionally, the hash of the last header to request.
        stop: Option<block::Hash>,
    },
}
