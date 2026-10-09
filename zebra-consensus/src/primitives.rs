//! Asynchronous verification of cryptographic primitives.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use once_cell::sync::Lazy;
use tokio::sync::oneshot::error::RecvError;

use crate::BoxError;

mod cache;
pub mod ed25519;
pub mod groth16;
pub mod halo2;
pub mod redjubjub;
pub mod redpallas;
pub mod sapling;

/// The maximum batch size for any of the batch verifiers.
const MAX_BATCH_SIZE: usize = 64;

/// The maximum latency bound for any of the batch verifiers.
const MAX_BATCH_LATENCY: std::time::Duration = std::time::Duration::from_millis(100);

/// Block registrations are scoped by their shared transaction-verification context.
static BLOCK_BATCH_FLUSHES: LazyLock<Mutex<HashMap<BlockVerifierBatchFlushKey, BlockFlush>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Opaque identity shared by all transaction requests from one block verification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BlockVerifierBatchFlushKey(usize);

/// Keeps a block's flush registration alive until its transaction verifications finish.
#[derive(Debug)]
pub(crate) struct BlockVerifierBatchFlushGuard {
    key: BlockVerifierBatchFlushKey,
}

impl Drop for BlockVerifierBatchFlushGuard {
    fn drop(&mut self) {
        BLOCK_BATCH_FLUSHES
            .lock()
            .expect("block batch registry is not held across verification or await points")
            .remove(&self.key);
    }
}

#[derive(Debug)]
struct BlockFlush {
    expected_transactions: usize,
    started_transactions: usize,
    flush_queued: bool,
}

/// Registers the transactions that will be dispatched for one semantic block verification.
pub(crate) fn register_block_verifier_batch_flush<T>(
    shared_block_context: &Arc<T>,
    expected_transactions: usize,
) -> BlockVerifierBatchFlushGuard {
    let key = block_verifier_batch_flush_key(shared_block_context);
    BLOCK_BATCH_FLUSHES
        .lock()
        .expect("block batch registry is not held across verification or await points")
        .insert(
            key,
            BlockFlush {
                expected_transactions,
                started_transactions: 0,
                flush_queued: expected_transactions == 0,
            },
        );
    BlockVerifierBatchFlushGuard { key }
}

/// Returns the identity of a block's shared transaction-verification context.
pub(crate) fn block_verifier_batch_flush_key<T>(
    shared_block_context: &Arc<T>,
) -> BlockVerifierBatchFlushKey {
    // The allocation address is only an identity, never dereferenced or used for arithmetic.
    BlockVerifierBatchFlushKey(Arc::as_ptr(shared_block_context) as usize)
}

/// Records a transaction's initial async-check polling and queues the block's flush once.
pub(crate) fn start_block_transaction_async_checks(key: BlockVerifierBatchFlushKey) {
    if block_verifier_batch_flush_ready(key) {
        flush_block_verifier_batches();
    }
}

fn block_verifier_batch_flush_ready(key: BlockVerifierBatchFlushKey) -> bool {
    let mut flushes = BLOCK_BATCH_FLUSHES
        .lock()
        .expect("block batch registry is not held across verification or await points");
    let Some(flush) = flushes.get_mut(&key) else {
        return false;
    };
    flush.started_transactions = flush.started_transactions.saturating_add(1);
    if flush.flush_queued || flush.started_transactions < flush.expected_transactions {
        return false;
    }
    flush.flush_queued = true;
    true
}

/// Starts partial batches without waiting for the latency timer or initializing unused workers.
///
/// Flush commands follow already-enqueued checks on each worker's FIFO. Saturation falls back
/// to normal size/timed flushing. Transaction futures still await every verification result.
fn flush_block_verifier_batches() {
    if let Some(verifier) = Lazy::get(&ed25519::VERIFIER) {
        queue_batch_flush("ed25519", verifier.primary().clone().try_flush());
    }
    if let Some(verifier) = Lazy::get(&redjubjub::VERIFIER) {
        queue_batch_flush("redjubjub", verifier.primary().clone().try_flush());
    }
    if let Some(verifier) = Lazy::get(&redpallas::VERIFIER) {
        queue_batch_flush("redpallas", verifier.primary().clone().try_flush());
    }
    if let Some(verifier) = Lazy::get(&sapling::VERIFIER) {
        queue_batch_flush("sapling", verifier.inner().primary().clone().try_flush());
    }
    for (name, verifier) in [
        ("halo2_pre_nu6_2", Lazy::get(&halo2::VERIFIER_PRE_NU6_2)),
        ("halo2_nu6_2", Lazy::get(&halo2::VERIFIER_NU6_2)),
        ("halo2_nu6_3", Lazy::get(&halo2::VERIFIER_NU6_3_ONWARD)),
    ] {
        if let Some(verifier) = verifier {
            queue_batch_flush(name, verifier.primary().clone().try_flush());
        }
    }
}

fn queue_batch_flush(verifier: &'static str, result: Result<bool, BoxError>) {
    match result {
        Ok(true) => {}
        Ok(false) => tracing::trace!(verifier, "batch saturated, retaining timed flush"),
        Err(error) => tracing::trace!(?error, verifier, "could not queue block batch flush"),
    }
}

#[cfg(test)]
mod tests;

/// Fires off a task into the Rayon threadpool, awaits the result through a oneshot channel,
/// then converts the error to a [`BoxError`].
pub async fn spawn_fifo_and_convert<
    E: 'static + std::error::Error + Into<BoxError> + Sync + Send,
    F: 'static + FnOnce() -> Result<(), E> + Send,
>(
    f: F,
) -> Result<(), BoxError> {
    spawn_fifo(f)
        .await
        .map_err(|_| {
            "threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?"
        })?
        .map_err(BoxError::from)
}

/// Fires off a task into the Rayon threadpool and awaits the result through a oneshot channel.
pub async fn spawn_fifo<T: 'static + Send, F: 'static + FnOnce() -> T + Send>(
    f: F,
) -> Result<T, RecvError> {
    // Rayon doesn't have a spawn function that returns a value,
    // so we use a oneshot channel instead.
    let (rsp_tx, rsp_rx) = tokio::sync::oneshot::channel();

    rayon::spawn_fifo(move || {
        let _ = rsp_tx.send(f());
    });

    rsp_rx.await
}
