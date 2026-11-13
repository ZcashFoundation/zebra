//! The header writer: the only writer of the header chain column families.
//!
//! Headers-first sync sends verified header batches here with
//! [`Request::CommitHeaders`](crate::Request::CommitHeaders). The writer runs on
//! its own thread, fed by a channel, so header writes never wait behind block
//! commits and never need a lock: it is the only code that writes
//! `header_by_height` and `height_by_header_hash`, and the block writer never
//! touches them.
//!
//! For each batch, the writer:
//! 1. deletes stale rows at or below the finalized tip, and the whole stored
//!    chain if the finalized tip is no longer its parent,
//! 2. finds where the batch connects to the header chain, and skips the
//!    headers it already has,
//! 3. checks the heights, contiguity, checkpoints, and each header's
//!    difficulty and time against the previous headers, and
//! 4. appends the batch, or replaces the stored headers above the fork point
//!    when the batch has more work than the headers it replaces. Forks are
//!    chosen by work alone, however deep they are below the header tip, but
//!    never below the finalized tip or the last checkpoint.
//!
//! The writer stores no header above the lookahead limit, see
//! [`MAX_HEADER_LOOKAHEAD`]. So every fork it can accept competes with fewer
//! stored headers than one batch holds, and the work of both is compared over
//! the same heights.
//!
//! The header chain only chooses which bodies to download. The block chain is
//! still chosen by the block writer, from validated blocks.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::Span;

use zebra_chain::{
    block::{self, Height},
    parameters::Network,
};

use crate::{
    constants::MAX_BLOCK_REORG_HEIGHT,
    service::{
        check::{check_header_context, difficulty::MAX_POW_ADJUSTMENT_BLOCK_SPAN},
        finalized_state::{DiskWriteBatch, ZebraDb},
    },
    ValidateContextError,
};

#[cfg(test)]
mod tests;

/// The most headers one [`Request::CommitHeaders`](crate::Request::CommitHeaders)
/// can hold: more than one checkpoint span on Mainnet.
pub const MAX_COMMIT_HEADERS: usize = 2_000;

/// How many headers the header writer stores above the highest of the last
/// checkpoint and the finalized tip's reorg window, `MAX_BLOCK_REORG_HEIGHT`
/// above the finalized tip.
///
/// Every fork the writer accepts forks at or above the finalized tip, and at
/// or above the last checkpoint, so it competes with at most
/// `MAX_BLOCK_REORG_HEIGHT + MAX_HEADER_LOOKAHEAD` stored headers, fewer than
/// one batch of [`MAX_COMMIT_HEADERS`]. The block chain is at most
/// `MAX_BLOCK_REORG_HEIGHT` above the finalized tip, so its bodies can always
/// be downloaded at least this many headers further, and the finalized tip,
/// and the limit with it, keep moving up.
pub const MAX_HEADER_LOOKAHEAD: u32 = 500;

// A fork can always send more headers than the stored headers it competes with.
// `MAX_COMMIT_HEADERS` is a small constant, so the cast is exact.
const _: () = assert!(MAX_BLOCK_REORG_HEIGHT + MAX_HEADER_LOOKAHEAD < MAX_COMMIT_HEADERS as u32);

/// How often the header writer deletes stale rows while it has no batch to
/// commit, so they don't pile up while the block writer commits bodies.
///
/// Each collection deletes at most `MAX_HEADER_GC_ROWS` (10,000) rows, more
/// than the block writer commits in this time.
pub const HEADER_GC_INTERVAL: Duration = Duration::from_secs(1);

/// The header chain after a commit.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HeadersCommitted {
    /// The header chain tip.
    pub tip: (Height, block::Hash),

    /// How many of the batch's headers were new.
    pub added: usize,

    /// Whether the batch replaced stored headers above a fork point.
    pub reorged: bool,
}

/// A header batch the header writer didn't store.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum CommitHeadersError {
    #[error("the header batch is empty")]
    Empty,

    #[error("the header batch has {count} headers, more than {MAX_COMMIT_HEADERS}")]
    TooMany { count: usize },

    #[error("the first header's parent {parent:?} is not on the header chain")]
    Disconnected { parent: block::Hash },

    #[error("header {height:?} doesn't link to the previous header in its batch")]
    NotContiguous { height: Height },

    #[error("the batch overflows the maximum height")]
    HeightOverflow,

    #[error("header {height:?} conflicts with the finalized tip {finalized_tip:?}")]
    BelowFinalizedTip {
        height: Height,
        finalized_tip: Height,
    },

    #[error("header {height:?} has hash {actual:?}, but the checkpoint is {expected:?}")]
    CheckpointMismatch {
        height: Height,
        expected: block::Hash,
        actual: block::Hash,
    },

    #[error("header {height:?} has an invalid difficulty or time: {source}")]
    InvalidContext {
        height: Height,
        #[source]
        source: Box<ValidateContextError>,
    },

    #[error("the fork at {fork_parent:?} is below the last checkpoint {checkpoint:?}")]
    BelowLastCheckpoint {
        fork_parent: Height,
        checkpoint: Height,
    },

    #[error("the fork at {fork_parent:?} doesn't have more work than the header chain")]
    LessWork { fork_parent: Height },

    #[error("a header below the fork at {fork_parent:?} is missing from the header chain")]
    MissingContext { fork_parent: Height },

    #[error("header {height:?} is above the header lookahead limit {limit:?}")]
    AboveLookahead { height: Height, limit: Height },

    #[error("the header writer has stopped: is Zebra shutting down?")]
    WriterStopped,
}

impl CommitHeadersError {
    /// Whether the peer that sent the batch misbehaved.
    ///
    /// A batch that doesn't connect, a valid fork with less work, or headers
    /// above the lookahead limit can come from an honest peer. Invalid headers
    /// can't.
    pub fn is_misbehavior(&self) -> bool {
        matches!(
            self,
            CommitHeadersError::NotContiguous { .. }
                | CommitHeadersError::CheckpointMismatch { .. }
                | CommitHeadersError::InvalidContext { .. }
                | CommitHeadersError::BelowFinalizedTip { .. }
        )
    }
}

/// A batch for the header writer, with the channel for its result.
#[derive(Debug)]
pub(crate) struct CommitHeadersMessage {
    pub headers: Vec<Arc<block::Header>>,
    pub rsp_tx: oneshot::Sender<Result<HeadersCommitted, CommitHeadersError>>,
}

/// Spawns the header writer thread for `db`, and returns its channel.
///
/// The thread checks the stored header chain against the finalized tip when it
/// starts, and deletes stale rows every [`HEADER_GC_INTERVAL`] while it has no
/// batch to commit. It exits when every sender is dropped.
pub(crate) fn spawn(db: ZebraDb, network: Network) -> mpsc::UnboundedSender<CommitHeadersMessage> {
    // Each sender waits for its batch's result before sending another, and the
    // only sender is headers-first sync, so the channel holds one batch per
    // concurrent caller.
    let (sender, mut receiver) = mpsc::unbounded_channel::<CommitHeadersMessage>();

    let span = Span::current();
    std::thread::Builder::new()
        .name("header-writer".to_string())
        .spawn(move || {
            // Only for the receive timeout: the writes stay on this thread.
            let timer = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("a current-thread runtime only fails if the OS is out of resources");

            span.in_scope(|| {
                collect_stale_headers(&db);
                drop_headers_off_the_checkpoints(&db, &network);

                loop {
                    let message = timer.block_on(async {
                        tokio::time::timeout(HEADER_GC_INTERVAL, receiver.recv()).await
                    });
                    let CommitHeadersMessage { headers, rsp_tx } = match message {
                        Ok(Some(message)) => message,
                        Ok(None) => break,
                        Err(_elapsed) => {
                            collect_stale_headers(&db);
                            continue;
                        }
                    };

                    let result = commit_headers(&db, &network, headers);

                    if let Ok(committed) = &result {
                        // Heights fit in an f64 without loss.
                        metrics::gauge!("state.header_chain.tip.height")
                            .set(f64::from(committed.tip.0 .0));
                    }

                    let _ = rsp_tx.send(result);
                }
            })
        })
        .expect("spawning a named thread only fails if the OS is out of resources");

    sender
}

/// Deletes stale header chain rows, and the whole stored chain if the
/// finalized tip is no longer the parent of its lowest row.
pub(crate) fn collect_stale_headers(db: &ZebraDb) {
    let mut batch = DiskWriteBatch::new();
    let mut deleted = batch.prepare_header_chain_gc(db);

    if let Some((finalized_tip, finalized_hash)) = db.tip() {
        let lowest = finalized_tip.next().ok().and_then(|height| {
            db.header_by_height_cf()
                .zs_get(&height)
                .map(|h| (height, h))
        });

        if let Some((lowest_height, lowest_header)) = lowest {
            if lowest_header.previous_block_hash != finalized_hash {
                tracing::info!(
                    ?finalized_tip,
                    "the finalized chain left the header chain: dropping the stored headers"
                );
                deleted += batch.prepare_header_chain_truncate(db, lowest_height);
            }
        }
    }

    if deleted > 0 {
        db.write_batch(batch)
            .expect("unexpected rocksdb error while deleting stale headers");
    }
}

/// Deletes the stored headers above the last checkpoint they match, if a
/// checkpoint they reach has another hash, or if they stop below the next
/// checkpoint.
///
/// A fork below the last checkpoint is never replaced, so headers stored above
/// an older Zebra's last checkpoint must still match the checkpoints a newer
/// Zebra adds there. Headers that stop below a checkpoint can't be checked
/// against it, so they are dropped too, and headers-first sync downloads them
/// again from the last checkpoint they match, which honest peers have.
pub(crate) fn drop_headers_off_the_checkpoints(db: &ZebraDb, network: &Network) {
    let (Some(finalized_tip), Some((header_tip, _))) =
        (db.finalized_tip_height(), db.header_chain_tip())
    else {
        return;
    };

    let checkpoints = network.checkpoint_list();
    let mut last_match = finalized_tip;
    for (height, hash) in checkpoints.iter_cloned() {
        if height <= finalized_tip {
            continue;
        }
        if height > header_tip {
            if last_match < header_tip {
                let Ok(from) = last_match.next() else {
                    return;
                };
                tracing::info!(
                    checkpoint = ?height,
                    "stored headers stop below a checkpoint: dropping the headers above the last match"
                );
                let mut batch = DiskWriteBatch::new();
                batch.prepare_header_chain_truncate(db, from);
                db.write_batch(batch)
                    .expect("unexpected rocksdb error while deleting headers below a checkpoint");
            }
            return;
        }
        if db.header_chain_hash(height) == Some(hash) {
            last_match = height;
            continue;
        }

        let Ok(from) = last_match.next() else {
            return;
        };
        tracing::info!(
            checkpoint = ?height,
            "stored headers don't match a checkpoint: dropping the headers above the last match"
        );
        let mut batch = DiskWriteBatch::new();
        batch.prepare_header_chain_truncate(db, from);
        db.write_batch(batch)
            .expect("unexpected rocksdb error while deleting headers off the checkpoints");
        return;
    }
}

/// The highest height the header writer stores a header at in `db`: see
/// [`MAX_HEADER_LOOKAHEAD`].
pub(crate) fn header_lookahead_limit(db: &ZebraDb, network: &Network) -> Height {
    let reorg_window = db
        .finalized_tip_height()
        .unwrap_or(Height(0))
        .0
        .saturating_add(MAX_BLOCK_REORG_HEIGHT);
    let base = reorg_window.max(network.checkpoint_list().max_height().0);

    Height(base.saturating_add(MAX_HEADER_LOOKAHEAD).min(Height::MAX.0))
}

/// Checks `headers` and stores them on the header chain of `db`, up to the
/// [lookahead limit](MAX_HEADER_LOOKAHEAD).
///
/// See the [module docs](self) for the steps.
///
/// # Panics
///
/// If the database write fails, like the block writer.
#[allow(clippy::unwrap_in_result)]
pub(crate) fn commit_headers(
    db: &ZebraDb,
    network: &Network,
    headers: Vec<Arc<block::Header>>,
) -> Result<HeadersCommitted, CommitHeadersError> {
    collect_stale_headers(db);

    if headers.is_empty() {
        return Err(CommitHeadersError::Empty);
    }
    if headers.len() > MAX_COMMIT_HEADERS {
        return Err(CommitHeadersError::TooMany {
            count: headers.len(),
        });
    }

    let tip = db
        .header_chain_tip()
        .ok_or(CommitHeadersError::Disconnected {
            parent: headers[0].previous_block_hash,
        })?;

    let parent = headers[0].previous_block_hash;
    let parent_height = db
        .header_chain_height(parent)
        .ok_or(CommitHeadersError::Disconnected { parent })?;

    let hashes: Vec<block::Hash> = headers.iter().map(|header| header.hash()).collect();
    for (index, header) in headers.iter().enumerate().skip(1) {
        if header.previous_block_hash != hashes[index - 1] {
            return Err(CommitHeadersError::NotContiguous {
                height: height_at(parent_height, index)?,
            });
        }
    }

    // Skip the headers the chain already has.
    let mut known = 0;
    while known < headers.len()
        && db.header_chain_hash(height_at(parent_height, known)?) == Some(hashes[known])
    {
        known += 1;
    }
    if known == headers.len() {
        return Ok(HeadersCommitted {
            tip,
            added: 0,
            reorged: false,
        });
    }

    let new_headers = &headers[known..];
    let new_hashes = &hashes[known..];
    let fork_parent = match known.checked_sub(1) {
        None => parent_height,
        Some(last_known) => height_at(parent_height, last_known)?,
    };
    let first_height = height_at(fork_parent, 0)?;

    if let Some(finalized_tip) = db.finalized_tip_height() {
        if fork_parent < finalized_tip {
            return Err(CommitHeadersError::BelowFinalizedTip {
                height: first_height,
                finalized_tip,
            });
        }
    }

    // A fork below the last checkpoint replaces the checkpointed header there,
    // so no batch can continue it to a valid chain. Rejecting it also bounds
    // the stored headers a fork's work is compared with by the headers above
    // the last checkpoint.
    let checkpoints = network.checkpoint_list();
    let last_checkpoint = checkpoints.max_height();
    if fork_parent < last_checkpoint && fork_parent < tip.0 {
        return Err(CommitHeadersError::BelowLastCheckpoint {
            fork_parent,
            checkpoint: last_checkpoint,
        });
    }

    // Store, check and compare only the headers up to the lookahead limit, so a
    // batch that crosses it still stores the headers below it.
    let limit = header_lookahead_limit(db, network);
    // Heights fit in a usize.
    let below_limit = limit.0.saturating_sub(fork_parent.0) as usize;
    if below_limit == 0 {
        return Err(CommitHeadersError::AboveLookahead {
            height: first_height,
            limit,
        });
    }
    let new_headers = &new_headers[..new_headers.len().min(below_limit)];
    let new_hashes = &new_hashes[..new_headers.len()];
    let last_height = height_at(fork_parent, new_headers.len() - 1)?;

    for (offset, hash) in new_hashes.iter().enumerate() {
        let height = height_at(fork_parent, offset)?;
        if let Some(expected) = checkpoints.hash(height) {
            if expected != *hash {
                return Err(CommitHeadersError::CheckpointMismatch {
                    height,
                    expected,
                    actual: *hash,
                });
            }
        }
    }

    check_contexts(db, network, fork_parent, new_headers)?;

    let reorged = fork_parent < tip.0;
    let mut batch = DiskWriteBatch::new();

    if reorged {
        // The fork is chosen by work alone, however deep it is below the header
        // tip: the header tip can be far ahead of the block chain, and a peer
        // could otherwise store a long low-work chain first and keep the
        // honest chain out. Forks below the finalized tip or the last
        // checkpoint were rejected above, so the stored headers above the fork
        // point fit in one batch.
        //
        // The fork and the stored headers are compared up to the lookahead
        // limit, the same heights, so two chains that both cross it can't
        // replace each other in turn. The stored chain wins ties.
        let new_work: u128 = new_headers
            .iter()
            .map(|header| header_work(header))
            .fold(0, u128::saturating_add);

        // Stop reading stored headers once they have at least the fork's work.
        // Older Zebra versions could store headers above the limit.
        let mut stored_work: u128 = 0;
        for height in first_height.0..=tip.0 .0.min(limit.0) {
            if let Some(header) = db.any_header(Height(height)) {
                stored_work = stored_work.saturating_add(header_work(&header));
            }
            if stored_work >= new_work {
                return Err(CommitHeadersError::LessWork { fork_parent });
            }
        }

        tracing::info!(
            ?fork_parent,
            old_tip = ?tip,
            new_tip = ?(last_height, new_hashes[new_hashes.len() - 1]),
            "a fork with more work replaces the stored headers"
        );
        batch.prepare_header_chain_truncate(db, first_height);
    }

    batch.prepare_header_chain_append(db, first_height, new_headers);
    db.write_batch(batch)
        .expect("unexpected rocksdb error while writing headers");

    Ok(HeadersCommitted {
        tip: (last_height, new_hashes[new_hashes.len() - 1]),
        added: new_headers.len(),
        reorged,
    })
}

/// Checks the difficulty and time of `headers`, which follow `fork_parent`,
/// against the previous headers on the header chain and in the batch.
fn check_contexts(
    db: &ZebraDb,
    network: &Network,
    fork_parent: Height,
    headers: &[Arc<block::Header>],
) -> Result<(), CommitHeadersError> {
    // The previous headers, in reverse height order, starting with the parent.
    let mut context: VecDeque<Arc<block::Header>> = (0..=fork_parent.0)
        .rev()
        .take(MAX_POW_ADJUSTMENT_BLOCK_SPAN)
        .map_while(|height| db.any_header(Height(height)))
        .collect();

    // A shorter context would change the expected difficulty and time limits,
    // so check every header against the full context or not at all.
    let expected = usize::try_from(fork_parent.0).map_or(MAX_POW_ADJUSTMENT_BLOCK_SPAN, |height| {
        height.saturating_add(1).min(MAX_POW_ADJUSTMENT_BLOCK_SPAN)
    });
    if context.len() < expected {
        return Err(CommitHeadersError::MissingContext { fork_parent });
    }

    for (offset, header) in headers.iter().enumerate() {
        let height = height_at(fork_parent, offset)?;

        check_header_context(context.make_contiguous(), header, height, network).map_err(
            |source| CommitHeadersError::InvalidContext {
                height,
                source: Box::new(source),
            },
        )?;

        context.push_front(header.clone());
        context.truncate(MAX_POW_ADJUSTMENT_BLOCK_SPAN);
    }

    Ok(())
}

/// The height `offset + 1` above `parent`.
fn height_at(parent: Height, offset: usize) -> Result<Height, CommitHeadersError> {
    u32::try_from(offset)
        .ok()
        .and_then(|offset| parent.0.checked_add(offset)?.checked_add(1))
        .map(Height)
        .filter(|height| *height <= Height::MAX)
        .ok_or(CommitHeadersError::HeightOverflow)
}

/// The work of `header`'s difficulty threshold, or zero for an invalid one,
/// which the verifier has already rejected.
fn header_work(header: &block::Header) -> u128 {
    header
        .difficulty_threshold
        .to_work()
        .map_or(0, |work| work.as_u128())
}
