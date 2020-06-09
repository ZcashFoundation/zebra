//! Transaction verification for Zebra.
//!
//! Verification occurs in multiple stages:
//!   - getting transactions from blocks or the mempool (disk- or network-bound)
//!   - context-free verification of signatures, proofs, and scripts (CPU-bound)
//!   - context-dependent verification of transactions against the chain state
//!     (awaits an up-to-date chain)
//!
//! Verification is provided via a `tower::Service`, to support backpressure and batch
//! verification.
//!
//! This is an internal module. Use `verify::BlockVerifier` for blocks and their
//! transactions, or `mempool::MempoolTransactionVerifier` for mempool transactions.

/// Internal transaction verification service.
///
/// After verification, the transaction future completes. State changes are handled by
/// `BlockVerifier` or `MempoolTransactionVerifier`.
///
/// `TransactionVerifier` is not yet implemented.
#[derive(Default)]
pub(crate) struct TransactionVerifier {}
