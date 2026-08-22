//! Shared block, header, and transaction reading code.
//!
//! In the functions in this module:
//!
//! The block write task commits blocks to the finalized state before updating
//! `chain` or `non_finalized_state` with a cached copy of the non-finalized chains
//! in `NonFinalizedState.chain_set`. Then the block commit task can
//! commit additional blocks to the finalized state after we've cloned the
//! `chain` or `non_finalized_state`.
//!
//! This means that some blocks can be in both:
//! - the cached [`Chain`] or [`NonFinalizedState`], and
//! - the shared finalized [`ZebraDb`] reference.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block, Height},
    block_info::BlockInfo,
    serialization::ZcashSerialize as _,
    transaction::{self, Transaction},
    transparent::{self, Utxo},
};

use crate::{
    request::{BlockField, BlockQuery, ChainSelector, TransactionQuery, UtxoQuery},
    response::{AnyTx, MinedTx, QueriedBlock},
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::{
            find::{hash_by_height, height_by_hash},
            tip,
            tree::{ironwood_tree, orchard_tree, sapling_tree},
        },
    },
    BoxError, ContextuallyVerifiedBlock, HashOrHeight,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chains` or finalized `db`.
pub fn any_block<'a, C: AsRef<Chain> + 'a>(
    mut chains: impl Iterator<Item = &'a C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<Block>> {
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chains
        .find_map(|c| c.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub fn block<C>(chain: Option<C>, db: &ZebraDb, hash_or_height: HashOrHeight) -> Option<Arc<Block>>
where
    C: AsRef<Chain>,
{
    any_block(chain.iter(), db, hash_or_height)
}

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub fn block_and_size<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<(Arc<Block>, usize)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| {
            let size = contextual.block.zcash_serialize_to_vec().unwrap().len();
            (contextual.block.clone(), size)
        })
        .or_else(|| db.block_and_size(hash_or_height))
}

/// Returns the [`block::Header`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub fn block_header<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<block::Header>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.header.clone())
        .or_else(|| db.block_header(hash_or_height))
}

/// Returns the [`Transaction`] with [`transaction::Hash`], if it exists in the
/// non-finalized `chain` or finalized `db`.
fn transaction<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<(Arc<Transaction>, Height, DateTime<Utc>)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since transactions are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first. (`chain` is always in
    // memory, but `db` stores transactions on disk, with a memory cache.)
    chain
        .and_then(|chain| {
            chain
                .as_ref()
                .transaction(hash)
                .map(|(tx, height, time)| (tx.clone(), height, time))
        })
        .or_else(|| db.transaction(hash))
}

/// Returns a [`MinedTx`] for a [`Transaction`] with [`transaction::Hash`],
/// if one exists in the non-finalized `chain` or finalized `db`.
pub fn mined_transaction<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<MinedTx>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // It is ok to do this lookup in two different calls. Finalized state updates
    // can only add overlapping blocks, and hashes are unique.
    let chain = chain.as_ref();

    let (tx, height, time) = transaction(chain, db, hash)?;
    let (tip_height, tip_hash) = tip(chain, db)?;
    let confirmations = 1 + tip_height.0 - height.0;

    Some(MinedTx::new(tx, height, confirmations, time, tip_hash))
}

/// Returns a [`AnyTx`] for a [`Transaction`] with [`transaction::Hash`],
/// if one exists in any chain in `chains` or finalized `db`.
/// The first chain in `chains` must be the best chain.
pub fn any_transaction<'a>(
    chains: impl Iterator<Item = &'a Arc<Chain>>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<AnyTx> {
    // # Correctness
    //
    // It is ok to do this lookup in multiple different calls. Finalized state updates
    // can only add overlapping blocks, and hashes are unique.
    //
    // Capture the best chain tip before searching, not inside the search closure.
    // The closure only runs when the tx is found in a non-finalized chain; if the tx
    // is only in the finalized DB, the closure never fires and best_chain would stay
    // None, causing tip_height to undercount confirmations by ~MAX_BLOCK_REORG_HEIGHT.
    // See <https://github.com/ZcashFoundation/zebra/issues/10470>.
    // peekable() reads the first element without consuming it, so the iterator can
    // still be used in find_map below.
    let mut chains = chains.peekable();
    let best_chain = chains.peek().copied();
    let (tx, height, time, in_best_chain, containing_chain) = chains
        .enumerate()
        .find_map(|(i, chain)| {
            chain
                .as_ref()
                .transaction(hash)
                .map(|(tx, height, time)| (tx.clone(), height, time, i == 0, Some(chain)))
        })
        .or_else(|| {
            db.transaction(hash)
                .map(|(tx, height, time)| (tx.clone(), height, time, true, None))
        })?;

    if in_best_chain {
        let (tip_height, tip_hash) = tip(best_chain, db)?;
        let confirmations = 1 + tip_height.0 - height.0;
        Some(AnyTx::Mined(MinedTx::new(
            tx,
            height,
            confirmations,
            time,
            tip_hash,
        )))
    } else {
        let block_hash = containing_chain?.block(height.into())?.hash;
        Some(AnyTx::Side((tx, block_hash)))
    }
}

/// Returns the [`transaction::Hash`]es for the block with `hash_or_height`,
/// if it exists in the non-finalized `chain` or finalized `db`.
///
/// The returned hashes are in block order.
///
/// Returns `None` if the block is not found.
pub fn transaction_hashes_for_block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<[transaction::Hash]>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().transaction_hashes_for_block(hash_or_height))
        .or_else(|| db.transaction_hashes_for_block(hash_or_height))
}

/// Returns the [`transaction::Hash`]es for the block with `hash_or_height`,
/// if it exists in any chain in `chains` or finalized `db`.
/// The first chain in `chains` must be the best chain.
///
/// The returned hashes are in block order.
///
/// Returns `None` if the block is not found.
pub fn transaction_hashes_for_any_block<'a>(
    chains: impl Iterator<Item = &'a Arc<Chain>>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<(Arc<[transaction::Hash]>, bool)> {
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chains
        .enumerate()
        .find_map(|(i, chain)| {
            chain
                .as_ref()
                .transaction_hashes_for_block(hash_or_height)
                .map(|hashes| (hashes.clone(), i == 0))
        })
        .or_else(|| {
            db.transaction_hashes_for_block(hash_or_height)
                .map(|hashes| (hashes, true))
        })
}

/// Returns the [`Utxo`] for [`transparent::OutPoint`], if it exists in the
/// non-finalized `chain` or finalized `db`.
///
/// Non-finalized UTXOs are returned regardless of whether they have been spent.
///
/// Finalized UTXOs are only returned if they are unspent in the finalized chain.
/// They may have been spent in the non-finalized chain,
/// but this function returns them without checking for non-finalized spends,
/// because we don't know which non-finalized chain will be committed to the finalized state.
pub fn utxo<C>(chain: Option<C>, db: &ZebraDb, outpoint: transparent::OutPoint) -> Option<Utxo>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since UTXOs are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first. (`chain` is always in
    // memory, but `db` stores transactions on disk, with a memory cache.)
    chain
        .and_then(|chain| chain.as_ref().created_utxo(&outpoint))
        .or_else(|| db.utxo(&outpoint).map(|utxo| utxo.utxo))
}

/// Returns the [`Utxo`] for [`transparent::OutPoint`], if it exists and is unspent in the
/// non-finalized `chain` or finalized `db`.
pub fn unspent_utxo<C>(
    chain: Option<C>,
    db: &ZebraDb,
    outpoint: transparent::OutPoint,
) -> Option<Utxo>
where
    C: AsRef<Chain>,
{
    match chain {
        Some(chain) if chain.as_ref().spent_utxos.contains_key(&outpoint) => None,
        chain => utxo(chain, db, outpoint),
    }
}

/// Returns the [`Hash`](transaction::Hash) of the transaction that spent an output at
/// the provided [`transparent::OutPoint`] or revealed the provided nullifier, if it exists
/// and is spent or revealed in the non-finalized `chain` or finalized `db` and its
/// spending transaction hash has been indexed.
#[cfg(feature = "indexer")]
pub fn spending_transaction_hash<C>(
    chain: Option<C>,
    db: &ZebraDb,
    spend: Spend,
) -> Option<transaction::Hash>
where
    C: AsRef<Chain>,
{
    chain
        .and_then(|chain| chain.as_ref().spending_transaction_hash(&spend))
        .or_else(|| db.spending_transaction_hash(&spend))
}

/// Returns the [`Utxo`] for [`transparent::OutPoint`], if it exists in any chain
/// in the `non_finalized_state`, or in the finalized `db`.
///
/// Non-finalized UTXOs are returned regardless of whether they have been spent.
///
/// Finalized UTXOs are only returned if they are unspent in the finalized chain.
/// They may have been spent in one or more non-finalized chains,
/// but this function returns them without checking for non-finalized spends,
/// because we don't know which non-finalized chain the request belongs to.
///
/// UTXO spends are checked once the block reaches the non-finalized state,
/// by [`check::utxo::transparent_spend()`](crate::service::check::utxo::transparent_spend).
pub fn any_utxo(
    non_finalized_state: NonFinalizedState,
    db: &ZebraDb,
    outpoint: transparent::OutPoint,
) -> Option<Utxo> {
    // # Correctness
    //
    // Since UTXOs are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first. (`non_finalized_state` is always in
    // memory, but `db` stores transactions on disk, with a memory cache.)
    non_finalized_state
        .any_utxo(&outpoint)
        .or_else(|| db.utxo(&outpoint).map(|utxo| utxo.utxo))
}

/// Returns the [`BlockInfo`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub fn block_info<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<BlockInfo>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block_info(hash_or_height))
        .or_else(|| db.block_info(hash_or_height))
}

/// Returns the [`QueriedBlock`] with the requested [`BlockField`]s of the block
/// with `hash_or_height`, if it exists in the selected chain(s).
///
/// Only the requested fields are read: fields that are stored separately from
/// the block's full transaction data are read without deserializing the whole
/// block, and the full block is only read when [`BlockField::Block`] or
/// [`BlockField::AuthDataRoot`] is requested (once, even if both are requested).
///
/// With [`ChainSelector::Best`], all fields are available, resolved against the
/// best chain. With [`ChainSelector::Any`], only a subset of fields is available
/// (see [`BlockQuery`]); requesting any other field is an error.
pub fn queried_block(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    query: BlockQuery,
) -> Result<Option<QueriedBlock>, BoxError> {
    let BlockQuery {
        hash_or_height,
        chain,
        fields,
    } = query;

    match chain {
        ChainSelector::Best => Ok(queried_best_chain_block(
            non_finalized_state.best_chain(),
            db,
            hash_or_height,
            &fields,
        )),

        ChainSelector::Any => {
            queried_any_chain_block(non_finalized_state, db, hash_or_height, &fields)
        }
    }
}

/// Returns the [`QueriedBlock`] with the requested [`BlockField`]s of the block
/// with `hash_or_height` in the best non-finalized `chain` or finalized `db`,
/// or `None` if the block was not found.
fn queried_best_chain_block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
    fields: &std::collections::HashSet<BlockField>,
) -> Option<QueriedBlock>
where
    C: AsRef<Chain>,
{
    // `Option<&C>` is `Copy`, and `&C: AsRef<Chain>` forwards to `C: AsRef<Chain>`,
    // so this can be passed to every field lookup below.
    let chain = chain.as_ref();

    // # Correctness
    //
    // Resolve the block's hash and height before reading any fields: every field
    // is keyed by one of them, and this also checks that the block is in the
    // chain at all. Resolving both up front also means every field below is read
    // for the same block, even if `hash_or_height` is a height and a reorg
    // happens between the field reads.
    let height = hash_or_height.height_or_else(|hash| height_by_hash(chain, db, hash))?;
    let hash = hash_or_height.hash_or_else(|height| hash_by_height(chain, db, height))?;

    let mut queried_block = QueriedBlock::default();

    if fields.contains(&BlockField::Hash) {
        queried_block.hash = Some(hash);
    }

    if fields.contains(&BlockField::Height) {
        queried_block.height = Some(height);
    }

    if fields.contains(&BlockField::Header) {
        queried_block.header = block_header(chain, db, hash.into());
    }

    if fields.contains(&BlockField::NextBlockHash) {
        queried_block.next_block_hash = height
            .next()
            .ok()
            .and_then(|next_height| hash_by_height(chain, db, next_height));
    }

    if fields.contains(&BlockField::Confirmations) {
        queried_block.confirmations =
            tip(chain, db).map(|(tip_height, _tip_hash)| 1 + tip_height.0 - height.0);
    }

    if fields.contains(&BlockField::TransactionIds) {
        queried_block.transaction_ids = transaction_hashes_for_block(chain, db, hash.into());
    }

    if fields.contains(&BlockField::BlockInfo) {
        queried_block.block_info = block_info(chain, db, hash.into());
    }

    if fields.contains(&BlockField::SaplingTree) {
        queried_block.sapling_tree = sapling_tree(chain, db, hash.into());
    }

    if fields.contains(&BlockField::OrchardTree) {
        queried_block.orchard_tree = orchard_tree(chain, db, hash.into());
    }

    if fields.contains(&BlockField::IronwoodTree) {
        queried_block.ironwood_tree = ironwood_tree(chain, db, hash.into());
    }

    // # Performance
    //
    // Read the full block data from disk at most once, even if multiple fields
    // that need it were requested.
    if fields.contains(&BlockField::Block) || fields.contains(&BlockField::AuthDataRoot) {
        if let Some(full_block) = block(chain, db, hash.into()) {
            if fields.contains(&BlockField::AuthDataRoot) {
                queried_block.auth_data_root = Some(full_block.auth_data_root());
            }

            if fields.contains(&BlockField::Block) {
                queried_block.block = Some(full_block);
            }
        }
    }

    Some(queried_block)
}

/// Returns the [`QueriedBlock`] with the requested [`BlockField`]s of the block
/// with `hash_or_height` in any chain in `non_finalized_state`, or in the finalized `db`.
///
/// Returns an error if `fields` contains fields that only have meaning on the
/// best chain; see [`BlockQuery`] for the fields available with
/// [`ChainSelector::Any`].
fn queried_any_chain_block(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
    fields: &std::collections::HashSet<BlockField>,
) -> Result<Option<QueriedBlock>, BoxError> {
    const ANY_CHAIN_FIELDS: &[BlockField] = &[
        BlockField::Hash,
        BlockField::Height,
        BlockField::Header,
        BlockField::Block,
        BlockField::TransactionIds,
    ];

    if let Some(unsupported) = fields
        .iter()
        .find(|field| !ANY_CHAIN_FIELDS.contains(field))
    {
        return Err(format!(
            "BlockField::{unsupported:?} is not available with ChainSelector::Any"
        )
        .into());
    }

    // Find the block, checking the best chain first, then side chains in order
    // from most to least work, then the finalized state. Blocks in the finalized
    // state are always on the best chain.
    let mut contextual: Option<(ContextuallyVerifiedBlock, bool)> = None;

    for (index, chain) in non_finalized_state.chain_iter().enumerate() {
        if let Some(block) = chain.block(hash_or_height) {
            contextual = Some((block.clone(), index == 0));
            break;
        }
    }

    let (hash, height, contextual, in_best_chain) = match contextual {
        Some((contextual, in_best_chain)) => {
            let (hash, height) = (contextual.hash, contextual.height);
            (hash, height, Some(contextual), in_best_chain)
        }
        None => {
            let Some(height) = hash_or_height.height_or_else(|hash| db.height(hash)) else {
                return Ok(None);
            };
            let Some(hash) = hash_or_height.hash_or_else(|height| db.hash(height)) else {
                return Ok(None);
            };
            (hash, height, None, true)
        }
    };

    let mut queried_block = QueriedBlock {
        in_best_chain: Some(in_best_chain),
        ..QueriedBlock::default()
    };

    if fields.contains(&BlockField::Hash) {
        queried_block.hash = Some(hash);
    }

    if fields.contains(&BlockField::Height) {
        queried_block.height = Some(height);
    }

    if fields.contains(&BlockField::Header) {
        queried_block.header = contextual
            .as_ref()
            .map(|contextual| contextual.block.header.clone())
            .or_else(|| db.block_header(hash.into()));
    }

    if fields.contains(&BlockField::TransactionIds) {
        queried_block.transaction_ids = contextual
            .as_ref()
            .map(|contextual| contextual.transaction_hashes.clone())
            .or_else(|| db.transaction_hashes_for_block(hash.into()));
    }

    if fields.contains(&BlockField::Block) {
        queried_block.block = contextual
            .map(|contextual| contextual.block.clone())
            .or_else(|| db.block(hash.into()));
    }

    Ok(Some(queried_block))
}

/// Returns the [`AnyTx`] for the transaction with `hash` in the selected chain(s),
/// if it exists in the non-finalized state or finalized `db`.
///
/// With [`ChainSelector::Best`], the result is always [`AnyTx::Mined`] when found.
pub fn transaction_query(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    query: TransactionQuery,
) -> Option<AnyTx> {
    let TransactionQuery { hash, chain } = query;

    match chain {
        ChainSelector::Best => {
            mined_transaction(non_finalized_state.best_chain(), db, hash).map(AnyTx::Mined)
        }
        ChainSelector::Any => any_transaction(non_finalized_state.chain_iter(), db, hash),
    }
}

/// Returns the [`Utxo`] for the outpoint in the selected chain(s), if it exists
/// in the non-finalized state or finalized `db`.
///
/// See [`UtxoQuery`] for the exact semantics of each chain selector.
pub fn utxo_query(
    non_finalized_state: &NonFinalizedState,
    db: &ZebraDb,
    query: UtxoQuery,
) -> Option<Utxo> {
    let UtxoQuery { outpoint, chain } = query;

    match chain {
        ChainSelector::Best => unspent_utxo(non_finalized_state.best_chain(), db, outpoint),
        ChainSelector::Any => any_utxo(non_finalized_state.clone(), db, outpoint),
    }
}
