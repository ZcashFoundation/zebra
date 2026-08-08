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
    block::{self, Block, Height, SyncHashEntry, TreeRootsEntry},
    block_info::BlockInfo,
    serialization::ZcashSerialize as _,
    transaction::{self, Transaction},
    transparent::{self, Utxo},
};

use crate::{
    response::{AnyTx, MinedTx},
    service::finalized_state::disk_format::chain::BLOCK_SIZE_VALUE_UNIT,
    service::{
        finalized_state::ZebraDb,
        non_finalized_state::{Chain, NonFinalizedState},
        read::tip,
    },
    HashOrHeight,
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

/// Returns best-chain block hashes and aggregated span metadata at a height
/// stride, for the v2 network protocol's `get-hashes` requests.
///
/// Entries stop at [`MAX_BLOCK_REORG_HEIGHT`] blocks below the best chain
/// tip, so every served height is finalized, and truncation is always a
/// prefix. Entries also stop where the synchronization metadata backfill
/// has not yet reached.
///
/// [`MAX_BLOCK_REORG_HEIGHT`]: crate::constants::MAX_BLOCK_REORG_HEIGHT
pub fn sync_hashes<C: AsRef<Chain>>(
    chain: Option<C>,
    db: &ZebraDb,
    start_height: u32,
    stride: u32,
    count: u32,
) -> Vec<SyncHashEntry> {
    let best_tip = chain
        .map(|chain| chain.as_ref().non_finalized_tip_height())
        .or_else(|| db.finalized_tip_height());
    let Some(best_tip) = best_tip else {
        return Vec::new();
    };
    let max_served_height = u64::from(
        best_tip
            .0
            .saturating_sub(crate::constants::MAX_BLOCK_REORG_HEIGHT),
    );

    // The requested entries' spans are contiguous, so the whole request is
    // one range of the metadata index: read it once, rather than once per
    // entry.
    //
    // The widening makes the height arithmetic infallible; served heights
    // fit `u32` because the tip height does.
    let last_height =
        u64::from(start_height) + u64::from(count.saturating_sub(1)) * u64::from(stride);
    let last_height = last_height.min(max_served_height);
    if u64::from(start_height) > last_height {
        return Vec::new();
    }
    let lowest_span_height = start_height.saturating_sub(stride.saturating_sub(1));
    let metadata = db.sync_metadata_map(Height(lowest_span_height)..=Height(last_height as u32));

    let mut entries = Vec::new();
    for k in 0..u64::from(count) {
        let height = u64::from(start_height) + k * u64::from(stride);
        if height > max_served_height {
            break;
        }
        let height = Height(height as u32);

        let Some(hash) = db.hash(height) else {
            break;
        };

        // The entry's span: the blocks above the previous entry's height,
        // up to its own, bounded below by the genesis height.
        let span_low = height.0.saturating_sub(stride.saturating_sub(1));

        let mut entry = SyncHashEntry {
            hash,
            span_size: 0,
            span_txs: 0,
            span_notes: 0,
        };
        let mut backfilled = true;
        for span_height in span_low..=height.0 {
            let Some(meta) = metadata.get(&Height(span_height)) else {
                // The backfill has not reached this span yet; truncate.
                backfilled = false;
                break;
            };

            // The size value is the network protocol's quantization of the
            // block's serialized size: a 1-byte quantity whose product with
            // the unit bounds the block's size from above.
            let size_value = u64::from(meta.size)
                .div_ceil(BLOCK_SIZE_VALUE_UNIT)
                .clamp(1, 255);
            entry.span_size += size_value;
            entry.span_txs += u64::from(meta.tx_count);
            entry.span_notes += u64::from(meta.note_count);
        }
        if !backfilled {
            break;
        }

        entries.push(entry);
    }

    entries
}

/// Returns per-block note commitment tree roots, ZIP 221 per-pool
/// transaction counts, and ZIP 244 authorizing data commitments for the
/// blocks at `start_height..start_height + count`, for the v2 network
/// protocol's `get-tree-roots` requests.
///
/// Returns `None` — the request must be refused — when the best chain does
/// not contain the block with hash `final_hash` at the highest requested
/// height, or the synchronization metadata index does not cover the range.
pub fn tree_roots(
    db: &ZebraDb,
    start_height: u32,
    final_hash: block::Hash,
    count: u32,
) -> Option<Vec<TreeRootsEntry>> {
    use zebra_chain::parameters::NetworkUpgrade;

    if count == 0 {
        return None;
    }
    let final_height = u64::from(start_height) + u64::from(count) - 1;
    let final_height = Height(u32::try_from(final_height).ok()?);

    // The anchor identifies the chain: entries are only served when the
    // best (finalized) chain contains the anchor block at the final height,
    // so an honest responder never serves entries for different blocks.
    if db.height(final_hash) != Some(final_height) {
        return None;
    }

    let network = db.network();
    let activation = |upgrade: NetworkUpgrade| upgrade.activation_height(&network);
    let sapling_activation = activation(NetworkUpgrade::Sapling);
    let orchard_activation = activation(NetworkUpgrade::Nu5);
    let ironwood_activation = activation(NetworkUpgrade::Nu6_3);
    let is_active = |activation: Option<Height>, height: Height| {
        activation.is_some_and(|activation| height >= activation)
    };

    let mut entries = Vec::with_capacity(count as usize);
    for height in start_height..=final_height.0 {
        let height = Height(height);
        let meta = db.sync_metadata(height)?;

        // For a pool that is not active at this height, the root is 32 zero
        // bytes; sync metadata provides the matching zero counts.
        let sapling_root: [u8; 32] = if is_active(sapling_activation, height) {
            db.sapling_tree_by_height(&height)?.root().into()
        } else {
            [0; 32]
        };
        let orchard_root: [u8; 32] = if is_active(orchard_activation, height) {
            db.orchard_tree_by_height(&height)?.root().into()
        } else {
            [0; 32]
        };
        let ironwood_root: [u8; 32] = if is_active(ironwood_activation, height) {
            db.ironwood_tree_by_height(&height)?.root().into()
        } else {
            [0; 32]
        };

        entries.push(TreeRootsEntry {
            sapling_root,
            orchard_root,
            ironwood_root,
            sapling_txs: u64::from(meta.sapling_tx_count),
            orchard_txs: u64::from(meta.orchard_tx_count),
            ironwood_txs: u64::from(meta.ironwood_tx_count),
            auth_data_root: meta.auth_data_root,
        });
    }

    Some(entries)
}
