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
    circuit::{OrchardCircuitVersion, VerifyingKey},
};
use rand::thread_rng;
use zcash_protocol::value::ZatBalance;
use zebra_chain::{parameters::NetworkUpgrade, transaction::SigHash};

use crate::BoxError;
use thiserror::Error;
use tokio::sync::watch;
use tower::Service;
use tower_batch_control::{Batch, BatchControl, RequestWeight};
use tower_fallback::Fallback;

use super::spawn_fifo;

#[cfg(test)]
mod tests;

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

/// The type of a prepared verifying key.
/// This is the key used to verify individual items.
pub type ItemVerifyingKey = VerifyingKey;

// NU6.2 re-enables Orchard actions and ships the *fixed* variable-base
// scalar-multiplication Orchard circuit (the circuit bug that caused Orchard to be temporarily
// disabled; see GHSA-jfw5-j458-pfv6). The fix changes the Orchard Action circuit, and therefore
// its verifying key: a proof produced under one circuit version does not verify under the other
// key. So we keep BOTH keys, each in its own dedicated verifier, and route each bundle to the
// correct verifier by the block's network upgrade (see [`verifier_for`]):
//
//   * Orchard bundles mined before NU6.2 (NU5..NU6.2) were produced by the historical, insecure
//     circuit and only verify under the [`InsecurePreNu6_2`] key. These must keep verifying so
//     that nodes can re-sync and reindex pre-soft-fork Orchard history.
//
//   * Orchard bundles mined at NU6.2 onward are produced by the fixed circuit and only verify
//     under the [`FixedPostNu6_2`] key.
//
// NOTE: this deliberately does NOT copy zcashd PR #176's WIP shortcut of validating everything
// against the fixed key; that is incorrect for re-syncing pre-soft-fork Orchard blocks, whose
// proofs only verify under the insecure key.
lazy_static::lazy_static! {
    /// The Orchard Action verifying key for the **pre-NU6.2** (insecure) circuit.
    ///
    /// Reconstructs the verifying key of the original (NU5..NU6.2) Orchard Action circuit.
    /// Bundles mined before NU6.2 committed to this circuit and only verify under this key, so it
    /// MUST be retained to re-verify pre-NU6.2 history on resync. It must never be used to verify
    /// post-NU6.2 bundles.
    pub static ref VERIFYING_KEY_PRE_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build_for_version(OrchardCircuitVersion::InsecurePreNu6_2);

    /// The Orchard Action verifying key for the **NU6.2+** (fixed) circuit.
    ///
    /// Built from the fixed variable-base scalar-multiplication Orchard Action circuit shipped in
    /// NU6.2. Bundles mined at or after the NU6.2 activation height commit to this circuit and
    /// only verify under this key. See [`VERIFYING_KEY_PRE_NU6_2`] for the era split.
    pub static ref VERIFYING_KEY_POST_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build();
}

/// A Halo2 verification item, used as the request type of the service.
///
/// An [`Item`] is key-agnostic: it carries only the bundle and sighash. The verifying key (pre-
/// vs post-NU6.2) is supplied by whichever [`Verifier`] processes the item, so an item is always
/// validated against exactly one key and eras are never mixed within a batch.
#[derive(Clone, Debug)]
pub struct Item {
    bundle: orchard::bundle::Bundle<orchard::bundle::Authorized, ZatBalance>,
    sighash: SigHash,
}

impl RequestWeight for Item {
    fn request_weight(&self) -> usize {
        self.bundle.actions().len()
    }
}

impl Item {
    /// Creates a new [`Item`] from a bundle and sighash.
    pub fn new(
        bundle: orchard::bundle::Bundle<orchard::bundle::Authorized, ZatBalance>,
        sighash: SigHash,
    ) -> Self {
        Self { bundle, sighash }
    }

    /// Perform non-batched verification of this [`Item`] against `vk`.
    ///
    /// This is useful (in combination with `Item::clone`) for implementing
    /// fallback logic when batch verification fails. The caller supplies the
    /// verifying key for the item's era.
    pub fn verify_single(self, vk: &ItemVerifyingKey) -> bool {
        let mut batch = BatchValidator::default();
        batch.queue(self);
        batch.validate(vk, thread_rng())
    }
}

trait QueueBatchVerify {
    fn queue(&mut self, item: Item);
}

impl QueueBatchVerify for BatchValidator {
    fn queue(&mut self, Item { bundle, sighash }: Item) {
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

/// The single-item fallback service for one Orchard circuit era.
///
/// When a batch fails, [`Fallback`] re-runs each item individually through this service. It holds
/// the *same* verifying key as the batch it backs, so the fallback can never validate an item
/// against a different era's key than the batch did. The key is named once, when the verifier is
/// built (see [`batch_verifier`]).
///
/// This is a tiny named service rather than a `service_fn` closure because the closure would have
/// to capture `vk`, and a capturing closure has an unnameable, non-`Clone` type — but the global
/// verifier must be `Clone` to hand out per-call handles. A `&'static` field keeps this `Copy`.
#[derive(Clone, Copy)]
pub struct OrchardFallback {
    /// The verifying key for this era, shared with the batch verifier it backs.
    vk: &'static ItemVerifyingKey,
}

impl Service<Item> for OrchardFallback {
    type Response = ();
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<(), BoxError>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, item: Item) -> Self::Future {
        Verifier::verify_single_spawning(item, self.vk).boxed()
    }
}

/// The concrete type of a global Halo2 verification service.
///
/// Each Orchard circuit era gets its own instance — see [`VERIFIER_PRE_NU6_2`] and
/// [`VERIFIER_POST_NU6_2`] — so that batches, fallbacks, and verifying keys are fully separated
/// per era.
type VerifierService = Fallback<Batch<Verifier, Item>, OrchardFallback>;

/// Builds a global Halo2 verifier that validates every item against `vk`.
///
/// The returned service batches contemporaneous proof verifications and, if a batch fails, falls
/// back to verifying each item individually. The batch and its fallback share the single `vk`
/// passed here, so an item built by this verifier is always checked against exactly one era's key.
/// Callers select the correct era's key by which `VERIFYING_KEY_*` they pass (see the two statics
/// below); there is no runtime key resolution.
fn batch_verifier(vk: &'static ItemVerifyingKey) -> VerifierService {
    Fallback::new(
        Batch::new(
            Verifier::new(vk),
            HALO2_MAX_BATCH_SIZE,
            None,
            super::MAX_BATCH_LATENCY,
        ),
        OrchardFallback { vk },
    )
}

/// Global batch verification context for **pre-NU6.2** Halo2 Action proofs.
///
/// Items routed here are verified against [`VERIFYING_KEY_PRE_NU6_2`] (the insecure circuit
/// retained for historical blocks). This service transparently batches contemporaneous proof
/// verifications, handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so you should call
/// `.clone()` on the global handle to create a local, mutable handle.
pub static VERIFIER_PRE_NU6_2: Lazy<VerifierService> =
    Lazy::new(|| batch_verifier(&VERIFYING_KEY_PRE_NU6_2));

/// Global batch verification context for **NU6.2+** Halo2 Action proofs.
///
/// Items routed here are verified against [`VERIFYING_KEY_POST_NU6_2`] (the fixed circuit). This
/// service transparently batches contemporaneous proof verifications, handling batch failures by
/// falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so you should call
/// `.clone()` on the global handle to create a local, mutable handle.
pub static VERIFIER_POST_NU6_2: Lazy<VerifierService> =
    Lazy::new(|| batch_verifier(&VERIFYING_KEY_POST_NU6_2));

/// Returns the global Halo2 verifier for Orchard bundles in blocks at `network_upgrade`.
///
/// The Orchard Action circuit — and therefore its verifying key — changed at NU6.2 (the fixed
/// variable-base scalar-multiplication circuit; see GHSA-jfw5-j458-pfv6), and a proof produced
/// under one circuit does not verify under the other key. So each bundle must be checked against
/// the key for the upgrade of the block it appears in:
///
///   * upgrades before NU6.2 are routed to [`VERIFIER_PRE_NU6_2`] (the historical insecure key),
///     so pre-soft-fork Orchard history still verifies on re-sync;
///   * NU6.2 and every later upgrade are routed to [`VERIFIER_POST_NU6_2`] (the fixed key).
///
/// The mapping is an explicit, exhaustive `match` on every [`NetworkUpgrade`] variant: there is
/// no version-comparison fallthrough and no default-to-insecure arm, so adding a future upgrade
/// is a compile error here until it is bound to a key on purpose.
pub fn verifier_for(network_upgrade: NetworkUpgrade) -> &'static VerifierService {
    use NetworkUpgrade::*;

    match network_upgrade {
        // Orchard did not exist before NU5, so these upgrades never carry Orchard bundles. They
        // are bound to the pre-NU6.2 (insecure) verifier because that is the only key under which
        // any Orchard history before NU6.2 verifies; routing them anywhere else cannot be correct.
        Genesis | BeforeOverwinter | Overwinter | Sapling | Blossom | Heartwood | Canopy | Nu5
        | Nu6 | Nu6_1 => &VERIFIER_PRE_NU6_2,

        // NU6.2 ships the fixed circuit, and every upgrade after it inherits that fixed circuit,
        // so all of them verify under the fixed key.
        Nu6_2 | Nu7 => &VERIFIER_POST_NU6_2,

        // `ZFuture` only exists under the `zcash_unstable = "zfuture"` cfg. It is a post-NU6.2
        // upgrade, so it inherits the fixed circuit and is bound to the fixed key here on purpose
        // (rather than via a wildcard) to keep this match exhaustive and fail-closed under every
        // build configuration.
        #[cfg(zcash_unstable = "zfuture")]
        ZFuture => &VERIFIER_POST_NU6_2,
    }
}

/// Halo2 proof verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Halo2 verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
///
/// Each verifier validates against a single, fixed [`ItemVerifyingKey`]; the two Orchard circuit
/// eras are served by two independent verifiers, so a batch never mixes pre- and post-NU6.2
/// proofs.
pub struct Verifier {
    /// The verifying key that every batch and fallback from this verifier uses.
    vk: &'static ItemVerifyingKey,

    /// The synchronous Halo2 batch validator.
    batch: BatchValidator,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Verifier {
    /// Creates a verifier that validates every item against `vk`.
    fn new(vk: &'static ItemVerifyingKey) -> Self {
        let (tx, _) = watch::channel(None);
        Self {
            vk,
            batch: BatchValidator::default(),
            tx,
        }
    }

    /// Returns the batch verifier and channel sender,
    /// replacing the batch and channel with new empty ones.
    fn take(&mut self) -> (BatchValidator, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut self.batch);
        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, tx)
    }

    /// Synchronously process the batch against `vk`, and send the result using
    /// the channel sender. This function blocks until the batch is completed.
    fn verify(batch: BatchValidator, vk: &'static ItemVerifyingKey, tx: Sender) {
        let result = batch.validate(vk, thread_rng());
        let _ = tx.send(Some(result));
    }

    /// Flush the batch using a thread pool, sending the result via the channel.
    /// This returns immediately, usually before the batch is completed.
    fn flush_blocking(&mut self) {
        let vk = self.vk;
        let (batch, tx) = self.take();

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // We don't care about execution order here, because this method is only called on drop.
        tokio::task::block_in_place(|| rayon::spawn_fifo(move || Self::verify(batch, vk, tx)));
    }

    /// Flush the batch using a thread pool, validating against `vk` and
    /// returning the result via the channel. This function returns a future that
    /// becomes ready when the batch is completed.
    async fn flush_spawning(batch: BatchValidator, vk: &'static ItemVerifyingKey, tx: Sender) {
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

    /// Verify a single item against `vk` using a thread pool, and return the result.
    async fn verify_single_spawning(
        item: Item,
        vk: &'static ItemVerifyingKey,
    ) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        if spawn_fifo(move || item.verify_single(vk)).await? {
            Ok(())
        } else {
            Err("could not validate orchard proof".into())
        }
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "Verifier";
        f.debug_struct(name).field("batch", &"..").finish()
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
                tracing::trace!("got item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();
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

                let vk = self.vk;
                let (batch, tx) = self.take();

                Box::pin(Self::flush_spawning(batch, vk, tx).map(|()| Ok(())))
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
