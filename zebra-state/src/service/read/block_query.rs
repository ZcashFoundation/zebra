//! Reads any combination of a block's parts in one request, for [`ReadRequest::BlockQuery`].
//!
//! [`ReadRequest::BlockQuery`]: crate::ReadRequest::BlockQuery

use std::sync::Arc;

use zebra_chain::{
    block::{self, Block, Height, SerializedBlock},
    block_info::BlockInfo,
    orchard, sapling,
    serialization::{CompactSizeMessage, ZcashSerialize as _},
    transaction::{self, Transaction},
};

use crate::{
    service::{
        finalized_state::{FromDisk, RawBytes, ZebraDb},
        non_finalized_state::Chain,
        read::{block::spent_outputs_for, find::tip_height},
    },
    BlockQueryFields, BlockReadError, ContextuallyVerifiedBlock, HashOrHeight, HistoryCapabilities,
    QueriedBlock, StateInitError,
};

#[cfg(test)]
thread_local! {
    /// The number of [`ZebraDb::history_capabilities`] reads `block_query` made on this thread.
    pub(crate) static HISTORY_CAPABILITIES_READS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// The parts that need the block body.
const BODY: BlockQueryFields = BlockQueryFields::RAW_BLOCK
    .union(BlockQueryFields::TRANSACTIONS)
    .union(BlockQueryFields::SPENT_OUTPUTS)
    .union(BlockQueryFields::AUTH_DATA_ROOT);

/// The parts that need the deserialized transactions.
const TRANSACTIONS: BlockQueryFields = BlockQueryFields::TRANSACTIONS
    .union(BlockQueryFields::SPENT_OUTPUTS)
    .union(BlockQueryFields::AUTH_DATA_ROOT);

/// Returns the `fields` of the best-chain block at `hash_or_height`, or `None` if the block is
/// unknown.
///
/// `chain` is the best chain sampled once for the whole request, so every part is read from one
/// chain snapshot. Unrequested parts are `None`.
///
/// Returns [`BlockReadError::BlockBodyUnavailable`] when a requested part needs a pruned body, and
/// [`BlockReadError::CorruptBlock`] when retained rows of a known block are missing on a primary.
/// Torn secondary views return `None` parts instead.
pub fn block_query(
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
    fields: BlockQueryFields,
) -> Result<Option<QueriedBlock>, BlockReadError> {
    let chain = chain.as_ref();
    let Some(resolved) = resolve(chain, db, hash_or_height)? else {
        return Ok(None);
    };
    let Resolved {
        height,
        hash,
        cached,
    } = resolved;

    let parts = match cached {
        Some(cached) => {
            let chain = chain.expect("a block is only cached when a chain was sampled");
            cached_parts(chain, cached, fields)
        }
        None => finalized_parts(db, height, fields)?,
    };

    let previous_block_info = fields
        .contains(BlockQueryFields::PREVIOUS_BLOCK_INFO)
        .then(|| height.previous().ok())
        .flatten()
        .and_then(|parent| match cached.and(chain) {
            Some(chain) if parent >= chain.non_finalized_root_height() => {
                chain.block_info(parent.into())
            }
            // The parent of a cached chain's root is the finalized tip.
            _ => db.block_info(parent.into()),
        });

    let spent_outputs = match &parts.block {
        Some(block) if fields.contains(BlockQueryFields::SPENT_OUTPUTS) => {
            spent_outputs_for(block, chain, db)?
        }
        _ => None,
    };

    // `auth_data_root()` hashes transactions in parallel on rayon for large blocks, which is
    // fine here because every read request runs on a blocking thread.
    let auth_data_root = parts
        .block
        .as_ref()
        .filter(|_| fields.contains(BlockQueryFields::AUTH_DATA_ROOT))
        .map(|block| block.auth_data_root());

    Ok(Some(QueriedBlock {
        hash,
        height,
        header: parts.header,
        depth: fields
            .contains(BlockQueryFields::DEPTH)
            .then(|| depth(chain, db, height))
            .flatten(),
        next_block_hash: fields
            .contains(BlockQueryFields::NEXT_BLOCK_HASH)
            .then(|| next_block_hash(chain, db, &resolved))
            .flatten(),
        transaction_count: parts.transaction_count,
        transaction_ids: parts.transaction_ids,
        block_info: parts.block_info,
        previous_block_info,
        sapling_tree: parts.sapling_tree,
        orchard_tree: parts.orchard_tree,
        ironwood_tree: parts.ironwood_tree,
        raw_block: parts.raw_block,
        block: parts.block,
        size: parts.size,
        spent_outputs,
        auth_data_root,
    }))
}

/// A best-chain block resolved under the request's chain snapshot.
#[derive(Clone, Copy)]
struct Resolved<'a> {
    height: Height,
    hash: block::Hash,
    /// The block, if it was resolved in the sampled chain. Its parts are then read from that
    /// chain, and otherwise from the finalized state.
    cached: Option<&'a ContextuallyVerifiedBlock>,
}

/// Resolves `hash_or_height` once, in the sampled `chain` and then in the finalized `db`.
///
/// Returns `None` for an unknown block. A height at or below the tip that has no hash row is
/// below a snapshot-born node's known history if [`below_known_history`] says so, and then
/// returns [`BlockReadError::BlockBodyUnavailable`].
///
/// # Panics
///
/// If the history capabilities are needed and could not be read from the database.
fn resolve<'a>(
    chain: Option<&'a Arc<Chain>>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Result<Option<Resolved<'a>>, BlockReadError> {
    if let Some(cached) = chain.and_then(|chain| chain.block(hash_or_height)) {
        return Ok(Some(Resolved {
            height: cached.height,
            hash: cached.hash,
            cached: Some(cached),
        }));
    }

    let finalized = match hash_or_height {
        HashOrHeight::Hash(hash) => db.height(hash).map(|height| (height, hash)),
        HashOrHeight::Height(height) => db.hash(height).map(|hash| (height, hash)),
    };
    if let Some((height, hash)) = finalized {
        return Ok(Some(Resolved {
            height,
            hash,
            cached: None,
        }));
    }

    // A missing hash is unknown: it can be below the known history, on a side chain, or not yet
    // mined. Heights above the tip are not mined yet, so they skip the history read.
    let HashOrHeight::Height(height) = hash_or_height else {
        return Ok(None);
    };
    if tip_height(chain, db).is_none_or(|tip| height > tip) {
        return Ok(None);
    }

    #[cfg(test)]
    HISTORY_CAPABILITIES_READS.with(|reads| reads.set(reads.get() + 1));

    below_known_history_error(db.history_capabilities(), height).map_or(Ok(None), Err)
}

/// Returns [`BlockReadError::BlockBodyUnavailable`] if `history` shows that `height`, which is at
/// or below the tip and has no hash row, is below the known history, or `None` if the block is
/// unknown.
///
/// # Panics
///
/// If the history capabilities could not be read from the database, like the body reads.
pub(crate) fn below_known_history_error(
    history: Result<HistoryCapabilities, StateInitError>,
    height: Height,
) -> Option<BlockReadError> {
    match history {
        Ok(history) if below_known_history(history, height) => {
            Some(BlockReadError::BlockBodyUnavailable { height })
        }
        Err(StateInitError::HistoryRead { source, .. }) if source.is::<rocksdb::Error>() => {
            panic!("unexpected database failure: {source}");
        }
        // An invalid retained suffix cannot show that the block is below the known history, so
        // the height stays unknown, as it is when the history is complete.
        _ => None,
    }
}

/// Returns the depth of the block at `height` below the best tip.
///
/// Like [`super::depth`], this counts from the sampled chain's tip whenever there is a chain,
/// wherever the block was resolved, so a finalized block's depth includes the non-finalized
/// blocks above it.
fn depth(chain: Option<&Arc<Chain>>, db: &ZebraDb, height: Height) -> Option<u32> {
    // The writer commits blocks to the finalized state before it publishes the chains without
    // them, so a block resolved there can be above a stale chain snapshot's tip. Its finalized
    // tip is then the newest tip this request has seen.
    let tip = tip_height(chain, db)
        .filter(|tip| *tip >= height)
        .or_else(|| db.finalized_tip_height())?;

    // The finalized tip is at or above every finalized height, so this does not saturate.
    Some(tip.0.saturating_sub(height.0))
}

/// Returns the hash of the best-chain block after `resolved`, or `None` at the tip.
///
/// Like the [`ReadRequest::BlockHeader`](crate::ReadRequest::BlockHeader) handler, this looks
/// in the sampled chain first, wherever the block was resolved, so the finalized tip's next block
/// is the chain's root. A stale chain snapshot can be a fork of the finalized chain, so neither
/// side's successor is used unless it extends the resolved block.
fn next_block_hash(
    chain: Option<&Arc<Chain>>,
    db: &ZebraDb,
    resolved: &Resolved,
) -> Option<block::Hash> {
    let next_height = resolved.height.next().ok()?;

    chain
        .and_then(|chain| chain.block(next_height.into()))
        .filter(|next| next.block.header.previous_block_hash == resolved.hash)
        .map(|next| next.hash)
        .or_else(|| {
            let next_hash = db.hash(next_height)?;

            // The finalized chain is linear, so its next block extends any block it contains.
            (resolved.cached.is_none() || db.hash(resolved.height) == Some(resolved.hash))
                .then_some(next_hash)
        })
}

/// The parts of a block that are read from the side it was resolved on.
struct Parts {
    header: Option<Arc<block::Header>>,
    transaction_count: Option<usize>,
    transaction_ids: Option<Arc<[transaction::Hash]>>,
    block_info: Option<BlockInfo>,
    sapling_tree: Option<Arc<sapling::tree::NoteCommitmentTree>>,
    orchard_tree: Option<Arc<orchard::tree::NoteCommitmentTree>>,
    ironwood_tree: Option<Arc<orchard::tree::NoteCommitmentTree>>,
    raw_block: Option<SerializedBlock>,
    block: Option<Arc<Block>>,
    size: Option<usize>,
}

/// Reads the `fields` of `cached`, a block in the sampled `chain`.
fn cached_parts(
    chain: &Chain,
    cached: &ContextuallyVerifiedBlock,
    fields: BlockQueryFields,
) -> Parts {
    let height = cached.height.into();
    let raw_block = fields
        .contains(BlockQueryFields::RAW_BLOCK)
        .then(|| SerializedBlock::from(cached.block.as_ref()));
    let block_info = fields
        .intersects(BlockQueryFields::BLOCK_INFO | BODY)
        .then(|| chain.block_info(height))
        .flatten();

    let size = fields.intersects(BODY).then(|| {
        raw_block
            .as_ref()
            .map(|raw_block| raw_block.as_ref().len())
            .or_else(|| {
                block_info.as_ref().map(|block_info| {
                    // A `u32` always fits in a `usize` on the platforms Zebra supports.
                    block_info.size() as usize
                })
            })
            .unwrap_or_else(|| cached.block.zcash_serialized_size())
    });

    Parts {
        header: fields
            .contains(BlockQueryFields::HEADER)
            .then(|| cached.block.header.clone()),
        transaction_count: fields
            .contains(BlockQueryFields::TRANSACTION_COUNT)
            .then(|| cached.transaction_hashes.len()),
        transaction_ids: fields
            .contains(BlockQueryFields::TRANSACTION_IDS)
            .then(|| cached.transaction_hashes.clone()),
        block_info: block_info.filter(|_| fields.contains(BlockQueryFields::BLOCK_INFO)),
        sapling_tree: fields
            .contains(BlockQueryFields::SAPLING_TREE)
            .then(|| chain.sapling_tree(height))
            .flatten(),
        orchard_tree: fields
            .contains(BlockQueryFields::ORCHARD_TREE)
            .then(|| chain.orchard_tree(height))
            .flatten(),
        ironwood_tree: fields
            .contains(BlockQueryFields::IRONWOOD_TREE)
            .then(|| chain.ironwood_tree(height))
            .flatten(),
        raw_block,
        block: fields
            .intersects(TRANSACTIONS)
            .then(|| cached.block.clone()),
        size,
    }
}

/// Reads the `fields` of the finalized block at `height`, which the caller has resolved.
fn finalized_parts(
    db: &ZebraDb,
    height: Height,
    fields: BlockQueryFields,
) -> Result<Parts, BlockReadError> {
    // One body read serves the raw block, the transactions, the spent outputs and the
    // authorizing data root, and the header when any of them is requested.
    let body = if fields.intersects(BODY) {
        db.raw_block(height.into())?
    } else {
        None
    };

    let body_header = body
        .as_ref()
        .filter(|_| fields.intersects(BlockQueryFields::HEADER | TRANSACTIONS))
        .map(|(raw_header, _)| Arc::<block::Header>::from_bytes(raw_header.raw_bytes()));

    let header = if fields.contains(BlockQueryFields::HEADER) {
        // Like the `BlockHeader` handler, a missing header of a known block is corrupt on
        // secondaries too.
        let header = body_header
            .clone()
            .or_else(|| db.block_header(height.into()))
            .ok_or(BlockReadError::CorruptBlock { height })?;
        Some(header)
    } else {
        None
    };

    let block = body
        .as_ref()
        .zip(body_header)
        .filter(|_| fields.intersects(TRANSACTIONS))
        .map(|((_, raw_transactions), header)| {
            Arc::new(Block {
                header,
                transactions: raw_transactions
                    .iter()
                    .map(|raw_transaction| {
                        Arc::<Transaction>::from_bytes(raw_transaction.raw_bytes())
                    })
                    .collect(),
            })
        });

    let transaction_ids = if fields.contains(BlockQueryFields::TRANSACTION_IDS) {
        db.transaction_hashes_at_known_height(height)?
    } else {
        None
    };

    let transaction_count = if fields.contains(BlockQueryFields::TRANSACTION_COUNT) {
        match (&transaction_ids, &body) {
            (Some(transaction_ids), _) => Some(transaction_ids.len()),
            (None, Some((_, raw_transactions))) => Some(raw_transactions.len()),
            (None, None) => db.transaction_count(height)?,
        }
    } else {
        None
    };

    Ok(Parts {
        header,
        transaction_count,
        transaction_ids,
        block_info: fields
            .contains(BlockQueryFields::BLOCK_INFO)
            .then(|| db.block_info(height.into()))
            .flatten(),
        sapling_tree: fields
            .contains(BlockQueryFields::SAPLING_TREE)
            .then(|| db.sapling_tree_at_known_height(&height))
            .flatten(),
        orchard_tree: fields
            .contains(BlockQueryFields::ORCHARD_TREE)
            .then(|| db.orchard_tree_at_known_height(&height))
            .flatten(),
        ironwood_tree: fields
            .contains(BlockQueryFields::IRONWOOD_TREE)
            .then(|| db.ironwood_tree_at_known_height(&height))
            .flatten(),
        raw_block: body
            .as_ref()
            .filter(|_| fields.contains(BlockQueryFields::RAW_BLOCK))
            .map(|(raw_header, raw_transactions)| serialized_block(raw_header, raw_transactions)),
        block,
        size: body
            .as_ref()
            .map(|(raw_header, raw_transactions)| serialized_size(raw_header, raw_transactions)),
    })
}

/// Returns the CompactSize transaction count that a serialized block has between its header and
/// its transactions.
fn transaction_count_prefix(raw_transactions: &[RawBytes]) -> Vec<u8> {
    CompactSizeMessage::try_from(raw_transactions.len())
        .expect("a stored block's transaction count fits in a CompactSize, it was serialized")
        .zcash_serialize_to_vec()
        .expect("serializing into a Vec cannot fail")
}

/// Returns the length of the network serialization of a block with these stored parts.
fn serialized_size(raw_header: &RawBytes, raw_transactions: &[RawBytes]) -> usize {
    raw_header.raw_bytes().len()
        + transaction_count_prefix(raw_transactions).len()
        + raw_transactions
            .iter()
            .map(|raw_transaction| raw_transaction.raw_bytes().len())
            .sum::<usize>()
}

/// Returns the network serialization of a block from its stored parts, without deserializing
/// them.
///
/// The stored header and transactions are their network serializations, so the block is the
/// header, the transaction count, and the transactions.
fn serialized_block(raw_header: &RawBytes, raw_transactions: &[RawBytes]) -> SerializedBlock {
    let mut bytes = Vec::with_capacity(serialized_size(raw_header, raw_transactions));
    bytes.extend_from_slice(raw_header.raw_bytes());
    bytes.extend_from_slice(&transaction_count_prefix(raw_transactions));
    for raw_transaction in raw_transactions {
        bytes.extend_from_slice(raw_transaction.raw_bytes());
    }

    SerializedBlock::from(bytes)
}

/// Returns whether `height` is below the known history of a snapshot-born node, where blocks are
/// known to exist but none of their data is stored.
///
/// Always `false` until the state snapshot sync change adds
/// `HistoryCapabilities::lowest_known_height` for this to compare `height` against.
fn below_known_history(history: HistoryCapabilities, height: Height) -> bool {
    let _ = (history, height);

    false
}
