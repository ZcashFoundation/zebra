//! Async Halo2 batch verifier service

use std::{
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{ready, Ready};
use once_cell::sync::Lazy;
use orchard::circuit::VerifyingKey;
use rand::thread_rng;
use tokio::sync::broadcast::{channel, error::RecvError, Sender};
use tower::{util::ServiceFn, Service};
use tower_batch::{Batch, BatchControl};
use tower_fallback::Fallback;

#[cfg(test)]
mod tests;

lazy_static::lazy_static! {
    pub static ref VERIFYING_KEY: VerifyingKey = VerifyingKey::build();
}

// === TEMPORARY BATCH HALO2 SUBSTITUTE ===
//
// These types are meant to be API compatible with the batch verification APIs
// in bellman::groth16::batch, reddsa::batch, redjubjub::batch, and
// ed25519-zebra::batch. Once Halo2 batch proof verification math and
// implementation is available, this code can be replaced with that.

mod batch {

    /// A Halo2 verification item, used as the request type of the service.
    #[derive(Clone, Debug)]
    pub struct Item {
        actions: Vec<zebra_chain::orchard::Action>,
        anchor: zebra_chain::orchard::tree::Root,
        flags: zebra_chain::orchard::Flags,
        proof: zebra_chain::primitives::Halo2Proof,
    }

    impl Item {
        /// Produce `Instance`s of the public inputs to the Orchard Action Halo2
        /// proof circuit.
        pub fn instances(&self) -> impl Iterator<Item = &orchard::circuit::Instance> {
            self.actions
                .iter()
                .map(|action| orchard::circuit::Instance {
                    anchor: self.anchor.into(),
                    cv_net: action.cv.into(),
                    nf_old: action.nullifier.into(),
                    rk: action.rk.into(),
                    cmx: action.cm_x.into(),
                    enable_spend: self
                        .flags
                        .contains(zebra_chain::orchard::Flags::ENABLE_SPENDS),
                    enable_output: self
                        .flags
                        .contains(zebra_chain::orchard::Flags::ENABLE_OUTPUTS),
                })
        }

        /// Perform non-batched verification of this `Item`.
        ///
        /// This is useful (in combination with `Item::clone`) for implementing
        /// fallback logic when batch verification fails.
        pub fn verify_single(&self, vk: &VerifyingKey) -> Result<(), VerificationError> {
            self.proof.verify(vk, &self.instances())
        }
    }

    #[derive(Default)]
    pub struct Verifier {
        queue: Vec<Item>,
    }

    impl Verifier {
        fn queue(&mut self, item: Item) {
            self.queue.push(item);
        }

        fn verify<R: RngCore + CryptoRng>(
            self,
            _rng: R,
            vk: &VerifyingKey,
        ) -> Result<(), VerificationError> {
            for item in self.queue {
                item.verify_single(vk)?;
            }

            Ok(())
        }
    }
}

// === END TEMPORARY BATCH HALO2 SUBSTITUTE ===

/// Type alias to clarify that this batch::Item is a Halo2 Item.
pub type Item = batch::Item;

impl From<zebra_chain::orchard::ShieldedData> for Item {
    fn from(shielded_data: zebra_chain::orchard::ShieldedData) -> Item {
        Self {
            actions: shielded_data.actions,
            anchor: shielded_data.anchor,
            flags: shielded_data.flags,
            proof: shielded_data.proof,
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
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> Ready<Result<(), VerificationError>>>>,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::new(&VERIFYING_KEY),
            super::MAX_BATCH_SIZE,
            super::MAX_BATCH_LATENCY,
        ),
        // We want to fallback to individual verification if batch verification
        // fails, so we need a Service to use. The obvious way to do this would
        // be to write a closure that returns an async block. But because we
        // have to specify the type of a static, we need to be able to write the
        // type of the closure and its return value, and both closures and async
        // blocks have eldritch types whose names cannot be written. So instead,
        // we use a Ready to avoid an async block and cast the closure to a
        // function (which is possible because it doesn't capture any state).
        tower::service_fn((|item: Item| ready(item.verify_single(&VERIFYING_KEY))) as fn(_) -> _),
    )
});

/// Halo2 signature verifier implementation
///
/// This is the core implementation for the batch verification logic of the
/// Halo2 verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    /// The sync Halo2 batch verifier.
    batch: batch::Verifier,
    // Making this 'static makes managing lifetimes much easier.
    vk: &'static VerifyingKey,
    /// Broadcast sender used to send the result of a batch verification to each
    /// request source in the batch.
    tx: Sender<Result<(), VerificationError>>,
}

impl Verifier {
    fn new(vk: &'static VerifyingKey) -> Self {
        let batch = batch::Verifier::default();
        let (tx, _) = channel(super::BROADCAST_BUFFER_SIZE);
        Self { batch, vk, tx }
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
    type Error = VerificationError;
    type Future = Pin<Box<dyn Future<Output = Result<(), VerificationError>> + Send + 'static>>;

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
                    match rx.recv().await {
                        Ok(result) => {
                            if result.is_ok() {
                                tracing::trace!(?result, "verified halo2 proof");
                                metrics::counter!("proofs.halo2.verified", 1);
                            } else {
                                tracing::trace!(?result, "invalid halo2 proof");
                                metrics::counter!("proofs.halo2.invalid", 1);
                            }

                            result
                        }
                        Err(RecvError::Lagged(_)) => {
                            tracing::error!(
                                "missed channel updates, BROADCAST_BUFFER_SIZE is too low!!"
                            );
                            Err(VerificationError::InvalidProof)
                        }
                        Err(RecvError::Closed) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got flush command");
                let batch = mem::take(&mut self.batch);
                let _ = self.tx.send(batch.verify(thread_rng(), self.vk));
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(batch.verify(thread_rng(), self.vk));
    }
}
