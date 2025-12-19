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
    block::{self, merkle::AuthDataRoot, Block, Height},
    block_info::BlockInfo,
    serialization::ZcashSerialize as _,
    transaction::{self, Transaction},
    transparent::{self, Utxo},
};

use crate::{
    response::{AnyTx, MinedTx},
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::tip_height,
    },
    HashOrHeight,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
pub fn block<C>(chain: Option<C>, db: &ZebraDb, hash_or_height: HashOrHeight) -> Option<Arc<Block>>
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
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
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
    let confirmations = 1 + tip_height(chain, db)?.0 - height.0;

    Some(MinedTx::new(tx, height, confirmations, time))
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
    let mut best_chain = None;
    let (tx, height, time, in_best_chain, containing_chain) = chains
        .enumerate()
        .find_map(|(i, chain)| {
            chain.as_ref().transaction(hash).map(|(tx, height, time)| {
                if i == 0 {
                    best_chain = Some(chain);
                }
                (tx.clone(), height, time, i == 0, Some(chain))
            })
        })
        .or_else(|| {
            db.transaction(hash)
                .map(|(tx, height, time)| (tx.clone(), height, time, true, None))
        })?;

    if in_best_chain {
        let confirmations = 1 + tip_height(best_chain, db)?.0 - height.0;
        Some(AnyTx::Mined(MinedTx::new(tx, height, confirmations, time)))
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

pub fn auth_data_root<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<AuthDataRoot>
where
    C: AsRef<Chain>,
{
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().auth_data_root(hash_or_height))
        .or_else(|| db.auth_data_root(hash_or_height))
}
