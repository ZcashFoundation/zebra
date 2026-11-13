//! Descriptions of the metrics this crate records.
//!
//! Applications that install a [`metrics`] recorder call [`describe`] right after installing it, so
//! that exporters can document each metric, for example in Prometheus `# HELP` lines. They also
//! call [`register_failure_counters`], so that exporters publish the failure counters before the
//! first failure.

use metrics::{describe_counter, describe_gauge, describe_histogram, Unit};

/// The label keys of each labelled metric this crate records.
///
/// - `zebra.consensus.batch.duration_seconds`:
///   - `verifier`: `ed25519`, `redjubjub`, `redpallas`, `halo2`, or `groth16_sapling`.
///   - `result`: `success` or `failure`.
/// - `zebra.consensus.cache.*`:
///   - `verifier`: `halo2_pre_nu6_2`, `halo2_nu6_2`, `halo2_nu6_3_onward`, or `groth16_sapling`.
pub const LABELS: &[(&str, &[&str])] = &[
    (
        "zebra.consensus.batch.duration_seconds",
        &["verifier", "result"],
    ),
    ("zebra.consensus.cache.hit", &["verifier"]),
    ("zebra.consensus.cache.miss", &["verifier"]),
    ("zebra.consensus.cache.insert", &["verifier"]),
    ("zebra.consensus.cache.evict", &["verifier"]),
    ("zebra.consensus.cache.size", &["verifier"]),
];

/// The counters of signatures and proofs that failed verification.
///
/// They are only recorded when a verification fails, and `rate()` can't see the first increase of
/// a series that didn't exist before, so [`register_failure_counters`] registers them at zero.
pub const FAILURE_COUNTERS: &[&str] = &[
    "signatures.ed25519.invalid",
    "signatures.redjubjub.invalid",
    "signatures.redpallas.invalid",
    "proofs.halo2.invalid",
    "proofs.sapling.invalid",
];

/// Registers every counter in [`FAILURE_COUNTERS`] with the installed [`metrics`] recorder,
/// without changing its value, so that exporters publish it at zero before the first failure.
pub fn register_failure_counters() {
    for name in FAILURE_COUNTERS {
        metrics::counter!(*name).increment(0);
    }
}

/// Describes every metric this crate records to the installed [`metrics`] recorder.
pub fn describe() {
    // Checkpoint verifier
    describe_gauge!(
        "checkpoint.verified.height",
        Unit::Count,
        "Height of the latest block verified by the checkpoint verifier."
    );
    describe_gauge!(
        "checkpoint.verified.count",
        Unit::Count,
        "Number of checkpoints verified so far."
    );
    describe_counter!(
        "checkpoint.verified.block.count",
        Unit::Count,
        "Blocks verified against checkpoints."
    );
    describe_gauge!(
        "checkpoint.processing.next.height",
        Unit::Count,
        "Height of the next checkpoint the checkpoint verifier is waiting to reach."
    );
    describe_gauge!(
        "checkpoint.queued.max.height",
        Unit::Count,
        "Highest block height queued in the checkpoint verifier, or NaN when none are queued."
    );
    describe_gauge!(
        "checkpoint.queued.continuous.height",
        Unit::Count,
        "Height of the end of the continuous run of queued blocks after the verified tip."
    );
    describe_gauge!(
        "checkpoint.queued_slots",
        Unit::Count,
        "Number of heights with blocks queued in the checkpoint verifier."
    );
    describe_counter!(
        "checkpoint.waiting.count",
        Unit::Count,
        "Times the checkpoint verifier waited for more blocks before it could verify a range."
    );

    // Batch verifiers
    describe_histogram!(
        "zebra.consensus.batch.duration_seconds",
        Unit::Seconds,
        "Time taken to verify one batch of signatures or proofs."
    );
    describe_counter!(
        "signatures.ed25519.validated",
        Unit::Count,
        "Ed25519 signatures that passed batch verification."
    );
    describe_counter!(
        "signatures.ed25519.invalid",
        Unit::Count,
        "Ed25519 signatures that failed verification."
    );
    describe_counter!(
        "signatures.redjubjub.validated",
        Unit::Count,
        "RedJubjub signatures that passed batch verification."
    );
    describe_counter!(
        "signatures.redjubjub.invalid",
        Unit::Count,
        "RedJubjub signatures that failed verification."
    );
    describe_counter!(
        "signatures.redpallas.validated",
        Unit::Count,
        "RedPallas signatures that passed batch verification."
    );
    describe_counter!(
        "signatures.redpallas.invalid",
        Unit::Count,
        "RedPallas signatures that failed verification."
    );
    describe_counter!(
        "proofs.halo2.verified",
        Unit::Count,
        "Halo2 proofs that passed batch verification."
    );
    describe_counter!(
        "proofs.halo2.invalid",
        Unit::Count,
        "Halo2 proofs that failed verification."
    );
    describe_counter!(
        "proofs.sapling.verified",
        Unit::Count,
        "Sapling Groth16 proofs that passed batch verification."
    );
    describe_counter!(
        "proofs.sapling.invalid",
        Unit::Count,
        "Sapling Groth16 proofs that failed verification."
    );

    // Verification caches
    describe_counter!(
        "zebra.consensus.cache.hit",
        Unit::Count,
        "Verifications answered from a verification cache."
    );
    describe_counter!(
        "zebra.consensus.cache.miss",
        Unit::Count,
        "Verifications that a verification cache passed to its verifier."
    );
    describe_counter!(
        "zebra.consensus.cache.insert",
        Unit::Count,
        "Keys recorded as verified in a verification cache."
    );
    describe_counter!(
        "zebra.consensus.cache.evict",
        Unit::Count,
        "Keys dropped from a verification cache to stay within its capacity."
    );
    describe_gauge!(
        "zebra.consensus.cache.size",
        Unit::Count,
        "Number of keys a verification cache currently remembers."
    );
}
