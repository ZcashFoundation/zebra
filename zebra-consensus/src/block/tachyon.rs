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

use zcash_tachyon::{
    stamp::StampState as _, Bundle, PointerStamp, ProofStamp, Tachygram, TachyonBundle,
    VerifyCoverageError,
};

use crate::error::BlockError;

#[cfg(test)]
mod tests;

/// A tachyon aggregate and the pointer-stamped bundles it covers: the proof-verification work
/// item for one proof stamp (rule 4 in the module docs).
#[derive(Debug)]
pub(crate) struct AggregateCoverage {
    /// The proof-stamped bundle whose stamp must verify.
    pub bundle: Bundle<ProofStamp>,

    /// The pointer-stamped bundles naming this aggregate, whose actions the stamp also covers.
    pub adjuncts: Vec<Bundle<PointerStamp>>,
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
/// > those of every pointer-stamped transaction naming it, and compute the descriptor digest;
/// > reject on a duplicate descriptor or a mismatch with the carried `hStampActionsTachyon`.
///
/// <https://github.com/turbocrime/tachyon/blob/main/book/src/zips/tachyon-bundle.md>
pub(crate) fn coherence(block: &Block) -> Result<Vec<AggregateCoverage>, BlockError> {
    let mut seen_tachygrams: BTreeSet<Tachygram> = BTreeSet::new();

    // Each proof-stamped bundle keyed by position, plus a wtxid index into it for pointer-stamp
    // resolution. Duplicate wtxids can't occur here: the block verifier already rejects
    // duplicate transactions, and a wtxid collision between distinct transactions is a hash
    // collision.
    let mut aggregates: Vec<AggregateCoverage> = Vec::new();
    let mut aggregates_by_wtxid: HashMap<[u8; 64], usize> = HashMap::new();

    // The pointer-stamped bundles, with the aggregate wtxid their stamp names.
    let mut adjuncts: Vec<([u8; 64], Bundle<PointerStamp>)> = Vec::new();

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
                // stamps (pointer-stamped bundles carry no tachygrams). Distinctness within one
                // stamp is structural: its tachygrams parse into a set.
                for &tachygram in &bundle.stamp.tachygrams {
                    if !seen_tachygrams.insert(tachygram) {
                        return Err(BlockError::DuplicateTachygram);
                    }
                }

                // The V7 auth digest includes the tachyon bundle's `auth_digest()` contribution,
                // so these wtxids commit to the aggregate's full bundle (signatures and stamp
                // included), unambiguously pinning the specific auth state a pointer stamp names.
                let wtxid = WtxId::from(transaction.as_ref()).as_bytes();

                aggregates_by_wtxid.insert(wtxid, aggregates.len());
                aggregates.push(AggregateCoverage {
                    bundle: bundle.clone(),
                    adjuncts: Vec::new(),
                });
            }

            TachyonBundle::Adjunct(bundle) => {
                // A pointer stamp's digest is the aggregate wtxid it names.
                adjuncts.push((bundle.stamp.stamp_digest(), bundle.clone()));
            }
        }
    }

    // Rule 2: every pointer stamp resolves to a proof-stamped transaction in this block. A
    // pointer to a non-proof-stamped transaction is inherently absent from the index. Resolved
    // adjuncts group under their aggregate for rules 3 and 4.
    for (target_wtxid, bundle) in adjuncts {
        let Some(&aggregate_index) = aggregates_by_wtxid.get(&target_wtxid) else {
            return Err(BlockError::TachyonAggregateNotFound);
        };

        aggregates[aggregate_index].adjuncts.push(bundle);
    }

    // Rule 3: each proof stamp's carried `hStampActionsTachyon` matches the digest of the
    // combined unique descriptors of its own and its adjuncts' actions. An autonome with no
    // adjuncts simply covers its own actions. (The pointer check inside `Bundle::verify` is
    // unnecessary here: adjuncts were grouped by the very wtxid it would recheck.)
    for aggregate in &aggregates {
        let adjunct_refs: Vec<_> = aggregate
            .adjuncts
            .iter()
            .map(|adjunct| adjunct.as_dyn())
            .collect();

        aggregate
            .bundle
            .verify_coverage(&adjunct_refs)
            .map_err(|err| match err {
                VerifyCoverageError::DuplicateActions => BlockError::TachyonDuplicateAction,
                VerifyCoverageError::StampActionsMismatch => BlockError::TachyonCoverageMismatch,
            })?;
    }

    Ok(aggregates)
}
