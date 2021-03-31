//! Asynchronous verification of cryptographic primitives.

pub mod ed25519;
pub mod groth16;
pub mod redjubjub;

/// The maximum batch size for any of the batch verifiers.
const MAX_BATCH_SIZE: usize = 64;

/// The maximum latency bound for any of the batch verifiers.
const MAX_BATCH_LATENCY: std::time::Duration = std::time::Duration::from_millis(100);

/// The size of the buffer in the broadcast channels used by batch verifiers.
///
/// This bound limits the number of concurrent batches for each verifier.
/// If tasks delay checking for verifier results, and the bound is too small,
/// new batches will be rejected with `RecvError`s.
const BROADCAST_BUFFER_SIZE: usize = 512;
