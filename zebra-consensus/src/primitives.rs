//! Asynchronous verification of cryptographic primitives.

pub mod ed25519;
pub mod groth16;
pub mod halo2;
pub mod redjubjub;
pub mod redpallas;

/// The maximum batch size for any of the batch verifiers.
const MAX_BATCH_SIZE: usize = 64;

/// The maximum latency bound for any of the batch verifiers.
const MAX_BATCH_LATENCY: std::time::Duration = std::time::Duration::from_millis(100);
