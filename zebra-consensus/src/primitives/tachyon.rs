//! Asynchronous verification of tachyon proof stamps (NU7, experimental).
//!
//! Ragu proofs are currently mocks, so this is a plain [`spawn_fifo`] call rather than a
//! `Batch`/`Fallback` service like [`super::halo2`]. Keep [`verify_proof_stamp`] as the only
//! entry point, so a batch verifier can replace the internals without touching callers once
//! ragu grows a batch-verification API.

use crate::{block::tachyon::AggregateCoverage, error::BlockError};

use super::spawn_fifo;

/// Verifies a tachyon aggregate's proof stamp against the actions it covers.
///
/// # Consensus
///
/// > Every proof stamp MUST verify. The validator reassembles the stamp PCD from `proofTachyon`,
/// > `anchorTachyon`, `cTachygrams`, and `cStampActionsTachyon`; reject if any proof fails.
///
/// <https://github.com/turbocrime/tachyon/blob/main/book/src/zips/tachyon-bundle.md>
///
/// `aggregate` groups the proof-stamped bundle with every pointer-stamped bundle naming it; see
/// `block::tachyon::coherence`, which also checks the stamp's coverage digest over the same
/// grouping. Proof verification is CPU-bound, so it runs on the rayon threadpool.
pub async fn verify_proof_stamp(aggregate: AggregateCoverage) -> Result<(), BlockError> {
    spawn_fifo(move || {
        let adjunct_refs: Vec<_> = aggregate
            .adjuncts
            .iter()
            .map(|adjunct| adjunct.as_dyn())
            .collect();

        match aggregate
            .bundle
            .verify_proof(&mut rand::thread_rng(), &adjunct_refs)
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(BlockError::TachyonProofInvalid(
                "proof stamp was disproved".to_string(),
            )),
            Err(err) => Err(BlockError::TachyonProofInvalid(err.to_string())),
        }
    })
    .await
    .map_err(|_| {
        BlockError::Other(
            "threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?"
                .to_string(),
        )
    })?
}
