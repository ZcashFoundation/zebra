//! Mempool transaction verification and state for Zebra.
//!
//! Mempool updates occur in multiple stages:
//!   - getting transactions (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of mempool transactions against the chain state
//!     (awaits an up-to-date chain)
//!   - adding transactions to the mempool
//!
//! The mempool is provided via a `tower::Service`, to support backpressure and batch
//! verification.

/// Mempool state.
///
/// New transactions are verified, checked against the chain state, then added to the
/// mempool.
///
/// `ZebraMempoolState` is not yet implemented.
#[derive(Default)]
struct ZebraMempoolState {}

/// Mempool transaction verification.
///
/// New transactions are verified, checked against the chain state, then added to the
/// mempool.
///
/// `MempoolTransactionVerifier` is not yet implemented.
#[derive(Default)]
struct MempoolTransactionVerifier {}
