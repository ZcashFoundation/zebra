//! Asynchronous verification of cryptographic primitives.

use tokio::sync::oneshot::error::RecvError;

use crate::BoxError;

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
