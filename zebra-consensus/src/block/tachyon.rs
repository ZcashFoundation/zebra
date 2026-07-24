//! Block-level tachyon consensus rules (NU7, experimental).
//!
//! The tachyon bundle spec mandates four block-validity rules, applied in order:
//!
//! 1. All tachygrams in a block MUST be distinct.
//! 2. Every pointer-stamped transaction MUST bear a `tachyonAggregateId` referring to the
//!    proof-stamped transaction in the same block covering its actions.
//! 3. For each proof stamp, the descriptor digest over the bundle's own actions together with
//!    those of every pointer-stamped transaction naming it MUST match the carried
//!    `hStampActionsTachyon`.
//! 4. Every proof stamp MUST verify.
//!
//! <https://github.com/turbocrime/tachyon/blob/main/book/src/zips/tachyon-bundle.md>
//!
//! [`coherence`] performs rules 1-3 in a cheap synchronous scan, and returns the work items for
//! rule 4, which the block verifier runs asynchronously via
//! [`crate::primitives::tachyon::verify_proof_stamp`].

use std::collections::{BTreeSet, HashMap};

use zebra_chain::{block::Block, transaction::WtxId};

use zcash_tachyon::{action::Descriptor, ProofStamp, Tachygram, TachyonBundle};

use crate::error::BlockError;

#[cfg(test)]
mod tests;

/// A tachyon proof stamp and the full covered action descriptor set it must verify against:
/// the stamp's own bundle actions plus those of every pointer-stamped transaction naming it.
#[derive(Debug)]
pub(crate) struct AggregateCoverage {
    /// The proof stamp to verify.
    pub stamp: ProofStamp,

    /// The descriptors of every action the stamp covers.
    pub covered_descriptors: Vec<Descriptor>,
}

/// Checks the synchronous block-level tachyon rules (1-3 in the module docs), and returns the
/// proof-verification work items for rule 4.
///
/// # Consensus
///
/// > All tachygrams in a block MUST be distinct.
///
/// > Every pointer-stamped transaction MUST bear a `tachyonAggregateId` referring to the
/// > proof-stamped transaction in the same block covering its actions
///
/// > For each proof stamp, collect the descriptors of the bundle's own actions together with
/// > those of every pointer-stamped transaction naming it, sort them, and compute the descriptor
/// > digest; reject on mismatch with the carried `hStampActionsTachyon`.
///
/// <https://github.com/turbocrime/tachyon/blob/main/book/src/zips/tachyon-bundle.md>
pub(crate) fn coherence(block: &Block) -> Result<Vec<AggregateCoverage>, BlockError> {
    let mut seen_tachygrams: BTreeSet<Tachygram> = BTreeSet::new();

    // Each proof-stamped bundle's stamp and covered descriptors (starting with its own actions),
    // keyed by position, plus a wtxid index into it for pointer-stamp resolution. Duplicate
    // wtxids can't occur here: the block verifier already rejects duplicate transactions, and a
    // wtxid collision between distinct transactions is a hash collision.
    let mut aggregates: Vec<AggregateCoverage> = Vec::new();
    let mut aggregates_by_wtxid: HashMap<[u8; 64], usize> = HashMap::new();

    // The pointer-stamped bundles' targets and action descriptors.
    let mut adjuncts: Vec<([u8; 64], Vec<Descriptor>)> = Vec::new();

    for transaction in &block.transactions {
        let Some(tachyon_shielded_data) = transaction.tachyon_shielded_data() else {
            continue;
        };

        match &tachyon_shielded_data.0 {
            // Zebra stores an absent bundle as `None`, so this variant is unreachable here, and
            // an empty bundle has no tachygrams or actions to check anyway.
            TachyonBundle::NoBundle => {}

            TachyonBundle::Proven(bundle) => {
                // Rule 1: block-wide tachygram distinctness, in a single scan over the proof
                // stamps (pointer-stamped bundles carry no tachygrams). This also subsumes the
                // per-stamp distinctness rule for mined blocks.
                for &tachygram in &bundle.stamp.tachygrams {
                    if !seen_tachygrams.insert(tachygram) {
                        return Err(BlockError::DuplicateTachygram);
                    }
                }

                // TODO: the V7 auth digest does not include the tachyon bundle's `auth_digest()`
                // contribution yet, so these wtxids are provisional; they are internally
                // consistent with the wtxids that pointer stamps in this Zebra fork carry.
                let wtxid = WtxId::from(transaction.as_ref()).as_bytes();

                aggregates_by_wtxid.insert(wtxid, aggregates.len());
                aggregates.push(AggregateCoverage {
                    stamp: bundle.stamp.clone(),
                    covered_descriptors: bundle.descriptors(),
                });
            }

            TachyonBundle::Adjunct(bundle) => {
                adjuncts.push((<[u8; 64]>::from(bundle.stamp), bundle.descriptors()));
            }
        }
    }

    // Rule 2: every pointer stamp resolves to a proof-stamped transaction in this block. A
    // pointer to a non-proof-stamped transaction is inherently absent from the index. Resolved
    // adjunct actions accumulate into their aggregate's covered descriptor set for rule 3.
    for (target_wtxid, mut descriptors) in adjuncts {
        let Some(&aggregate_index) = aggregates_by_wtxid.get(&target_wtxid) else {
            return Err(BlockError::TachyonAggregateNotFound);
        };

        aggregates[aggregate_index]
            .covered_descriptors
            .append(&mut descriptors);
    }

    // Rule 3: each proof stamp's carried `hStampActionsTachyon` matches the digest of its
    // covered descriptors. `covers` sorts the descriptors internally, so the accumulation order
    // above doesn't matter. An autonome with no adjuncts simply covers its own actions.
    for aggregate in &aggregates {
        if !aggregate.stamp.covers(&aggregate.covered_descriptors) {
            return Err(BlockError::TachyonCoverageMismatch);
        }
    }

    Ok(aggregates)
}
