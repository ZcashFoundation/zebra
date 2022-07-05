//! Async Groth16 batch verifier service

use std::{
    convert::{TryFrom, TryInto},
    fmt,
    future::Future,
    mem,
    pin::Pin,
    task::{Context, Poll},
};

use bellman::{
    gadgets::multipack,
    groth16::{batch, VerifyingKey},
    VerificationError,
};
use bls12_381::Bls12;
use futures::future::{ready, Ready};
use once_cell::sync::Lazy;
use rand::thread_rng;
use tokio::sync::broadcast::{channel, error::RecvError, Sender};
use tower::{util::ServiceFn, Service};

use tower_batch::{Batch, BatchControl};
use tower_fallback::{BoxedError, Fallback};

use zebra_chain::{
    primitives::{
        ed25519::{self, VerificationKeyBytes},
        Groth16Proof,
    },
    sapling::{Output, PerSpendAnchor, Spend},
    sprout::{JoinSplit, Nullifier, RandomSeed},
};

mod params;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vectors;

pub use params::{Groth16Parameters, GROTH16_PARAMETERS};

use crate::error::TransactionError;

/// Global batch verification context for Groth16 proofs of Spend statements.
///
/// This service transparently batches contemporaneous proof verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static SPEND_VERIFIER: Lazy<
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> Ready<Result<(), VerificationError>>>>,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::new(&GROTH16_PARAMETERS.sapling.spend.vk),
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
            (|item: Item| {
                ready(item.verify_single(&GROTH16_PARAMETERS.sapling.spend_prepared_verifying_key))
            }) as fn(_) -> _,
        ),
    )
});

/// Global batch verification context for Groth16 proofs of Output statements.
///
/// This service transparently batches contemporaneous proof verifications,
/// handling batch failures by falling back to individual verification.
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static OUTPUT_VERIFIER: Lazy<
    Fallback<Batch<Verifier, Item>, ServiceFn<fn(Item) -> Ready<Result<(), VerificationError>>>>,
> = Lazy::new(|| {
    Fallback::new(
        Batch::new(
            Verifier::new(&GROTH16_PARAMETERS.sapling.output.vk),
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
            (|item: Item| {
                ready(item.verify_single(&GROTH16_PARAMETERS.sapling.output_prepared_verifying_key))
            }) as fn(_) -> _,
        ),
    )
});

/// Global batch verification context for Groth16 proofs of JoinSplit statements.
///
/// This service does not yet batch verifications, see
/// <https://github.com/ZcashFoundation/zebra/issues/3127>
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static JOINSPLIT_VERIFIER: Lazy<ServiceFn<fn(Item) -> Ready<Result<(), BoxedError>>>> =
    Lazy::new(|| {
        // We need a Service to use. The obvious way to do this would
        // be to write a closure that returns an async block. But because we
        // have to specify the type of a static, we need to be able to write the
        // type of the closure and its return value, and both closures and async
        // blocks have eldritch types whose names cannot be written. So instead,
        // we use a Ready to avoid an async block and cast the closure to a
        // function (which is possible because it doesn't capture any state).
        tower::service_fn(
            (|item: Item| {
                ready(
                    // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
                    //
                    // TODO: use spawn_blocking to avoid blocking code running concurrently in this task
                    tokio::task::block_in_place(|| {
                        item.verify_single(
                            &GROTH16_PARAMETERS.sprout.joinsplit_prepared_verifying_key,
                        )
                        .map_err(|e| TransactionError::Groth16(e.to_string()))
                        .map_err(tower_fallback::BoxedError::from)
                    }),
                )
            }) as fn(_) -> _,
        )
    });

/// A Groth16 Description (JoinSplit, Spend, or Output) with a Groth16 proof
/// and its inputs encoded as scalars.
pub trait Description {
    /// The Groth16 proof of this description.
    fn proof(&self) -> &Groth16Proof;
    /// The primary inputs for this proof, encoded as [`jubjub::Fq`] scalars.
    fn primary_inputs(&self) -> Vec<jubjub::Fq>;
}

impl Description for Spend<PerSpendAnchor> {
    /// Encodes the primary input for the Sapling Spend proof statement as 7 Bls12_381 base
    /// field elements, to match [`bellman::groth16::verify_proof`] (the starting fixed element
    /// `1` is filled in by [`bellman`].
    ///
    /// NB: jubjub::Fq is a type alias for bls12_381::Scalar.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#cctsaplingspend>
    fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let mut inputs = vec![];

        let rk_affine = jubjub::AffinePoint::from_bytes(self.rk.clone().into()).unwrap();
        inputs.push(rk_affine.get_u());
        inputs.push(rk_affine.get_v());

        let cv_affine = jubjub::AffinePoint::from(self.cv);
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        // TODO: V4 only
        inputs.push(jubjub::Fq::from_bytes(&self.per_spend_anchor.into()).unwrap());

        let nullifier_limbs: [jubjub::Fq; 2] = self.nullifier.into();

        inputs.push(nullifier_limbs[0]);
        inputs.push(nullifier_limbs[1]);

        inputs
    }

    fn proof(&self) -> &Groth16Proof {
        &self.zkproof
    }
}

impl Description for Output {
    /// Encodes the primary input for the Sapling Output proof statement as 5 Bls12_381 base
    /// field elements, to match [`bellman::groth16::verify_proof`] (the starting fixed element
    /// `1` is filled in by [`bellman`].
    ///
    /// NB: [`jubjub::Fq`] is a type alias for [`bls12_381::Scalar`].
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#cctsaplingoutput>
    fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let mut inputs = vec![];

        let cv_affine = jubjub::AffinePoint::from(self.cv);
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        let epk_affine = jubjub::AffinePoint::from_bytes(self.ephemeral_key.into()).unwrap();
        inputs.push(epk_affine.get_u());
        inputs.push(epk_affine.get_v());

        inputs.push(self.cm_u);

        inputs
    }

    fn proof(&self) -> &Groth16Proof {
        &self.zkproof
    }
}

/// Compute the [h_{Sig} hash function][1] which is used in JoinSplit descriptions.
///
/// `random_seed`: the random seed from the JoinSplit description.
/// `nf1`: the first nullifier from the JoinSplit description.
/// `nf2`: the second nullifier from the JoinSplit description.
/// `joinsplit_pub_key`: the JoinSplit public validation key from the transaction.
///
/// [1]: https://zips.z.cash/protocol/protocol.pdf#hsigcrh
pub(super) fn h_sig(
    random_seed: &RandomSeed,
    nf1: &Nullifier,
    nf2: &Nullifier,
    joinsplit_pub_key: &VerificationKeyBytes,
) -> [u8; 32] {
    let h_sig: [u8; 32] = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashComputehSig")
        .to_state()
        .update(&(<[u8; 32]>::from(random_seed))[..])
        .update(&(<[u8; 32]>::from(nf1))[..])
        .update(&(<[u8; 32]>::from(nf2))[..])
        .update(joinsplit_pub_key.as_ref())
        .finalize()
        .as_bytes()
        .try_into()
        .expect("32 byte array");
    h_sig
}

impl Description for (&JoinSplit<Groth16Proof>, &ed25519::VerificationKeyBytes) {
    /// Encodes the primary input for the JoinSplit proof statement as Bls12_381 base
    /// field elements, to match [`bellman::groth16::verify_proof()`].
    ///
    /// NB: [`jubjub::Fq`] is a type alias for [`bls12_381::Scalar`].
    ///
    /// `joinsplit_pub_key`: the JoinSplit public validation key for this JoinSplit, from
    /// the transaction. (All JoinSplits in a transaction share the same validation key.)
    ///
    /// This is not yet officially documented; see the reference implementation:
    /// <https://github.com/zcash/librustzcash/blob/0ec7f97c976d55e1a194a37b27f247e8887fca1d/zcash_proofs/src/sprout.rs#L152-L166>
    /// <https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc>
    //
    // The borrows are actually needed to avoid taking ownership
    #[allow(clippy::needless_borrow)]
    fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let (joinsplit, joinsplit_pub_key) = self;

        let rt: [u8; 32] = joinsplit.anchor.into();
        let mac1: [u8; 32] = (&joinsplit.vmacs[0]).into();
        let mac2: [u8; 32] = (&joinsplit.vmacs[1]).into();
        let nf1: [u8; 32] = (&joinsplit.nullifiers[0]).into();
        let nf2: [u8; 32] = (&joinsplit.nullifiers[1]).into();
        let cm1: [u8; 32] = (&joinsplit.commitments[0]).into();
        let cm2: [u8; 32] = (&joinsplit.commitments[1]).into();
        let vpub_old = joinsplit.vpub_old.to_bytes();
        let vpub_new = joinsplit.vpub_new.to_bytes();

        let h_sig = h_sig(
            &joinsplit.random_seed,
            &joinsplit.nullifiers[0],
            &joinsplit.nullifiers[1],
            joinsplit_pub_key,
        );

        // Prepare the public input for the verifier
        let mut public_input = Vec::with_capacity((32 * 8) + (8 * 2));
        public_input.extend(rt);
        public_input.extend(h_sig);
        public_input.extend(nf1);
        public_input.extend(mac1);
        public_input.extend(nf2);
        public_input.extend(mac2);
        public_input.extend(cm1);
        public_input.extend(cm2);
        public_input.extend(vpub_old);
        public_input.extend(vpub_new);

        let public_input = multipack::bytes_to_bits(&public_input);

        multipack::compute_multipacking(&public_input)
    }

    fn proof(&self) -> &Groth16Proof {
        &self.0.zkproof
    }
}

/// A Groth16 verification item, used as the request type of the service.
pub type Item = batch::Item<Bls12>;

/// A wrapper to allow a TryFrom blanket implementation of the [`Description`]
/// trait for the [`Item`] struct.
/// See <https://github.com/rust-lang/rust/issues/50133> for more details.
pub struct DescriptionWrapper<T>(pub T);

impl<T> TryFrom<DescriptionWrapper<&T>> for Item
where
    T: Description,
{
    type Error = TransactionError;

    fn try_from(input: DescriptionWrapper<&T>) -> Result<Self, Self::Error> {
        // # Consensus
        //
        // > Elements of a JoinSplit description MUST have the types given above
        //
        // https://zips.z.cash/protocol/protocol.pdf#joinsplitdesc
        //
        // This validates the 𝜋_{ZKJoinSplit} element. In #3179 we plan to validate
        // during deserialization, see [`JoinSplit::zcash_deserialize`].
        Ok(Item::from((
            bellman::groth16::Proof::read(&input.0.proof().0[..])
                .map_err(|e| TransactionError::MalformedGroth16(e.to_string()))?,
            input.0.primary_inputs(),
        )))
    }
}

/// Groth16 signature verifier implementation
///
/// This is the core implementation for the batch verification logic of the groth
/// verifier. It handles batching incoming requests, driving batches to
/// completion, and reporting results.
pub struct Verifier {
    batch: batch::Verifier<Bls12>,
    // Making this 'static makes managing lifetimes much easier.
    vk: &'static VerifyingKey<Bls12>,
    /// Broadcast sender used to send the result of a batch verification to each
    /// request source in the batch.
    tx: Sender<Result<(), VerificationError>>,
}

impl Verifier {
    fn new(vk: &'static VerifyingKey<Bls12>) -> Self {
        let batch = batch::Verifier::default();
        let (tx, _) = channel(1);
        Self { batch, vk, tx }
    }

    /// Flush the batch and return the result via the channel
    fn flush(&mut self) {
        let batch = mem::take(&mut self.batch);

        // # Correctness
        //
        // Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        //
        // TODO: use spawn_blocking to avoid blocking code running concurrently in this task
        let result = tokio::task::block_in_place(|| batch.verify(thread_rng(), self.vk));
        let _ = self.tx.send(result);

        // Use a new channel for each batch.
        // TODO: replace with a watch channel (#4729)
        let (tx, _) = channel(1);
        let _ = mem::replace(&mut self.tx, tx);
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
                                tracing::trace!(?result, "verified groth16 proof");
                                metrics::counter!("proofs.groth16.verified", 1);
                            } else {
                                tracing::trace!(?result, "invalid groth16 proof");
                                metrics::counter!("proofs.groth16.invalid", 1);
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

                self.flush();

                Box::pin(async { Ok(()) })
            }
        }
    }
}

impl Drop for Verifier {
    fn drop(&mut self) {
        // We need to flush the current batch in case there are still any pending futures.
        self.flush()
    }
}
