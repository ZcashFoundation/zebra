//! Async Halo2 batch verifier service

use std::{
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;
use orchard::{
    bundle::BatchValidator,
    circuit::{FixedPostNu6_2, InsecurePreNu6_2, VerifyingKey},
};
use rand::thread_rng;
use zcash_protocol::value::ZatBalance;
use zebra_chain::transaction::SigHash;

use crate::BoxError;
use thiserror::Error;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use tower_fallback::Fallback;

use super::spawn_fifo;

/// Adjusted batch size for halo2 batches.
///
/// Unlike other batch verifiers, halo2 has aggregate proofs.
/// This means that there can be hundreds of actions verified by some proofs,
/// but just one action in others.
///
/// To compensate for larger proofs, we process the batch once there are over
/// [`HALO2_MAX_BATCH_SIZE`] total actions among pending items in the queue.
const HALO2_MAX_BATCH_SIZE: usize = super::MAX_BATCH_SIZE;

/// The type of verification results.
type VerifyResult = bool;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/// Temporary substitute type for fake batch verification.
///
/// TODO: implement batch verification
pub type BatchVerifyingKey = ItemVerifyingKey;

/// The type of a prepared verifying key.
/// This is the key used to verify individual items.
pub type ItemVerifyingKey = VerifyingKey;

// NU6.2 re-enables Orchard actions and ships the *fixed* variable-base
// scalar-multiplication Orchard circuit (the circuit bug that caused Orchard to be
// temporarily disabled; see GHSA-2x4w-pxqw-58v9). The fix changes the Orchard Action
// circuit, and therefore its verifying key: a proof produced under one circuit version
// does not verify under the other key. So we must keep BOTH keys and select per bundle by
// the block's network upgrade (era):
//
//   * Orchard bundles mined before NU6.2 (NU5..NU6.2) were produced by the historical,
//     insecure circuit and only verify under the [`InsecurePreNu6_2`] key. These must keep
//     verifying so that nodes can re-sync and reindex pre-soft-fork Orchard history.
//
//   * Orchard bundles mined at NU6.2 onward are produced by the fixed circuit and only
//     verify under the [`FixedPostNu6_2`] key.
//
// The era is threaded in from transaction verification
// (`zebra-consensus/src/transaction.rs`, via `request.upgrade(network)`) through
// [`Item`] into the [`Verifier`], which keeps a separate batch per era and validates each
// batch against the matching key.
//
// NOTE: this deliberately does NOT copy zcashd PR #176's WIP shortcut of validating
// everything against the fixed key; that is incorrect for re-syncing pre-soft-fork Orchard
// blocks, whose proofs only verify under the insecure key.
lazy_static::lazy_static! {
    /// The fixed (post-NU6.2) halo2 proof verifying key.
    ///
    /// Built from the fixed variable-base scalar-multiplication Orchard Action circuit
    /// shipped in NU6.2. Use this for Orchard bundles in blocks at or after the NU6.2
    /// activation height.
    pub static ref VERIFYING_KEY_POST_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build::<FixedPostNu6_2>();

    /// The historical, insecure (pre-NU6.2) halo2 proof verifying key.
    ///
    /// Reconstructs the verifying key of the original (NU5..NU6.2) Orchard Action circuit.
    /// Use this ONLY to verify Orchard bundles in blocks mined before the NU6.2 activation
    /// height, so that pre-soft-fork Orchard history can be re-synced and reindexed. It must
    /// never be used to verify post-NU6.2 bundles.
    pub static ref VERIFYING_KEY_PRE_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build::<InsecurePreNu6_2>();
}

/// The Orchard circuit era of a bundle, which selects the verifying key it is checked
/// against.
///
/// The Orchard Action circuit — and therefore its verifying key — changed at NU6.2 (the
/// fixed variable-base scalar-multiplication circuit; see GHSA-2x4w-pxqw-58v9). A proof
/// produced under one era does not verify under the other era's key, so each bundle must be
/// checked against the key for the era of the block it appears in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum OrchardEra {
    /// Blocks before the NU6.2 activation height, verified with [`VERIFYING_KEY_PRE_NU6_2`].
    PreNu6_2,
    /// Blocks at or after the NU6.2 activation height, verified with
    /// [`VERIFYING_KEY_POST_NU6_2`].
    PostNu6_2,
}

impl OrchardEra {
    /// Returns the verifying key for this era.
    fn verifying_key(self) -> &'static ItemVerifyingKey {
        match self {
            OrchardEra::PreNu6_2 => &VERIFYING_KEY_PRE_NU6_2,
            OrchardEra::PostNu6_2 => &VERIFYING_KEY_POST_NU6_2,
        }
    }
}

/// A Halo2 verification item, used as the request type of the service.
#[derive(Clone, Debug)]
pub struct Item {
    bundle: orchard::bundle::Bundle<orchard::bundle::Authorized, ZatBalance>,
    sighash: SigHash,
    /// The Orchard circuit era of the block this bundle appears in, which selects the
    /// verifying key. See [`OrchardEra`].
    era: OrchardEra,
}

impl RequestWeight for Item {
    fn request_weight(&self) -> usize {
        self.bundle.actions().len()
    }
}

impl Item {
    /// Creates a new [`Item`] from a bundle, sighash, and the Orchard circuit era of the
    /// block the bundle appears in.
    pub fn new(
        bundle: orchard::bundle::Bundle<orchard::bundle::Authorized, ZatBalance>,
        sighash: SigHash,
        era: OrchardEra,
    ) -> Self {
        Self {
            bundle,
            sighash,
            era,
        }
    }

    /// The Orchard circuit era of this item.
    pub fn era(&self) -> OrchardEra {
        self.era
    }

    /// Perform non-batched verification of this [`Item`] against its era's verifying key.
    ///
    /// This is useful (in combination with `Item::clone`) for implementing
    /// fallback logic when batch verification fails.
    pub fn verify_single(self) -> bool {
        let vk = self.era.verifying_key();
        let mut batch = BatchValidator::default();
        batch.queue(self);
        batch.validate(vk, thread_rng())
    }
}

trait QueueBatchVerify {
    fn queue(&mut self, item: Item);
}

impl QueueBatchVerify for BatchValidator {
    fn queue(
        &mut self,
        Item {
            bundle, sighash, ..
        }: Item,
    ) {
        self.add_bundle(&bundle, sighash.0);
    }
}

/// An error that may occur when verifying [Halo2 proofs of Zcash Orchard Action
/// descriptions][actions].
///
/// [actions]: https://zips.z.cash/protocol/protocol.pdf#actiondesc
// TODO: if halo2::plonk::Error gets the std::error::Error trait derived on it,
// remove this and just wrap `halo2::plonk::Error` as an enum variant of
// `crate::transaction::Error`, which does the trait derivation via `thiserror`
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[allow(missing_docs)]
pub enum Halo2Error {
    #[error("the constraint system is not satisfied")]
    ConstraintSystemFailure,
    #[error("unknown Halo2 error")]
    Other,
}

impl From<halo2::plonk::Error> for Halo2Error {
    fn from(err: halo2::plonk::Error) -> Halo2Error {
        match err {
            halo2::plonk::Error::ConstraintSystemFailure => Halo2Error::ConstraintSystemFailure,
            _ => Halo2Error::Other,
        }
    }
}

/// Global batch verification context for Halo2 proofs of Action statements.
///
/// This service transparently batches contemporaneous proof verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static VERIFIER: Lazy<
    Fallback<
        Batch<Verifier, Item>,
        ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), BoxError>>>,
    >,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::new(),
            HALO2_MAX_BATCH_SIZE,
            None,
            super::MAX_BATCH_LATENCY,
        ),
        // We want to fallback to individual verification if batch verification fails,
        // so we need a Service to use.
        //
        // Because we have to specify the type of a static, we need to be able to
        // write the type of the closure and its return value. But both closures and
        // async blocks have unnameable types. So instead we cast the closure to a function
        // (which is possible because it doesn't capture any state), and use a BoxFuture
        // to erase the result type.
        // (We can't use BoxCloneService to erase the service type, because it is !Sync.)
        //
        // The fallback verifies each item against its own era's verifying key (see
        // [`Item::verify_single`]).
        tower::service_fn(
            (|item: Item| Verifier::verify_single_spawning(item).boxed()) as fn(_) -> _,
        ),
    )
});

/// Halo2 proof verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Halo2 verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    /// The per-era synchronous Halo2 batch validators.
    ///
    /// Orchard's verifying key changed at NU6.2 (see [`OrchardEra`]), and a single
    /// [`BatchValidator`] is validated against exactly one verifying key. So we keep a
    /// separate batch per era and route each incoming [`Item`] into the batch for its era,
    /// flushing each batch against the matching key. This keeps the batching optimization
    /// while never mixing proofs from different circuit eras into a single batch.
    pre_nu6_2: EraBatch,
    post_nu6_2: EraBatch,
}

/// A single-era batch and its result-broadcast channel.
struct EraBatch {
    /// The synchronous Halo2 batch validator for this era.
    batch: BatchValidator,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Default for EraBatch {
    fn default() -> Self {
        let (tx, _) = watch::channel(None);
        Self {
            batch: BatchValidator::default(),
            tx,
        }
    }
}

impl Verifier {
    fn new() -> Self {
        Self {
            pre_nu6_2: EraBatch::default(),
            post_nu6_2: EraBatch::default(),
        }
    }

    /// Returns a mutable reference to the [`EraBatch`] for `era`.
    fn era_batch(&mut self, era: OrchardEra) -> &mut EraBatch {
        match era {
            OrchardEra::PreNu6_2 => &mut self.pre_nu6_2,
            OrchardEra::PostNu6_2 => &mut self.post_nu6_2,
        }
    }

    /// Returns the batch verifier, verifying key, and channel sender for `era`,
    /// replacing the batch and channel with new empty ones.
    fn take(&mut self, era: OrchardEra) -> (BatchValidator, &'static BatchVerifyingKey, Sender) {
        let vk = era.verifying_key();
        let era_batch = self.era_batch(era);

        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut era_batch.batch);
        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut era_batch.tx, tx);

        (batch, vk, tx)
    }

    /// Synchronously process the batch, and send the result using the channel sender.
    /// This function blocks until the batch is completed.
    fn verify(batch: BatchValidator, vk: &'static BatchVerifyingKey, tx: Sender) {
        let result = batch.validate(vk, thread_rng());
        let _ = tx.send(Some(result));
    }

    /// Flush all per-era batches using a thread pool, sending each result via its channel.
    /// This returns immediately, usually before the batches are completed.
    fn flush_blocking(&mut self) {
        for era in [OrchardEra::PreNu6_2, OrchardEra::PostNu6_2] {
            let (batch, vk, tx) = self.take(era);

            // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
            //
            // We don't care about execution order here, because this method is only called on drop.
            tokio::task::block_in_place(|| rayon::spawn_fifo(move || Self::verify(batch, vk, tx)));
        }
    }

    /// Flush the batch using a thread pool, and return the result via the channel.
    /// This function returns a future that becomes ready when the batch is completed.
    async fn flush_spawning(batch: BatchValidator, vk: &'static BatchVerifyingKey, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let start = std::time::Instant::now();
        let result = spawn_fifo(move || batch.validate(vk, thread_rng())).await;
        let duration = start.elapsed().as_secs_f64();

        let result_label = match &result {
            Ok(true) => "success",
            _ => "failure",
        };
        metrics::histogram!(
            "zebra.consensus.batch.duration_seconds",
            "verifier" => "halo2",
            "result" => result_label
        )
        .record(duration);

        let _ = tx.send(result.ok());
    }

    /// Verify a single item using a thread pool, and return the result.
    ///
    /// The item is verified against its own era's verifying key (see [`Item::verify_single`]).
    async fn verify_single_spawning(item: Item) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        if spawn_fifo(move || item.verify_single()).await? {
            Ok(())
        } else {
            Err("could not validate orchard proof".into())
        }
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "Verifier";
        f.debug_struct(name)
            .field("pre_nu6_2", &"..")
            .field("post_nu6_2", &"..")
            .finish()
    }
}

impl Service<BatchControl<Item>> for Verifier {
    type Response = ();
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Item>) -> Self::Future {
        match req {
            BatchControl::Item(item) => {
                let era = item.era();
                tracing::trace!(?era, "got item");
                // Route the item into the batch for its era, and subscribe its future to
                // that era's result channel, so each item is verified against the verifying
                // key for the circuit era of its block.
                let era_batch = self.era_batch(era);
                era_batch.batch.queue(item);
                let mut rx = era_batch.tx.subscribe();
                Box::pin(async move {
                    match rx.changed().await {
                        Ok(()) => {
                            // We use a new channel for each batch,
                            // so we always get the correct batch result here.
                            let is_valid = *rx
                                .borrow()
                                .as_ref()
                                .ok_or("threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?")?;

                            if is_valid {
                                tracing::trace!(?is_valid, "verified halo2 proof");
                                metrics::counter!("proofs.halo2.verified").increment(1);
                                Ok(())
                            } else {
                                tracing::trace!(?is_valid, "invalid halo2 proof");
                                metrics::counter!("proofs.halo2.invalid").increment(1);
                                Err("could not validate halo2 proofs".into())
                            }
                        }
                        Err(_recv_error) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got halo2 flush command");

                // Flush both era batches, each against its own verifying key. The future is
                // ready only once both batches have completed and broadcast their results.
                let (pre_batch, pre_vk, pre_tx) = self.take(OrchardEra::PreNu6_2);
                let (post_batch, post_vk, post_tx) = self.take(OrchardEra::PostNu6_2);

                Box::pin(
                    futures::future::join(
                        Self::flush_spawning(pre_batch, pre_vk, pre_tx),
                        Self::flush_spawning(post_batch, post_vk, post_tx),
                    )
                    .map(|((), ())| Ok(())),
                )
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        // This returns immediately, usually before the batch is completed.
        self.flush_blocking()
    }
}
