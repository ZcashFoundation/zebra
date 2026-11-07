//! Context-free verification of block headers that arrive before their bodies.
//!
//! Headers-first sync checks each batch of headers here before it stores them:
//! - the headers are contiguous,
//! - each header's difficulty threshold is at most the network's limit, and
//!   its hash passes the difficulty filter,
//! - each header's Equihash solution is valid,
//! - no header's time is more than 2 hours in the future, and
//! - each header at a checkpoint height has the checkpoint's hash.
//!
//! The proof of work checks are skipped exactly where the block verifier skips
//! them, on networks with `disable_pow`. The contextual checks, which need the
//! previous headers of the chain, run in the state (see
//! `zebra_state::check::check_header_context`).
//!
//! Hashing and Equihash verification run on the rayon thread pool, in parallel
//! across the batch, so they never block a tokio worker.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use thiserror::Error;

use zebra_chain::{
    block::{self, BlockTimeError, Height},
    parameters::Network,
    work::equihash,
};

use crate::{
    block::check::{
        difficulty_is_valid, difficulty_threshold_is_valid, equihash_solution_is_valid,
        time_is_valid_at,
    },
    primitives::spawn_fifo,
    BlockError,
};

#[cfg(test)]
mod tests;

/// An invalid header batch.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum HeaderError {
    #[error("the header batch is empty")]
    Empty,

    #[error("header {height:?} {hash:?} doesn't link to the previous header in its batch")]
    NotContiguous { height: Height, hash: block::Hash },

    #[error("header heights above {first_height:?} overflow the maximum height")]
    HeightOverflow { first_height: Height },

    #[error("header {height:?} has hash {actual:?}, but the checkpoint is {expected:?}")]
    CheckpointMismatch {
        height: Height,
        expected: block::Hash,
        actual: block::Hash,
    },

    #[error("invalid difficulty in header {height:?} {hash:?}")]
    Difficulty {
        height: Height,
        hash: block::Hash,
        #[source]
        source: Box<BlockError>,
    },

    #[error("invalid Equihash solution in header {height:?} {hash:?}")]
    Equihash {
        height: Height,
        hash: block::Hash,
        #[source]
        source: equihash::Error,
    },

    #[error("invalid time in header {height:?}")]
    Time {
        height: Height,
        #[source]
        source: BlockTimeError,
    },

    #[error("the header verification thread pool dropped the batch: is Zebra shutting down?")]
    ThreadPool,
}

impl HeaderError {
    /// Whether the peer that sent the batch misbehaved.
    ///
    /// Every error except a dropped batch means the headers are invalid, and an
    /// honest peer never sends invalid headers. A header that is only too far in
    /// the future can become valid later, so it is not misbehavior.
    pub fn is_misbehavior(&self) -> bool {
        !matches!(self, HeaderError::ThreadPool | HeaderError::Time { .. })
    }
}

/// Verifies a batch of contiguous `headers` from `first_height` on `network`,
/// with `now` as the local time, and returns their hashes.
///
/// Doesn't check that the first header links to the chain below it, or the
/// contextual difficulty and time rules: the state checks those when it stores
/// the headers.
pub async fn verify_headers(
    network: &Network,
    first_height: Height,
    headers: Vec<Arc<block::Header>>,
    now: DateTime<Utc>,
) -> Result<Vec<block::Hash>, HeaderError> {
    let network = network.clone();

    spawn_fifo(move || verify_headers_blocking(&network, first_height, &headers, now))
        .await
        .map_err(|_| HeaderError::ThreadPool)?
}

/// Verifies a batch of headers on the current thread, using the rayon thread
/// pool for the per-header checks.
///
/// See [`verify_headers`] for details.
pub fn verify_headers_blocking(
    network: &Network,
    first_height: Height,
    headers: &[Arc<block::Header>],
    now: DateTime<Utc>,
) -> Result<Vec<block::Hash>, HeaderError> {
    if headers.is_empty() {
        return Err(HeaderError::Empty);
    }

    let last_offset = u32::try_from(headers.len() - 1)
        .map_err(|_| HeaderError::HeightOverflow { first_height })?;
    first_height
        .0
        .checked_add(last_offset)
        .filter(|last_height| Height(*last_height) <= Height::MAX)
        .ok_or(HeaderError::HeightOverflow { first_height })?;

    let checkpoints = network.checkpoint_list();
    let skip_pow = network.disable_pow();

    let hashes: Vec<block::Hash> = headers
        .par_iter()
        .enumerate()
        .map(|(offset, header)| {
            // The offset is at most `last_offset`, which fits in a u32.
            let height = Height(first_height.0 + offset as u32);
            let hash = header.hash();

            verify_header(network, header, height, hash, now, skip_pow)?;

            if let Some(expected) = checkpoints.hash(height) {
                if expected != hash {
                    return Err(HeaderError::CheckpointMismatch {
                        height,
                        expected,
                        actual: hash,
                    });
                }
            }

            Ok(hash)
        })
        .collect::<Result<_, _>>()?;

    for (offset, (header, previous_hash)) in headers.iter().skip(1).zip(&hashes).enumerate() {
        if header.previous_block_hash != *previous_hash {
            // The offset is below `last_offset`, which fits in a u32.
            let height = Height(first_height.0 + offset as u32 + 1);
            return Err(HeaderError::NotContiguous {
                height,
                hash: hashes[offset + 1],
            });
        }
    }

    Ok(hashes)
}

/// The context-free checks of one header.
fn verify_header(
    network: &Network,
    header: &block::Header,
    height: Height,
    hash: block::Hash,
    now: DateTime<Utc>,
    skip_pow: bool,
) -> Result<(), HeaderError> {
    let difficulty_error = |source| HeaderError::Difficulty {
        height,
        hash,
        source: Box::new(source),
    };

    if skip_pow {
        difficulty_threshold_is_valid(header, network, &height, &hash)
            .map(|_threshold| ())
            .map_err(difficulty_error)?;
    } else {
        difficulty_is_valid(header, network, &height, &hash).map_err(difficulty_error)?;
        equihash_solution_is_valid(header).map_err(|source| HeaderError::Equihash {
            height,
            hash,
            source,
        })?;
    }

    time_is_valid_at(header, now, &height, &hash)
        .map_err(|source| HeaderError::Time { height, source })
}
