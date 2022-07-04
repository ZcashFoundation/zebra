//! Async Halo2 batch verifier service

use std::{
    convert::TryFrom,
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use futures::future::{ready, Ready};
use halo2::{
    pasta::{pallas, vesta},
    plonk,
};
use once_cell::sync::Lazy;
use orchard::circuit::{Circuit, VerifyingKey};
use rand::{thread_rng, CryptoRng, RngCore};
use thiserror::Error;
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

trait FromExt<T>: Sized {
    fn from(_: T) -> Self;
}

// Similar to `orchard::circuit::Instance::to_halo2_instance()`
// https://github.com/zcash/orchard/blob/1a77930f5ff314b5ae8165c555787eb0c725c76c/src/circuit.rs#L769
impl FromExt<orchard::circuit::Instance> for Vec<Vec<Vec<vesta::Scalar>>> {
    fn from(orchard_instance: orchard::circuit::Instance) -> Vec<Vec<Vec<vesta::Scalar>>> {
        // let halo2_instance: Vec<Vec<Vec<vesta::Scalar>>> = Default::default();

        const ANCHOR: usize = 0;
        const CV_NET_X: usize = 1;
        const CV_NET_Y: usize = 2;
        const NF_OLD: usize = 3;
        const RK_X: usize = 4;
        const RK_Y: usize = 5;
        const CMX: usize = 6;
        const ENABLE_SPEND: usize = 7;
        const ENABLE_OUTPUT: usize = 8;

        let mut instance = vec![vesta::Scalar::zero(); 9];

        instance[ANCHOR] = orchard_instance.anchor.inner();
        instance[CV_NET_X] = orchard_instance.cv_net.x();
        instance[CV_NET_Y] = orchard_instance.cv_net.y();
        instance[NF_OLD] = orchard_instance.nf_old.0;

        let rk = pallas::Point::from_bytes(&orchard_instance.rk.clone().into())
            .unwrap()
            .to_affine()
            .coordinates()
            .unwrap();

        instance[RK_X] = *rk.x();
        instance[RK_Y] = *rk.y();
        instance[CMX] = orchard_instance.cmx.inner();
        instance[ENABLE_SPEND] = vesta::Scalar::from(u64::from(orchard_instance.enable_spend));
        instance[ENABLE_OUTPUT] = vesta::Scalar::from(u64::from(orchard_instance.enable_output));

        vec![vec![instance]]
    }
}

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
    pub fn verify_single(&self, vk: &VerifyingKey) -> Result<(), halo2::plonk::Error> {
        self.proof.verify(vk, &self.instances[..])
    }
}

// #[derive(Default)]
pub struct BatchVerifier {
    batch: &'static mut plonk::BatchVerifier<vesta::Affine>,
}

impl BatchVerifier {
    pub fn queue(&mut self, item: Item) {
        let instances = item
            .instances
            .iter()
            .map(|i| {
                i.to_halo2_instance()
                    .into_iter()
                    .map(|c| c.into_iter().collect())
                    .collect()
            })
            .collect();

        self.batch.add_proof(instances, item.proof.clone());
    }

    pub fn verify<R: RngCore + CryptoRng>(
        self,
        _rng: R,
        vk: &VerifyingKey,
    ) -> Result<(), halo2::plonk::Error> {
        // for item in self.queue {
        //     item.verify_single(vk)?;
        // }

        // Build the Orchard circuit params and verifiying key from scratch until this
        // becomes exposed in the public API.

        // The value 11 is the size of the Orchard circuit:
        // https://github.com/zcash/orchard/blob/3faab98e9e82618a0f2d887054e9e28b0f7947dd/src/circuit.rs#L66
        let params = halo2::poly::commitment::Params::new(11);
        let circuit: Circuit = Default::default();

        let vk = plonk::keygen_vk(&params, &circuit).unwrap();

        if self.batch.finalize(&params, &vk) {
            return Ok(());
        } else {
            // TODO: `orchard` directly exposes halo2/plonk Error types, we should create
            // our own
            return Err(halo2::plonk::Error::Opening);
        }
    }
}

// === END TEMPORARY BATCH HALO2 SUBSTITUTE ===

impl From<&zebra_chain::orchard::ShieldedData> for Item {
    fn from(shielded_data: &zebra_chain::orchard::ShieldedData) -> Item {
        use orchard::{circuit, note, primitives::redpallas, tree, value};

        let anchor = tree::Anchor::from_bytes(shielded_data.shared_anchor.into()).unwrap();

        let enable_spend = shielded_data
            .flags
            .contains(zebra_chain::orchard::Flags::ENABLE_SPENDS);
        let enable_output = shielded_data
            .flags
            .contains(zebra_chain::orchard::Flags::ENABLE_OUTPUTS);

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
                    enable_spend,
                    enable_output,
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
#[allow(dead_code)]
pub static VERIFIER: Lazy<
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> Ready<Result<(), Halo2Error>>>>,
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
        tower::service_fn(
            (|item: Item| ready(item.verify_single(&VERIFYING_KEY).map_err(Halo2Error::from)))
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
    /// The sync Halo2 batch verifier.
    batch: BatchVerifier,
    // Making this 'static makes managing lifetimes much easier.
    vk: &'static VerifyingKey,
    /// Broadcast sender used to send the result of a batch verification to each
    /// request source in the batch.
    tx: Sender<Result<(), Halo2Error>>,
}

impl Verifier {
    #[allow(dead_code)]
    fn new(vk: &'static VerifyingKey) -> Self {
        let batch = BatchVerifier::default();
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
    type Error = Halo2Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Halo2Error>> + Send + 'static>>;

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
                            // This is the enum variant that
                            // orchard::circuit::Proof.verify() returns on
                            // evaluation failure.
                            Err(Halo2Error::ConstraintSystemFailure)
                        }
                        Err(RecvError::Closed) => panic!("verifier was dropped without flushing"),
                    }
                })
            }

            BatchControl::Flush => {
                tracing::trace!("got flush command");
                let batch = mem::take(&mut self.batch);
                let _ = self.tx.send(
                    batch
                        .verify(thread_rng(), self.vk)
                        .map_err(Halo2Error::from),
                );
                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending
        // futures.
        let batch = mem::take(&mut self.batch);
        let _ = self.tx.send(
            batch
                .verify(thread_rng(), self.vk)
                .map_err(Halo2Error::from),
        );
    }
}
