//! Async Halo2 batch verifier service

use std::{
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use nonempty::NonEmpty;
use once_cell::sync::Lazy;
use orchard::{
    bundle::BatchValidator,
    circuit::VerifyingKey,
    note::{ExtractedNoteCommitment, TransmittedNoteCiphertext},
};
use rand::thread_rng;
use zebra_chain::{amount::Amount, transaction::SigHash};

use crate::BoxError;
use thiserror::Error;
use tokio::sync::watch;
use tower::{util::ServiceFn, Service};
use tower_batch_control::{Batch, BatchControl, ItemSize};
use tower_fallback::Fallback;

use super::spawn_fifo;

// #[cfg(test)]
// mod tests;

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
type VerifyResult = bool;

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
    /// The halo2 proof verifying key.
    pub static ref VERIFYING_KEY: ItemVerifyingKey = ItemVerifyingKey::build();
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
    bundle: orchard::bundle::Bundle<orchard::bundle::Authorized, Amount>,
    sighash: SigHash,
}

impl ItemSize for Item {
    fn item_size(&self) -> usize {
        self.bundle.actions().len()
    }
}

impl Item {
    /// Perform non-batched verification of this `Item`.
    ///
    /// This is useful (in combination with `Item::clone`) for implementing
    /// fallback logic when batch verification fails.
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

impl From<(&zebra_chain::orchard::ShieldedData, SigHash)> for Item {
    fn from((shielded_data, sighash): (&zebra_chain::orchard::ShieldedData, SigHash)) -> Item {
        let anchor = orchard::tree::Anchor::from_bytes(shielded_data.shared_anchor.into()).unwrap();
        let authorization = orchard::bundle::Authorized::from_parts(
            orchard::Proof::new(shielded_data.proof.0.clone()),
            <[u8; 64]>::from(shielded_data.binding_sig).into(),
        );

        let bundle = orchard::bundle::Bundle::from_parts(
            NonEmpty::from_vec(
                shielded_data
                    .actions
                    .iter()
                    .map(
                        |zebra_chain::orchard::AuthorizedAction {
                             action:
                                 zebra_chain::orchard::Action {
                                     cv,
                                     nullifier,
                                     rk,
                                     cm_x,
                                     ephemeral_key,
                                     enc_ciphertext,
                                     out_ciphertext,
                                 },
                             spend_auth_sig,
                         }: &zebra_chain::orchard::AuthorizedAction| {
                            let rk = <[u8; 32]>::from(*rk).try_into().expect("should be valid");
                            let cmx = ExtractedNoteCommitment::from_bytes(&<[u8; 32]>::from(*cm_x))
                                .expect("should be valid");
                            let encrypted_note = TransmittedNoteCiphertext {
                                epk_bytes: ephemeral_key.into(),
                                enc_ciphertext: enc_ciphertext.into(),
                                out_ciphertext: out_ciphertext.into(),
                            };
                            let cv_net =
                                orchard::value::ValueCommitment::from_bytes(&<[u8; 32]>::from(*cv))
                                    .expect("should be valid");

                            let spend_auth_sig = <[u8; 64]>::from(*spend_auth_sig).into();

                            orchard::Action::from_parts(
                                nullifier.into(),
                                rk,
                                cmx,
                                encrypted_note,
                                cv_net,
                                spend_auth_sig,
                            )
                        },
                    )
                    .collect::<Vec<_>>(),
            )
            .expect("should be valid tx format"),
            orchard::bundle::Flags::from_byte(shielded_data.flags.bits()).expect("should be valid"),
            shielded_data.value_balance,
            anchor,
            authorization,
        );

        Self { bundle, sighash }
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
            Verifier::new(&VERIFYING_KEY),
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
            (|item: Item| Verifier::verify_single_spawning(item, &VERIFYING_KEY).boxed())
                as fn(_) -> _,
        ),
    )
});

/// Halo2 proof verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Halo2 verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    /// The synchronous Halo2 batch validator.
    batch: BatchValidator,

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
        let batch = BatchValidator::default();
        let (tx, _) = watch::channel(None);
        Self { batch, vk, tx }
    }

    /// Returns the batch verifier and channel sender from `self`,
    /// replacing them with a new empty batch.
    fn take(&mut self) -> (BatchValidator, &'static BatchVerifyingKey, Sender) {
        // Use a new verifier and channel for each batch.
        let batch = mem::take(&mut self.batch);

        let (tx, _) = watch::channel(None);
        let tx = mem::replace(&mut self.tx, tx);

        (batch, self.vk, tx)
    }

    /// Synchronously process the batch, and send the result using the channel sender.
    /// This function blocks until the batch is completed.
    fn verify(batch: BatchValidator, vk: &'static BatchVerifyingKey, tx: Sender) {
        let result = batch.validate(vk, thread_rng());
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
    async fn flush_spawning(batch: BatchValidator, vk: &'static BatchVerifyingKey, tx: Sender) {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        let _ = tx.send(
            spawn_fifo(move || batch.validate(vk, thread_rng()))
                .await
                .ok(),
        );
    }

    /// Verify a single item using a thread pool, and return the result.
    async fn verify_single_spawning(
        item: Item,
        pvk: &'static ItemVerifyingKey,
    ) -> Result<(), BoxError> {
        // TODO: Restore code for verifying single proofs or return a result from batch.validate()
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        if spawn_fifo(move || item.verify_single(pvk)).await? {
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
