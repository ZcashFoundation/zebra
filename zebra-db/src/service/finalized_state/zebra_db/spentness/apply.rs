//! Resolve each hinted block's spends and membership bits for the ordinary writer.
//!
//! Applying consumes every output bit, genesis and non-address scripts
//! included. The ordinary writer then inserts the survivors into the UTXO set
//! and writes every index, balance and value pool. Omitted outputs get no UTXO
//! rows, because blocks spend them before `H`, so their spends delete nothing.
//!
//! Ported from `zakura/feat/spentness-writer` (`a909419a59`, `spentness/apply.rs`).

use std::collections::{BTreeMap, HashMap, HashSet};

use rayon::prelude::*;

use zebra_chain::{
    block::{self, Height},
    parameters::spentness_hints::VerifiedArtifact,
    transparent,
};

use super::{
    live::{LiveOutput, LiveOutputs},
    read_progress,
    record::{Phase, Progress},
    HintedOutputs, SpentnessError, SPENTNESS_JOURNAL,
};
use crate::FinalizedBlock;

use crate::service::finalized_state::{
    disk_db::{DiskWriteBatch, ReadDisk, WriteDisk},
    disk_format::{OutputLocation, TransactionLocation},
    zebra_db::{ZebraDb, PARALLEL_BLOCK_READ_THRESHOLD},
};

/// The artifact position and counts at a block boundary.
#[derive(Clone, Copy, Debug)]
struct Cursor {
    ordinal: u64,
    omitted: u64,
    resolved: u64,
}

/// Reads `finalized`'s membership bits, resolves its spends, and adds its
/// journal rows and construction record to `batch`.
#[allow(clippy::unwrap_in_result)]
pub(super) fn apply_block(
    artifact: &VerifiedArtifact,
    live: &mut LiveOutputs,
    db: &ZebraDb,
    finalized: &FinalizedBlock,
    new_outputs: &BTreeMap<OutputLocation, transparent::Utxo>,
    batch: &mut DiskWriteBatch,
) -> Result<HintedOutputs, SpentnessError> {
    let height = finalized.height;
    let commitment = artifact.pin().spentness_hash;
    let mut cursor = cursor(artifact, db, height)?;
    let unresolved = SpentnessError::UnresolvedSpend { height };

    // Membership bits, in canonical output order.
    let mut survivors = BTreeMap::new();
    let mut omitted = HashSet::new();
    for location in output_locations(&finalized.block, height) {
        let retained = retains(artifact, cursor.ordinal)?;
        // `retains` accepted the ordinal, so it is below `output_count` and cannot overflow.
        cursor.ordinal += 1;

        if height.is_min() {
            // Genesis outputs are unspendable, so they are neither survivors nor omitted.
            if retained {
                return Err(SpentnessError::RetainedGenesisOutput);
            }
        } else if retained {
            let utxo = new_outputs
                .get(&location)
                .ok_or(SpentnessError::Inconsistent(
                    "a block output is missing from its new outputs",
                ))?;
            survivors.insert(location, utxo.clone());
        } else {
            omitted.insert(location);
        }
    }

    // Spends: from this block, then the live map, then the journal.
    let tx_hash_indexes: HashMap<_, _> = finalized
        .transaction_hashes
        .iter()
        .enumerate()
        .map(|(index, hash)| (*hash, index))
        .collect();
    let mut spent = Vec::new();
    let mut misses = Vec::new();
    let mut spent_here = HashSet::new();
    for outpoint in finalized
        .block
        .transactions
        .iter()
        .flat_map(|transaction| transaction.inputs())
        .filter_map(|input| input.outpoint())
    {
        if let Some(tx_index) = tx_hash_indexes.get(&outpoint.hash) {
            let location = OutputLocation::from_outpoint(
                TransactionLocation::from_usize(height, *tx_index),
                &outpoint,
            );
            // A same-block spend of a survivor contradicts the artifact.
            if !omitted.contains(&location) {
                return Err(unresolved);
            }
            let utxo = new_outputs.get(&location).ok_or(unresolved.clone())?;
            spent_here.insert(location);
            spent.push(Some((outpoint, location, utxo.clone())));
        } else if let Some(output) = live.take(&outpoint) {
            spent.push(Some((outpoint, output.location, output.utxo)));
        } else {
            misses.push((spent.len(), outpoint));
            spent.push(None);
        }
    }

    let resolve = |(slot, outpoint): (usize, transparent::OutPoint)| {
        journal_output(db, &outpoint)
            .map(|(location, utxo)| (slot, (outpoint, location, utxo)))
            .ok_or(unresolved.clone())
    };
    let resolved: Vec<_> = if misses.len() >= PARALLEL_BLOCK_READ_THRESHOLD {
        misses
            .par_iter()
            .copied()
            .map(resolve)
            .collect::<Result<_, _>>()?
    } else {
        misses
            .iter()
            .copied()
            .map(resolve)
            .collect::<Result<_, _>>()?
    };
    metrics::counter!("state.spentness.spends.journal").increment(count(resolved.len()));
    for (slot, spend) in resolved {
        spent[slot] = Some(spend);
    }
    let spent: Vec<_> = spent
        .into_iter()
        .map(|spend| spend.ok_or(unresolved.clone()))
        .collect::<Result<_, _>>()?;

    cursor.omitted += count(omitted.len());
    // Every spend resolved to a distinct omitted output in a valid chain, and the
    // count check at `H` rejects any excess.
    cursor.resolved += count(spent.len());

    let journal = db
        .db
        .cf_handle(SPENTNESS_JOURNAL)
        .expect("the spentness_journal column family is declared");
    let progress = if height == artifact.header().terminal_height {
        terminal_progress(artifact, finalized.hash, cursor)?
    } else {
        // Journal the omitted outputs that outlive their block, and keep them live.
        for location in output_locations(&finalized.block, height) {
            if !omitted.contains(&location) || spent_here.contains(&location) {
                continue;
            }
            let utxo = new_outputs
                .get(&location)
                .ok_or(SpentnessError::Inconsistent(
                    "a block output is missing from its new outputs",
                ))?;
            batch.zs_insert(&journal, location, &utxo.output);
            let outpoint = transparent::OutPoint::from_usize(
                finalized.transaction_hashes[location.transaction_index().as_usize()],
                location.output_index().as_usize(),
            );
            live.insert(
                outpoint,
                LiveOutput {
                    location,
                    utxo: utxo.clone(),
                },
            );
        }
        metrics::counter!("state.spentness.live.evicted").increment(count(live.evict_if_full()));
        // The map holds far fewer than 2^52 entries, so the gauge value is exact.
        metrics::gauge!("state.spentness.live.entries").set(live.len() as f64);

        Progress {
            phase: Phase::Applying,
            height,
            next_ordinal: cursor.ordinal,
            omitted: cursor.omitted,
            resolved: cursor.resolved,
        }
    };

    if progress.phase == Phase::Complete {
        // Every omitted output is spent, so the journal holds nothing live.
        batch.zs_delete_range(
            &journal,
            OutputLocation::from_usize(Height(0), 0, 0),
            OutputLocation::from_usize(
                (height + 1).expect("the terminal height is a checkpoint below the maximum"),
                0,
                0,
            ),
        );
        live.clear();
        tracing::info!(target: "zebra_state::service::finalized_state::zebra_db::spentness::apply", ?height, "spentness hints applied and verified");
    }
    batch.prepare_spentness_progress(db, commitment, progress);
    metrics::gauge!("state.spentness.height").set(f64::from(height.0));

    Ok(HintedOutputs { survivors, spent })
}

/// The cursor for `height`: zero on an empty database, the recorded cursor
/// when the record is at the parent block, or the genesis output count when
/// adopting after an unhinted genesis.
#[allow(clippy::unwrap_in_result)]
fn cursor(
    artifact: &VerifiedArtifact,
    db: &ZebraDb,
    height: Height,
) -> Result<Cursor, SpentnessError> {
    const MISMATCH: SpentnessError =
        SpentnessError::Inconsistent("the construction record does not match the finalized tip");

    let tip = db.finalized_tip_height();
    match read_progress(db)? {
        None if tip.is_none() && height.is_min() => Ok(Cursor {
            ordinal: 0,
            omitted: 0,
            resolved: 0,
        }),
        None if tip == Some(Height(0)) && height == Height(1) => {
            // Genesis was committed before the artifact arrived. Its outputs were never
            // in the UTXO set, so its bits must be zero.
            let genesis = db
                .block(Height(0).into())
                .ok()
                .flatten()
                .ok_or(SpentnessError::Inconsistent("the genesis block is missing"))?;
            let mut ordinal = 0;
            for _ in output_locations(&genesis, Height(0)) {
                if retains(artifact, ordinal)? {
                    return Err(SpentnessError::RetainedGenesisOutput);
                }
                ordinal += 1;
            }
            Ok(Cursor {
                ordinal,
                omitted: 0,
                resolved: 0,
            })
        }
        Some((commitment, progress))
            if commitment == artifact.pin().spentness_hash
                && progress.phase == Phase::Applying
                && tip == Some(progress.height)
                && (progress.height + 1) == Some(height) =>
        {
            Ok(Cursor {
                ordinal: progress.next_ordinal,
                omitted: progress.omitted,
                resolved: progress.resolved,
            })
        }
        _ => Err(MISMATCH),
    }
}

/// The `Complete` record for the block at `H`, after the terminal checks.
fn terminal_progress(
    artifact: &VerifiedArtifact,
    hash: block::Hash,
    cursor: Cursor,
) -> Result<Progress, SpentnessError> {
    let header = artifact.header();
    if hash != header.terminal_hash {
        return Err(SpentnessError::Inconsistent(
            "the block at the terminal height is not the artifact's terminal block",
        ));
    }
    if cursor.ordinal != header.output_count {
        return Err(SpentnessError::Ordinal {
            ordinal: cursor.ordinal,
        });
    }
    if cursor.resolved != cursor.omitted {
        return Err(SpentnessError::UnbalancedAtTerminal {
            omitted: cursor.omitted,
            resolved: cursor.resolved,
        });
    }

    Ok(Progress {
        phase: Phase::Complete,
        height: header.terminal_height,
        next_ordinal: cursor.ordinal,
        omitted: cursor.omitted,
        resolved: cursor.resolved,
    })
}

/// Finds an omitted output created by an earlier block in the journal.
#[allow(clippy::unwrap_in_result)]
fn journal_output(
    db: &ZebraDb,
    outpoint: &transparent::OutPoint,
) -> Option<(OutputLocation, transparent::Utxo)> {
    let location = OutputLocation::from_outpoint(db.transaction_location(outpoint.hash)?, outpoint);
    let journal = db
        .db
        .cf_handle(SPENTNESS_JOURNAL)
        .expect("the spentness_journal column family is declared");
    let output: transparent::Output = db.db.zs_get(&journal, &location)?;

    Some((
        location,
        transparent::Utxo::new(
            output,
            location.height(),
            location.transaction_index().as_usize() == 0,
        ),
    ))
}

/// Every transparent output location of `block`, in canonical order.
fn output_locations(
    block: &block::Block,
    height: Height,
) -> impl Iterator<Item = OutputLocation> + '_ {
    block
        .transactions
        .iter()
        .enumerate()
        .flat_map(move |(tx_index, transaction)| {
            (0..transaction.outputs().len())
                .map(move |output_index| OutputLocation::from_usize(height, tx_index, output_index))
        })
}

/// The artifact bit at `ordinal`.
fn retains(artifact: &VerifiedArtifact, ordinal: u64) -> Result<bool, SpentnessError> {
    artifact
        .retains(ordinal)
        .map_err(|_| SpentnessError::Ordinal { ordinal })
}

/// Converts a collection length to a counter value.
fn count(len: usize) -> u64 {
    // Supported targets have at most 64-bit pointers, so every length fits in u64.
    len as u64
}
