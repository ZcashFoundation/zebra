//! Asynchronous verification of tachyon proof stamps (NU7, experimental).
//!
//! Ragu proofs are currently mocks, so this is a plain [`spawn_fifo`] call rather than a
//! `Batch`/`Fallback` service like [`super::halo2`]. Keep [`verify_proof_stamp`] as the only
//! entry point, so a batch verifier can replace the internals without touching callers once
//! ragu grows a batch-verification API.

use zcash_tachyon::{action::Descriptor, ProofStamp};

use crate::error::BlockError;

use super::spawn_fifo;

/// Verifies a tachyon proof stamp against the actions it must cover.
///
/// # Consensus
///
/// > Every proof stamp MUST verify. The validator reassembles the stamp PCD from `proofTachyon`,
/// > `anchorTachyon`, `cTachygrams`, and `cStampActionsTachyon`; reject if any proof fails.
///
/// <https://github.com/turbocrime/tachyon/blob/main/book/src/zips/tachyon-bundle.md>
///
/// `covered_descriptors` must contain the descriptors of the bundle's own actions plus those of
/// every pointer-stamped transaction naming this stamp's transaction; see
/// `block::tachyon::coherence`. Proof verification is CPU-bound, so it runs on the rayon
/// threadpool.
pub async fn verify_proof_stamp(
    stamp: ProofStamp,
    covered_descriptors: Vec<Descriptor>,
) -> Result<(), BlockError> {
    spawn_fifo(move || {
        stamp
            .verify(&mut rand::thread_rng(), &covered_descriptors)
            .map_err(|err| BlockError::TachyonProofInvalid(err.to_string()))
    })
    .await
    .map_err(|_| {
        BlockError::Other(
            "threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?"
                .to_string(),
        )
    })?
}
