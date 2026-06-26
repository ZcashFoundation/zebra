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

// The Orchard Action circuit — and therefore its verifying key — has changed across upgrades, and
// a proof produced under one circuit version does not verify under another. We keep one key per
// circuit version, each in its own dedicated verifier, and route each bundle to the correct one by
// the block era (network upgrade) it was mined in. The circuit era is a function of the Orchard
// pool and the block's upgrade, NOT of the transaction version (v5 vs v6): an Orchard-pool bundle
// mined at NU6.3 commits to the NU6.3 circuit whether it is carried in a v5 or a v6 transaction
// (see [`orchard_v5_verifier_for`] / [`orchard_v6_verifier`]):
//
//   * Orchard bundles before NU6.2 (NU5..NU6.2) were produced by the historical, insecure circuit
//     and only verify under the [`InsecurePreNu6_2`] key. These must keep verifying so that nodes
//     can re-sync and reindex pre-soft-fork Orchard history.
//
//   * Orchard bundles from NU6.2 until NU6.3 use the fixed circuit and only verify under the
//     [`FixedPostNu6_2`] key.
//
//   * Every Orchard Action from NU6.3 onward uses the NU6.3 cross-address circuit and only verifies
//     under the [`PostNu6_3`] key. The NU6.3 circuit extends the fixed circuit with the
//     `disableCrossAddress` constraint that enforces the Orchard-pool cross-address restriction. Per
//     ZIP 229 that restriction applies to every Orchard-pool Action "regardless of transaction
//     version ... so that it cannot be bypassed by using a version 5 transaction", so v5 Orchard
//     bundles at NU6.3, v6 Orchard bundles, and Ironwood bundles all share this one key.
//
// Routing therefore depends on the block era (network upgrade), not the transaction version.
//
// NOTE: this deliberately does NOT copy zcashd PR #176's WIP shortcut of validating everything
// against the fixed key; that is incorrect both for re-syncing pre-soft-fork Orchard blocks (whose
// proofs only verify under the insecure key) and for NU6.3 Orchard Actions (whose cross-address
// restriction the fixed key cannot enforce).
lazy_static::lazy_static! {
    /// The Orchard Action verifying key for the **pre-NU6.2** (insecure) circuit.
    ///
    /// Reconstructs the verifying key of the original (NU5..NU6.2) Orchard Action circuit.
    /// Bundles mined before NU6.2 committed to this circuit and only verify under this key, so it
    /// MUST be retained to re-verify pre-NU6.2 history on resync. It must never be used to verify
    /// post-NU6.2 bundles.
    pub static ref VERIFYING_KEY_V5_PRE_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build(OrchardCircuitVersion::InsecurePreNu6_2);

    /// The Orchard Action verifying key for the **NU6.2-until-NU6.3** (fixed) circuit.
    ///
    /// Built from the fixed variable-base scalar-multiplication Orchard Action circuit shipped in
    /// NU6.2. Orchard bundles mined from the NU6.2 activation height until NU6.3 commit to this
    /// circuit and only verify under this key. At NU6.3 the Orchard pool moves to
    /// [`VERIFYING_KEY_V6`]. See [`VERIFYING_KEY_V5_PRE_NU6_2`] for the era split.
    pub static ref VERIFYING_KEY_V5_POST_NU6_2: ItemVerifyingKey =
        ItemVerifyingKey::build(OrchardCircuitVersion::FixedPostNu6_2);

    /// The Orchard Action verifying key for the **NU6.3-onward** circuit.
    ///
    /// Built from the NU6.3 Action circuit, which extends the fixed circuit with the
    /// `disableCrossAddress` constraint that enforces the Orchard-pool cross-address restriction.
    /// Every Orchard Action mined from NU6.3 onward commits to this circuit and only verifies under
    /// this key: v5 Orchard bundles at NU6.3, v6 Orchard bundles, and Ironwood bundles. (Named
    /// `V6` for the transaction version that introduced the Ironwood pool, but the key is selected
    /// by block era, not transaction version.)
    pub static ref VERIFYING_KEY_V6: ItemVerifyingKey =
        ItemVerifyingKey::build(OrchardCircuitVersion::PostNu6_3);
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
        let mut batch = BatchValidator::new(vk);
        // `add_bundle` rejects a bundle whose cross-address restriction is not supported by this
        // era's verifying key; such an item is invalid under this key.
        if batch.queue(self).is_err() {
            return false;
        }
        batch.validate(thread_rng())
    }
}

trait QueueBatchVerify {
    fn queue(&mut self, item: Item) -> Result<(), orchard::bundle::BatchError>;
}

impl QueueBatchVerify for BatchValidator<'_> {
    fn queue(&mut self, Item { bundle, sighash }: Item) -> Result<(), orchard::bundle::BatchError> {
        self.add_bundle(&bundle, sighash.0)
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
/// Each Orchard circuit version gets its own instance — see [`VERIFIER_V5_PRE_NU6_2`],
/// [`VERIFIER_V5_POST_NU6_2`], and [`VERIFIER_V6`] — so that batches, fallbacks, and verifying
/// keys are fully separated per circuit version. The Orchard verifier routing functions
/// ([`orchard_v5_verifier_for`] / [`orchard_v6_verifier`]) return a borrow of the matching one.
pub type VerifierService = Fallback<Batch<Verifier, Item>, OrchardFallback>;

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
/// Items routed here are verified against [`VERIFYING_KEY_V5_PRE_NU6_2`] (the insecure circuit
/// retained for historical blocks). This service transparently batches contemporaneous proof
/// verifications, handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so you should call
/// `.clone()` on the global handle to create a local, mutable handle.
pub static VERIFIER_V5_PRE_NU6_2: Lazy<VerifierService> =
    Lazy::new(|| batch_verifier(&VERIFYING_KEY_V5_PRE_NU6_2));

/// Global batch verification context for **NU6.2-until-NU6.3** Halo2 Action proofs.
///
/// Items routed here are verified against [`VERIFYING_KEY_V5_POST_NU6_2`] (the fixed circuit). This
/// service transparently batches contemporaneous proof verifications, handling batch failures by
/// falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so you should call
/// `.clone()` on the global handle to create a local, mutable handle.
pub static VERIFIER_V5_POST_NU6_2: Lazy<VerifierService> =
    Lazy::new(|| batch_verifier(&VERIFYING_KEY_V5_POST_NU6_2));

/// Global batch verification context for **NU6.3-onward** Halo2 Action proofs.
///
/// Items routed here are verified against [`VERIFYING_KEY_V6`] (the NU6.3 Action circuit, which
/// adds the `disableCrossAddress` constraint). Every Orchard Action mined from NU6.3 onward routes
/// here — v5 Orchard bundles at NU6.3, v6 Orchard bundles, and Ironwood bundles. This service
/// transparently batches contemporaneous proof verifications, handling batch failures by falling
/// back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so you should call
/// `.clone()` on the global handle to create a local, mutable handle.
pub static VERIFIER_V6: Lazy<VerifierService> = Lazy::new(|| batch_verifier(&VERIFYING_KEY_V6));

/// Returns the global Halo2 verifier for the **Orchard-pool** bundle of a **v5** transaction in a
/// block at `network_upgrade`.
///
/// The Orchard Action circuit — and therefore its verifying key — changes with the block era, and
/// a proof produced under one circuit does not verify under another era's key. The era is a
/// function of the block's network upgrade, **not** the transaction version, so each v5 Orchard
/// bundle is checked against the key for the upgrade of the block it appears in:
///
///   * upgrades before NU6.2 → [`VERIFIER_V5_PRE_NU6_2`] (the historical insecure key), so
///     pre-soft-fork Orchard history still verifies on re-sync;
///   * NU6.2 until NU6.3 → [`VERIFIER_V5_POST_NU6_2`] (the fixed key);
///   * NU6.3 onward → [`VERIFIER_V6`] (the NU6.3 circuit). The Orchard-pool cross-address
///     restriction is enforced for every Orchard Action from NU6.3 onward regardless of transaction
///     version, "so that it cannot be bypassed by using a version 5 transaction" (ZIP 229); that
///     restriction lives in the NU6.3 circuit, which the NU6.2 fixed key cannot verify. So a v5
///     Orchard bundle at NU6.3 uses the same key as v6 Orchard and Ironwood bundles.
///
/// v6 Orchard and Ironwood bundles use [`orchard_v6_verifier`], which returns that same
/// NU6.3-onward key.
///
/// The mapping is an explicit, exhaustive `match` on every [`NetworkUpgrade`] variant: there is no
/// version-comparison fallthrough and no default arm, so adding a future upgrade is a compile error
/// here until it is bound to a key on purpose.
pub fn orchard_v5_verifier_for(network_upgrade: NetworkUpgrade) -> &'static VerifierService {
    use NetworkUpgrade::*;

    match network_upgrade {
        // Orchard did not exist before NU5, so these upgrades never carry Orchard bundles. They
        // are bound to the pre-NU6.2 (insecure) verifier because that is the only key under which
        // any Orchard history before NU6.2 verifies; routing them anywhere else cannot be correct.
        Genesis | BeforeOverwinter | Overwinter | Sapling | Blossom | Heartwood | Canopy | Nu5
        | Nu6 | Nu6_1 => &VERIFIER_V5_PRE_NU6_2,

        // NU6.2 ships the fixed circuit and is the only upgrade that uses it: it is active from the
        // NU6.2 activation height until NU6.3.
        Nu6_2 => &VERIFIER_V5_POST_NU6_2,

        // NU6.3 adds the `disableCrossAddress` constraint to the Orchard Action circuit. Every
        // Orchard Action from NU6.3 onward — including those in v5 transactions — commits to this
        // circuit, so NU6.3 and later route to the NU6.3-onward key. Per ZIP 229 the cross-address
        // restriction applies "regardless of transaction version ... so that it cannot be bypassed
        // by using a version 5 transaction". Verifying these under the NU6.2 fixed key would both
        // reject honest proofs (different key) and fail to enforce the restriction.
        Nu6_3 | Nu7 => &VERIFIER_V6,

        // `ZFuture` only exists under the `zcash_unstable = "zfuture"` cfg. It is a post-NU6.3
        // upgrade, so it inherits the NU6.3 circuit and is bound to the NU6.3-onward key here on
        // purpose (rather than via a wildcard) to keep this match exhaustive and fail-closed under
        // every build configuration.
        #[cfg(zcash_unstable = "zfuture")]
        ZFuture => &VERIFIER_V6,
    }
}

/// Returns the global Halo2 verifier for **v6** Orchard-pool and Ironwood-pool bundles.
///
/// v6 Orchard and Ironwood bundles only exist from NU6.3 onward, so they always use the NU6.3
/// circuit — the same [`VERIFIER_V6`] key that v5 Orchard bundles at NU6.3 route to.
pub fn orchard_v6_verifier() -> &'static VerifierService {
    &VERIFIER_V6
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
    ///
    /// Borrows `vk` (which is `'static`), so the validator is `BatchValidator<'static>`.
    batch: BatchValidator<'static>,

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
            batch: BatchValidator::new(vk),
            tx,
        }
    }

    /// Returns the batch verifier and channel sender,
    /// replacing the batch and channel with new empty ones.
    fn take(&mut self) -> (BatchValidator<'static>, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::replace(&mut self.batch, BatchValidator::new(self.vk));
        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, tx)
    }

    /// Synchronously process the batch (the verifying key is held by the batch), and send the
    /// result using the channel sender. This function blocks until the batch is completed.
    fn verify(batch: BatchValidator<'static>, tx: Sender) {
        let result = batch.validate(thread_rng());
        let _ = tx.send(Some(result));
    }

    /// Flush the batch using a thread pool, sending the result via the channel.
    /// This returns immediately, usually before the batch is completed.
    fn flush_blocking(&mut self) {
        let (batch, tx) = self.take();

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // We don't care about execution order here, because this method is only called on drop.
        tokio::task::block_in_place(|| rayon::spawn_fifo(move || Self::verify(batch, tx)));
    }

    /// Flush the batch using a thread pool, returning the result via the channel. This function
    /// returns a future that becomes ready when the batch is completed.
    async fn flush_spawning(batch: BatchValidator<'static>, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let start = std::time::Instant::now();
        let result = spawn_fifo(move || batch.validate(thread_rng())).await;
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
                match self.batch.queue(item) {
                    Ok(()) => {}
                    Err(orchard::bundle::BatchError::RestrictionUnsupportedByKey) => unreachable!(
                        "the error fires only for a bundle with cross_address_enabled = false on a \
                         key that does not support the restriction; VERIFIER_V6's key supports it, \
                         and the pre-NU6.3 verifiers only ever see v5-format bundles, which always \
                         report cross_address_enabled = true"
                    ),
                    // `BatchError` is `#[non_exhaustive]`; any future variant lands here. Reject the
                    // item on its own without poisoning the rest of the batch.
                    Err(other) => {
                        return Box::pin(async move {
                            metrics::counter!("proofs.halo2.invalid").increment(1);
                            Err(format!("could not validate halo2 proof: {other}").into())
                        });
                    }
                }
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

                let (batch, tx) = self.take();

                Box::pin(Self::flush_spawning(batch, tx).map(|()| Ok(())))
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
