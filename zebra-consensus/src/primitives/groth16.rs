//! Async Groth16 batch verifier service

use std::fmt;

use bellman::{
    gadgets::multipack,
    groth16::{batch, PreparedVerifyingKey, VerifyingKey},
    VerificationError,
};
use bls12_381::Bls12;
use futures::{future::BoxFuture, FutureExt};
use once_cell::sync::Lazy;

use tokio::sync::watch;
use tower::util::ServiceFn;

use tower_batch_control::RequestWeight;
use tower_fallback::BoxedError;

use zebra_chain::{
    primitives::{
        ed25519::{self, VerificationKeyBytes},
        Groth16Proof,
    },
    sapling::{Output, PerSpendAnchor, Spend},
    sprout::{JoinSplit, Nullifier, RandomSeed},
};

use crate::BoxError;

use super::spawn_fifo_and_convert;

mod params;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vectors;

pub use params::{SAPLING, SPROUT};

use crate::error::TransactionError;

/// The type of verification results.
type VerifyResult = Result<(), VerificationError>;

/// The type of the batch sender channel.
type Sender = watch::Sender<Option<VerifyResult>>;

/// The type of the batch item.
/// This is a newtype around a Groth16 verification item.
#[derive(Clone, Debug)]
pub struct Item(batch::Item<Bls12>);

impl RequestWeight for Item {}

impl<T: Into<batch::Item<Bls12>>> From<T> for Item {
    fn from(value: T) -> Self {
        Self(value.into())
    }
}

impl Item {
    /// Convenience method to call a method on the inner value to perform non-batched verification.
    pub fn verify_single(self, pvk: &PreparedVerifyingKey<Bls12>) -> VerifyResult {
        self.0.verify_single(pvk)
    }
}

/// The type of a raw verifying key.
/// This is the key used to verify batches.
pub type BatchVerifyingKey = VerifyingKey<Bls12>;

/// The type of a prepared verifying key.
/// This is the key used to verify individual items.
pub type ItemVerifyingKey = PreparedVerifyingKey<Bls12>;

/// Global batch verification context for Groth16 proofs of JoinSplit statements.
///
/// This service does not yet batch verifications, see
/// <https://github.com/ZcashFoundation/zebra/issues/3127>
///
/// Note that making a `Service` call requires mutable access to the service, so
/// you should call `.clone()` on the global handle to create a local, mutable
/// handle.
pub static JOINSPLIT_VERIFIER: Lazy<
    ServiceFn<fn(Item) -> BoxFuture<'static, Result<(), BoxedError>>>,
> = Lazy::new(|| {
    // We just need a Service to use: there is no batch verification for JoinSplits.
    //
    // See the note on [`SPEND_VERIFIER`] for details.
    tower::service_fn(
        (|item: Item| {
            // TODO: Simplify the call stack here.
            Verifier::verify_single_spawning(item, SPROUT.prepared_verifying_key())
                .map(|result| {
                    result
                        .map_err(|e| TransactionError::Groth16(e.to_string()))
                        .map_err(tower_fallback::BoxedError::from)
                })
                .boxed()
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

        let cv_affine = jubjub::AffinePoint::from_bytes(self.cv.0.to_bytes()).unwrap();
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

        let cv_affine = jubjub::AffinePoint::from_bytes(self.cv.0.to_bytes()).unwrap();
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        let epk_affine = jubjub::AffinePoint::from_bytes(self.ephemeral_key.into()).unwrap();
        inputs.push(epk_affine.get_u());
        inputs.push(epk_affine.get_v());

        inputs.push(jubjub::Fq::from_bytes(&self.cm_u.to_bytes()).unwrap());

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
    /// A channel for broadcasting the result of a batch to the futures for each batch item.
    ///
    /// Each batch gets a newly created channel, so there is only ever one result sent per channel.
    /// Tokio doesn't have a oneshot multi-consumer channel, so we use a watch channel.
    tx: Sender,
}

impl Verifier {
    /// Verify a single item using a thread pool, and return the result.
    async fn verify_single_spawning(
        item: Item,
        pvk: &'static ItemVerifyingKey,
    ) -> Result<(), BoxError> {
        // Correctness: Do CPU-intensive work on a dedicated thread, to avoid blocking other futures.
        spawn_fifo_and_convert(move || item.verify_single(pvk)).await
    }
}

impl fmt::Debug for Verifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = "Verifier";
        f.debug_struct(name)
            .field("batch", &"..")
            .field("tx", &self.tx)
            .finish()
    }
}
