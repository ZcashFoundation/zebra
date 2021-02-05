//! Async Groth16 batch verifier service

use std::{
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use bellman::{
    groth16::{PreparedVerifyingKey, Proof},
    VerificationError,
};
use bls12_381::Bls12;
use pairing::MultiMillerLoop;
use rand::{thread_rng, CryptoRng, RngCore};
use tokio::sync::broadcast::{channel, error::RecvError, Sender};
use tower::Service;
use tower_batch::BatchControl;
use tower_fallback::Fallback;

use crate::BoxError;

use self::hash_reader::HashReader;

mod hash_reader;
mod params;

pub use params::PARAMS;

// === TEMPORARY BATCH BELLMAN SUBSTITUTE ===
// These types are meant to be API compatible with the work in progress batch
// verification API being implemented in Bellman. Once we've finished that
// implementation and upgraded our dependency, we should be able to remove this
// section of code and replace each of these types with the commented out items
// from the rest of this file.

#[derive(Clone)]
pub struct Item<E: MultiMillerLoop> {
    proof: Proof<E>,
    public_inputs: Vec<E::Fr>,
}

impl<E: MultiMillerLoop> Item<E> {
    fn verify_single(self, pvk: &PreparedVerifyingKey<E>) -> Result<(), VerificationError> {
        let Item {
            proof,
            public_inputs,
        } = self;

        bellman::groth16::verify_proof(pvk, &proof, &public_inputs)
    }
}

impl<E: MultiMillerLoop> From<(&Proof<E>, &[E::Fr])> for Item<E> {
    fn from((proof, public_inputs): (&Proof<E>, &[E::Fr])) -> Self {
        (proof.clone(), public_inputs.to_owned()).into()
    }
}

impl<E: MultiMillerLoop> From<(Proof<E>, Vec<E::Fr>)> for Item<E> {
    fn from((proof, public_inputs): (Proof<E>, Vec<E::Fr>)) -> Self {
        Self {
            proof,
            public_inputs,
        }
    }
}

#[derive(Default)]
struct Batch {
    queue: Vec<Item<Bls12>>,
}

impl Batch {
    fn queue(&mut self, item: Item<Bls12>) {
        self.queue.push(item);
    }

    fn verify<R: RngCore + CryptoRng>(
        self,
        _rng: R,
        pvk: &PreparedVerifyingKey<Bls12>,
    ) -> Result<(), VerificationError> {
        for item in self.queue {
            item.verify_single(pvk)?;
        }

        Ok(())
    }
}

// === TEMPORARY BATCH BELLMAN SUBSTITUTE END ===

// /// A Groth16 verification item, used as the request type of the service.
// pub type Item = batch::Item<Bls12>;

/// Groth16 signature verifier service
#[derive(Clone, Debug)]
pub struct Verifier {
    inner: Fallback<tower_batch::Batch<VerifierImpl, Item<Bls12>>, FallbackVerifierImpl>,
}

impl Verifier {
    /// Constructs a new verifier.
    pub fn new(pvk: &'static PreparedVerifyingKey<Bls12>) -> Self {
        let verifier_impl = VerifierImpl::new(pvk);
        let fallback_impl = FallbackVerifierImpl::new(pvk);

        let max_items = super::MAX_BATCH_SIZE;
        let max_latency = super::MAX_BATCH_LATENCY;

        let inner = tower_batch::Batch::new(verifier_impl, max_items, max_latency);
        let inner = Fallback::new(inner, fallback_impl);

        Self { inner }
    }
}

impl Service<Item<Bls12>> for Verifier {
    type Response = ();
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<(), BoxError>> + Send + 'static>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Item<Bls12>) -> Self::Future {
        use futures::FutureExt;
        self.inner.call(req).boxed()
    }
}

/// Groth16 signature verifier implementation
///
/// This is the core implementation for the batch verification logic of the groth
/// verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
struct VerifierImpl {
    // batch: batch::Verifier<Bls12>,
    batch: Batch,
    // Making this 'static makes managing lifetimes much easier.
    pvk: &'static PreparedVerifyingKey<Bls12>,
    /// Broadcast sender used to send the result of a batch verification to each
    /// request source in the batch.
    tx: Sender<Result<(), VerificationError>>,
}

impl VerifierImpl {
    fn new(pvk: &'static PreparedVerifyingKey<Bls12>) -> Self {
        // let batch = batch::Verifier::default();
        let batch = Batch::default();
        let (tx, _) = channel(super::BROADCAST_BUFFER_SIZE);
        Self { batch, tx, pvk }
    }
}

impl fmt::Debug for VerifierImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "VerifierImpl";
        f.debug_struct(name)
            .field("batch", &"..")
            .field("pvk", &"..")
            .field("tx", &self.tx)
            .finish()
    }
}

impl Service<BatchControl<Item<Bls12>>> for VerifierImpl {
    type Response = ();
    type Error = VerificationError;
    type Future = Pin<Box<dyn Future<Output = Result<(), VerificationError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: BatchControl<Item<Bls12>>) -> Self::Future {
        match req {
            BatchControl::Item(item) => {
                tracing::trace!("got item");
                self.batch.queue(item);
                let mut rx = self.tx.subscribe();
                Box::pin(async move {
                    match rx.recv().await {
                        Ok(result) => result,
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
                let _ = self.tx.send(batch.verify(thread_rng(), self.pvk));
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for VerifierImpl {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(batch.verify(thread_rng(), self.pvk));
    }
}

/// Groth16 signature verifier fallback implementation
#[derive(Clone)]
struct FallbackVerifierImpl {
    pvk: &'static PreparedVerifyingKey<Bls12>,
}

impl FallbackVerifierImpl {
    fn new(pvk: &'static PreparedVerifyingKey<Bls12>) -> Self {
        Self { pvk }
    }
}

impl fmt::Debug for FallbackVerifierImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "FallbackVerifierImpl";
        f.debug_struct(name).field("pvk", &"..").finish()
    }
}

impl Service<Item<Bls12>> for FallbackVerifierImpl {
    type Response = ();
    type Error = VerificationError;
    type Future = Pin<Box<dyn Future<Output = Result<(), VerificationError>> + Send + 'static>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, item: Item<Bls12>) -> Self::Future {
        tracing::trace!("got item");
        let pvk = self.pvk;
        Box::pin(async move { item.verify_single(pvk) })
    }
}
