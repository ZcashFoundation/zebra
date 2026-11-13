//! Reading best-chain sync-metadata entries, for serving version 2
//! `get-hashes` requests (design §5).
//!
//! Ported from #11349's `sync_hashes` (`zebra-state/src/service/read/block.rs`
//! on `p2p-v2-6-known-hash-formats`), simplified to stride 1: each entry
//! describes one block, so there are no spans to aggregate.

use std::ops::RangeInclusive;

use zebra_chain::block::{self, Height, SyncHashEntry};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
};

#[cfg(test)]
mod tests;

/// Returns the best-chain entries for `count` heights from `start`.
///
/// See [`ReadRequest::SyncHashes`](crate::ReadRequest::SyncHashes) for the
/// margin and prefix rules. Entries come from the finalized state only: the
/// margin (`MAX_BLOCK_REORG_HEIGHT` below the best tip) keeps them below the
/// non-finalized chain in steady state, and `chain` is used only to find the
/// best tip. A served height the finalized state does not have yet (the
/// finalized writer is behind) ends the response, like missing metadata.
pub fn sync_hashes<C: AsRef<Chain>>(
    chain: Option<C>,
    db: &ZebraDb,
    start: block::Height,
    count: u32,
) -> Vec<SyncHashEntry> {
    let best_tip = chain
        .map(|chain| chain.as_ref().non_finalized_tip_height())
        .or_else(|| db.finalized_tip_height());
    let Some(range) = served_range(best_tip, start, count) else {
        return Vec::new();
    };

    // `txouts` at `start` needs the cumulative output count of the block below it, so the one
    // range scan starts a block early. Genesis has no block below it.
    let below = start.previous().ok();
    let scan = below.unwrap_or(start)..=*range.end();
    let counts = db
        .sync_metadata_range(scan)
        .into_iter()
        .map(|meta| BlockCounts {
            size: meta.size,
            txs: meta.tx_count,
            notes: meta.note_count,
            cumulative_txouts: meta.cumulative_transparent_outputs,
        });

    // One scan of the height index, not a lookup per height: a request can
    // name `MAX_GET_HASHES_COUNT` heights.
    let first = *range.start();
    let hashes = db.hash_range(range.clone());
    entries(range, below.is_some(), counts, |height| {
        let index = usize::try_from(height.0.checked_sub(first.0)?).ok()?;
        hashes.get(index).copied()
    })
}

/// The heights a `get-hashes` request for `count` heights from `start` is
/// served from, when the best tip is `best_tip`, or `None` if it is served
/// with no entries.
///
/// # Security
///
/// The range ends [`MAX_BLOCK_REORG_HEIGHT`] below the best tip (decision D6),
/// so every served entry is final: no entry is ever withdrawn by a rollback,
/// and the response is always a prefix of the request. A chain no longer than
/// the margin serves nothing. The range is bounded by `count`, which the wire
/// request bounds at `MAX_GET_HASHES_COUNT`.
fn served_range(
    best_tip: Option<Height>,
    start: Height,
    count: u32,
) -> Option<RangeInclusive<Height>> {
    let max_served = best_tip?.0.checked_sub(MAX_BLOCK_REORG_HEIGHT)?;

    // Widened, so that the request's last height cannot overflow.
    let requested_end = u64::from(start.0) + u64::from(count.checked_sub(1)?);
    let end = requested_end.min(u64::from(max_served));
    let end = Height(u32::try_from(end).expect("the end is at most max_served, which is a u32"));

    (start <= end).then_some(start..=end)
}

/// The sync metadata fields a `get-hashes` entry is built from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct BlockCounts {
    size: u32,
    txs: u32,
    notes: u32,
    /// The transparent outputs of this block and every block below it.
    cumulative_txouts: u64,
}

/// Builds the entries for `range` from the metadata `counts` of a contiguous
/// prefix of the scanned heights, and the hash index.
///
/// When `has_below`, the scan started one height below `range` (for the
/// cumulative output count below the first entry), so the first of `counts`
/// is that height's; otherwise `range` starts at genesis, which has no outputs
/// below it. The entries stop at the first height whose metadata or hash is
/// missing.
fn entries(
    range: RangeInclusive<Height>,
    has_below: bool,
    counts: impl IntoIterator<Item = BlockCounts>,
    mut hash: impl FnMut(Height) -> Option<block::Hash>,
) -> Vec<SyncHashEntry> {
    let mut counts = counts.into_iter();
    let mut cumulative_below = if has_below {
        // No metadata for the height below `start`: the upgrade has not reached it yet, so no
        // entry's output count is known.
        let Some(below) = counts.next() else {
            return Vec::new();
        };
        below.cumulative_txouts
    } else {
        0
    };

    let mut entries = Vec::new();
    for (height, counts) in (range.start().0..=range.end().0).map(Height).zip(counts) {
        let Some(hash) = hash(height) else {
            break;
        };

        // The cumulative count never decreases along the chain, so a decrease is a corrupt record.
        // Serving a wrapped count would mislead every requester: end the response here instead.
        let Some(txouts) = counts.cumulative_txouts.checked_sub(cumulative_below) else {
            tracing::warn!(
                ?height,
                "sync metadata output count decreased, ending the get-hashes response"
            );
            break;
        };
        cumulative_below = counts.cumulative_txouts;

        entries.push(SyncHashEntry {
            hash,
            size: counts.size.into(),
            txs: counts.txs.into(),
            txouts,
            notes: counts.notes.into(),
        });
    }

    entries
}
