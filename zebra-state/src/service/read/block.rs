//! Shared block, header, and transaction reading code.
//!
//! In the functions in this module:
//!
//! Before removing a root from the published non-finalized snapshot, the block
//! writer commits it to the finalized database. Further blocks can be finalized
//! after we've cloned `chain` or `non_finalized_state`.
//!
//! This means that some blocks can be in both:
//! - the cached [`Chain`] or [`NonFinalizedState`], and
//! - the shared finalized [`ZebraDb`] reference.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Utc};

use zebra_chain::{
    block::{self, Block, Height},
    block_info::BlockInfo,
    serialization::ZcashSerialize as _,
    transaction::{self, Transaction},
    transparent::{self, Utxo},
};

use crate::{
    response::{AnyTx, MinedTx, OutputWithMetadata},
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::tip,
    },
    BlockReadError, BoxError, HashOrHeight,
};

#[cfg(feature = "indexer")]
use crate::request::Spend;

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chains` or finalized `db`.
///
/// Returns an error if primary data is incomplete. An incomplete secondary view returns `None`.
pub fn any_block<'a, C: AsRef<Chain> + 'a>(
    mut chains: impl Iterator<Item = &'a C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Result<Option<Arc<Block>>, BlockReadError> {
    // # Correctness
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chains
        .find_map(|c| c.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .map_or_else(|| db.block(hash_or_height), |block| Ok(Some(block)))
}

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
///
/// Returns an error if primary data is incomplete. An incomplete secondary view returns `None`.
pub fn block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Result<Option<Arc<Block>>, BlockReadError>
where
    C: AsRef<Chain>,
{
    any_block(chain.iter(), db, hash_or_height)
}

/// Returns the [`Block`] with [`block::Hash`] or
/// [`Height`], if it exists in the non-finalized `chain` or finalized `db`.
///
/// Returns an error if primary data is incomplete. An incomplete secondary view returns `None`.
pub fn block_and_size<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Result<Option<(Arc<Block>, usize)>, BlockReadError>
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
        .map_or_else(
            || db.block_and_size(hash_or_height),
            |block| Ok(Some(block)),
        )
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

/// Returns whether the transaction is in the best chain, even if its body is unavailable.
pub fn known_transaction<C>(chain: Option<C>, db: &ZebraDb, hash: transaction::Hash) -> bool
where
    C: AsRef<Chain>,
{
    chain.is_some_and(|chain| chain.as_ref().tx_loc_by_hash.contains_key(&hash))
        || db.transaction_location(hash).is_some()
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

/// Returns the transparent outputs spent by the non-coinbase inputs of the block with
/// `hash_or_height`, keyed by the [`OutPoint`](transparent::OutPoint) that spends them, if the
/// block exists in the non-finalized `chain` or finalized `db`.
///
/// Used by the `getblock` RPC at verbosity 3 to fill in each input's prevout and the
/// transaction fee.
///
/// Returns an error if primary data is incomplete. An incomplete secondary view returns `None`.
///
/// # Correctness
///
/// The block and all of its spent outputs are read from the same `chain`/`db` snapshot, so the
/// prevouts are consistent with the block.
///
/// A spent output that cannot be found (for example, an input of a side-chain block that spends
/// an output not in the best chain) is omitted from the map. Callers must treat a missing
/// outpoint as "prevout unknown", not as an error.
///
/// Coinbase transactions spend no outputs, so they contribute nothing to the map.
///
/// # Performance
///
/// Spent outputs are deleted from the finalized UTXO set, so an output already spent in the
/// finalized chain is recovered by reading its whole parent transaction. Each distinct parent
/// transaction is read at most once, so this is `O(distinct spent parent transactions)` reads —
/// the same cost as resolving the prevouts individually, but in a single state request under one
/// snapshot.
///
/// UTXO lookups only help for a non-finalized block: its spent outputs are still in the
/// non-finalized chain's created UTXOs or the finalized UTXO set. A finalized block skips them
/// and reads the parent transactions directly.
pub fn spent_outputs_for_block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Result<Option<HashMap<transparent::OutPoint, Utxo>>, BlockReadError>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();
    let Some(block) = block(chain, db, hash_or_height)? else {
        return Ok(None);
    };

    // The finalized tip can only advance between these reads, which can only turn a UTXO hit
    // into a parent read, never the reverse.
    let is_finalized = match (block.coinbase_height(), db.finalized_tip_height()) {
        (Some(height), Some(finalized_tip)) => height <= finalized_tip,
        _ => false,
    };

    let mut spent_outputs = HashMap::new();

    // Outpoints missing from the UTXO set, grouped by parent so each parent is read once,
    // used, and dropped before the next one is read.
    let mut by_parent: HashMap<transaction::Hash, Vec<transparent::OutPoint>> = HashMap::new();

    for tx in block.transactions.iter().filter(|tx| !tx.is_coinbase()) {
        for outpoint in tx.spent_outpoints() {
            let unspent = if is_finalized {
                None
            } else {
                utxo(chain, db, outpoint)
            };
            if let Some(utxo) = unspent {
                spent_outputs.insert(outpoint, utxo);
            } else {
                by_parent.entry(outpoint.hash).or_default().push(outpoint);
            }
        }
    }

    for (parent_hash, outpoints) in by_parent {
        let Some((parent_tx, height, _)) = transaction(chain, db, parent_hash) else {
            continue;
        };
        let from_coinbase = parent_tx.is_coinbase();
        let outputs = parent_tx.outputs();
        for outpoint in outpoints {
            if let Some(output) = outputs.get(outpoint.index as usize) {
                spent_outputs.insert(outpoint, Utxo::new(output.clone(), height, from_coinbase));
            }
        }
    }

    Ok(Some(spent_outputs))
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

/// Returns an unspent output and metadata from one best-chain view.
///
/// Finalized tip changes invalidate the read. A cached chain must agree with the finalized
/// overlap; otherwise a spend on another fork could be combined with the cached tip.
pub fn unspent_output<C>(
    chain: Option<C>,
    db: &ZebraDb,
    outpoint: transparent::OutPoint,
) -> Result<Option<OutputWithMetadata>, BoxError>
where
    C: AsRef<Chain>,
{
    let chain = chain.as_ref();
    for _ in 0..=super::FINALIZED_STATE_QUERY_RETRIES {
        let finalized_tip = db.tip();
        let chain = chain.filter(|chain| {
            finalized_tip
                .is_none_or(|(height, _)| height < chain.as_ref().non_finalized_tip_height())
        });
        if let (Some(chain), Some((height, hash))) = (chain, finalized_tip) {
            let chain = chain.as_ref();
            let root = chain.non_finalized_root_height();
            let matches = if height < root {
                height.next().ok() == Some(root)
                    && chain.blocks[&root].block.header.previous_block_hash == hash
            } else {
                chain.hash_by_height(height) == Some(hash)
            };
            if !matches {
                return Err(
                    "unable to get output: finalized and non-finalized chains disagree".into(),
                );
            }
        }

        let tip = chain
            .map(|chain| chain.as_ref().non_finalized_tip())
            .or(finalized_tip);
        #[cfg(test)]
        super::tests::unspent_output::AFTER_TIP_CAPTURE.with(|hook| {
            if let Some(commit) = hook.borrow_mut().take() {
                commit();
            }
        });
        let output = unspent_utxo(chain, db, outpoint);
        let result = match (output, tip) {
            (Some(utxo), Some((height, best_chain_tip_hash))) => {
                let Some(confirmations) = height
                    .0
                    .checked_sub(utxo.height.0)
                    .and_then(|depth| depth.checked_add(1))
                else {
                    // A newly committed output can be above the tip captured before its lookup.
                    if finalized_tip != db.tip() {
                        continue;
                    }
                    return Err("output is above the captured best-chain tip".into());
                };
                let transaction_version = chain
                    .and_then(|chain| {
                        chain
                            .as_ref()
                            .transaction(outpoint.hash)
                            .map(|(tx, ..)| tx.version())
                    })
                    .or_else(|| db.transaction_version(outpoint.hash));
                if transaction_version.is_none()
                    && utxo.height >= db.history_capabilities()?.lowest_retained_height
                {
                    return Err(BlockReadError::CorruptBlock {
                        height: utxo.height,
                    }
                    .into());
                }
                Some(OutputWithMetadata {
                    utxo,
                    transaction_version,
                    confirmations,
                    best_chain_tip_hash,
                })
            }
            _ => None,
        };
        if finalized_tip == db.tip() {
            return Ok(result);
        }
    }
    Err("unable to get output: finalized tip changed during the query".into())
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
