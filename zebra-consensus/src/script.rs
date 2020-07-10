//! Script verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting transactions from blocks or the mempool (disk- or network-bound)
//!   - context-free verification of scripts, signatures, and proofs (CPU-bound)
//!   - context-dependent verification of transactions against the chain state
//!     (awaits an up-to-date chain)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.
//!
//! This is an internal module. Use `verify::BlockVerifier` for blocks and their
//! transactions, or `mempool::MempoolTransactionVerifier` for mempool transactions.

/// Internal script verification service.
///
/// After verification, the script future completes. State changes are handled by
/// `BlockVerifier` or `MempoolTransactionVerifier`.
///
/// `ScriptVerifier` is not yet implemented.
#[derive(Default)]
pub(crate) struct ScriptVerifier {}
