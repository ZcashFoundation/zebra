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
use orchard::circuit::VerifyingKey;
use rand::{thread_rng, CryptoRng, RngCore};

use thiserror::Error;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl};
use tower_fallback::Fallback;

use zebra_chain::orchard::{OrchardVanilla, OrchardZSA, ShieldedData, ShieldedDataFlavor};

use crate::BoxError;

use super::{spawn_fifo, spawn_fifo_and_convert};

#[cfg(test)]
mod tests;

/// Adjusted batch size for halo2 batches.
///
/// Unlike other batch verifiers, halo2 has aggregate proofs.
/// This means that there can be hundreds of actions verified by some proofs,
/// but just one action in others.
///
/// To compensate for larger proofs, we decrease the batch size.
///
/// We also decrease the batch size for these reasons:
/// - the default number of actions in `zcashd` is 2,
/// - halo2 proofs take longer to verify than Sapling proofs, and
/// - transactions with many actions generate very large proofs.
///
/// # TODO
///
/// Count each halo2 action as a batch item.
/// We could increase the batch item count by the action count each time a batch request
/// is received, which would reduce batch size, but keep the batch queue size larger.
const HALO2_MAX_BATCH_SIZE: usize = 2;

/* TODO: implement batch verification

/// The type of the batch verifier.
type BatchVerifier = plonk::BatchVerifier<vesta::Affine>;
 */

/// The type of verification results.
type VerifyResult = Result<(), Halo2Error>;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/* TODO: implement batch verification

/// The type of a raw verifying key.
/// This is the key used to verify batches.
pub type BatchVerifyingKey = VerifyingKey<vesta::Affine>;
 */
/// Temporary substitute type for fake batch verification.
///
/// TODO: implement batch verification
pub type BatchVerifyingKey = ItemVerifyingKey;

/// The type of a prepared verifying key.
/// This is the key used to verify individual items.
pub type ItemVerifyingKey = VerifyingKey;

lazy_static::lazy_static! {
    /// The halo2 proof verifying key for Orchard Vanilla
    pub static ref VERIFYING_KEY_VANILLA: ItemVerifyingKey = ItemVerifyingKey::build::<OrchardVanilla>();

    /// The halo2 proof verifying key for OrchardZSA
    pub static ref VERIFYING_KEY_ZSA: ItemVerifyingKey = ItemVerifyingKey::build::<OrchardZSA>();
}

// === TEMPORARY BATCH HALO2 SUBSTITUTE ===
//
// These types are meant to be API compatible with the batch verification APIs
// in bellman::groth16::batch, reddsa::batch, redjubjub::batch, and
// ed25519-zebra::batch. Once Halo2 batch proof verification math and
// implementation is available, this code can be replaced with that.

/// A Halo2 verification item, used as the request type of the service.
#[derive(Clone, Debug)]
pub struct Item {
    instances: Vec<orchard::circuit::Instance>,
    proof: orchard::circuit::Proof,
}

impl Item {
    /// Perform non-batched verification of this `Item`.
    ///
    /// This is useful (in combination with `Item::clone`) for implementing
    /// fallback logic when batch verification fails.
    pub fn verify_single(&self, vk: &ItemVerifyingKey) -> Result<(), halo2::plonk::Error> {
        self.proof.verify(vk, &self.instances[..])
    }
}

/// A fake batch verifier that queues and verifies halo2 proofs.
#[derive(Default)]
pub struct BatchVerifier {
    queue: Vec<Item>,
}

impl BatchVerifier {
    /// Queues an item for fake batch verification.
    pub fn queue(&mut self, item: Item) {
        self.queue.push(item);
    }

    /// Verifies the current fake batch.
    pub fn verify<R: RngCore + CryptoRng>(
        self,
        _rng: R,
        vk: &ItemVerifyingKey,
    ) -> Result<(), halo2::plonk::Error> {
        for item in self.queue {
            item.verify_single(vk)?;
        }

        Ok(())
    }
}

// === END TEMPORARY BATCH HALO2 SUBSTITUTE ===

impl<V: OrchardVerifier> From<&ShieldedData<V>> for Item {
    fn from(shielded_data: &ShieldedData<V>) -> Item {
        use orchard::{circuit, note, primitives::redpallas, tree, value};

        let anchor = tree::Anchor::from_bytes(shielded_data.shared_anchor.into()).unwrap();

        let flags = orchard::bundle::Flags::from_byte(shielded_data.flags.bits())
            .expect("failed to convert flags: shielded_data.flags contains unexpected bits that are not valid in orchard::bundle::Flags");

        let instances = shielded_data
            .actions()
            .map(|action| {
                circuit::Instance::from_parts(
                    anchor,
                    value::ValueCommitment::from_bytes(&action.cv.into()).unwrap(),
                    note::Nullifier::from_bytes(&action.nullifier.into()).unwrap(),
                    redpallas::VerificationKey::<redpallas::SpendAuth>::try_from(<[u8; 32]>::from(
                        action.rk,
                    ))
                    .expect("should be a valid redpallas spendauth verification key"),
                    note::ExtractedNoteCommitment::from_bytes(&action.cm_x.into()).unwrap(),
                    flags,
                )
            })
            .collect();

        Item {
            instances,
            proof: orchard::circuit::Proof::new(shielded_data.proof.0.clone()),
        }
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

type VerificationContext = Fallback<
    Batch<Verifier, Item>,
    ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), BoxError>>>,
>;

pub(crate) trait OrchardVerifier: ShieldedDataFlavor {
    const ZSA_ENABLED: bool;

    fn get_verifying_key() -> &'static ItemVerifyingKey;
    fn get_verifier() -> &'static VerificationContext;
}

fn create_verification_context<V: OrchardVerifier>() -> VerificationContext {
    Fallback::new(
        Batch::new(
            Verifier::new(V::get_verifying_key()),
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
        tower::service_fn(
            (|item: Item| Verifier::verify_single_spawning(item, V::get_verifying_key()).boxed())
                as fn(_) -> _,
        ),
    )
}

/// Global batch verification context for Halo2 proofs of Action statements.
///
/// This service transparently batches contemporaneous proof verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static VERIFIER_VANILLA: Lazy<VerificationContext> =
    Lazy::new(create_verification_context::<OrchardVanilla>);

/// FIXME: copy a doc from VERIFIER_VANILLA or just refer to its doc?
pub static VERIFIER_ZSA: Lazy<VerificationContext> =
    Lazy::new(create_verification_context::<OrchardZSA>);

impl OrchardVerifier for OrchardVanilla {
    const ZSA_ENABLED: bool = false;

    fn get_verifying_key() -> &'static ItemVerifyingKey {
        &VERIFYING_KEY_VANILLA
    }

    fn get_verifier() -> &'static VerificationContext {
        &VERIFIER_VANILLA
    }
}

impl OrchardVerifier for OrchardZSA {
    const ZSA_ENABLED: bool = true;

    fn get_verifying_key() -> &'static ItemVerifyingKey {
        &VERIFYING_KEY_ZSA
    }

    fn get_verifier() -> &'static VerificationContext {
        &VERIFIER_ZSA
    }
}

/// Halo2 proof verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Halo2 verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    /// The synchronous Halo2 batch verifier.
    batch: BatchVerifier,

    /// The halo2 proof verification key.
    ///
    /// Making this 'static makes managing lifetimes much easier.
    vk: &'static ItemVerifyingKey,

    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Verifier {
    fn new(vk: &'static ItemVerifyingKey) -> Self {
        let batch = BatchVerifier::default();
        let (tx, _) = watch::channel(None);
        Self { batch, vk, tx }
    }

    /// Returns the batch verifier and channel sender from `self`,
    /// replacing them with a new empty batch.
    fn take(&mut self) -> (BatchVerifier, &'static BatchVerifyingKey, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut self.batch);

        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, self.vk, tx)
    }

    /// Synchronously process the batch, and send the result using the channel sender.
    /// This function blocks until the batch is completed.
    fn verify(batch: BatchVerifier, vk: &'static BatchVerifyingKey, tx: Sender) {
        let result = batch.verify(thread_rng(), vk).map_err(Halo2Error::from);
        let _ = tx.send(Some(result));
    }

    /// Flush the batch using a thread pool, and return the result via the channel.
    /// This returns immediately, usually before the batch is completed.
    fn flush_blocking(&mut self) {
        let (batch, vk, tx) = self.take();

        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // We don't care about execution order here, because this method is only called on drop.
        tokio::task::block_in_place(|| rayon::spawn_fifo(|| Self::verify(batch, vk, tx)));
    }

    /// Flush the batch using a thread pool, and return the result via the channel.
    /// This function returns a future that becomes ready when the batch is completed.
    async fn flush_spawning(batch: BatchVerifier, vk: &'static BatchVerifyingKey, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let _ = tx.send(
            spawn_fifo(move || batch.verify(thread_rng(), vk).map_err(Halo2Error::from))
                .await
                .ok(),
        );
    }

    /// Verify a single item using a thread pool, and return the result.
    async fn verify_single_spawning(
        item: Item,
        pvk: &'static ItemVerifyingKey,
    ) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        spawn_fifo_and_convert(move || item.verify_single(pvk).map_err(Halo2Error::from)).await
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "Verifier";
        f.debug_struct(name)
            .field("batch", &"..")
            .field("vk", &"..")
            .field("tx", &self.tx)
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
                tracing::trace!("got item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();
                Box::pin(async move {
                    match rx.changed().await {
                        Ok(()) => {
                            // We use a new channel for each batch,
                            // so we always get the correct batch result here.
                            let result = rx
                                .borrow()
                                .as_ref()
                                .ok_or("threadpool unexpectedly dropped response channel sender. Is Zebra shutting down?")?
                                .clone();

                            if result.is_ok() {
                                tracing::trace!(?result, "verified halo2 proof");
                                metrics::counter!("proofs.halo2.verified").increment(1);
                            } else {
                                tracing::trace!(?result, "invalid halo2 proof");
                                metrics::counter!("proofs.halo2.invalid").increment(1);
                            }

                            result.map_err(BoxError::from)
                        }
                        Err(_recv_error) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got halo2 flush command");

                let (batch, vk, tx) = self.take();

                Box::pin(Self::flush_spawning(batch, vk, tx).map(Ok))
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
