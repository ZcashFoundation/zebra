//! Async Groth16 verifier service for Sprout JoinSplit proofs

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

use crate::BoxError;

use super::spawn_fifo_and_convert;

mod params;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vectors;

pub use params::SPROUT;

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

/// Compute the [h_{Sig} hash function][1] which is used in JoinSplit descriptions.
///
/// `random_seed`: the random seed from the JoinSplit description.
/// `nf1`: the first nullifier from the JoinSplit description.
/// `nf2`: the second nullifier from the JoinSplit description.
/// `joinsplit_pub_key`: the JoinSplit public validation key from the transaction.
///
/// [1]: https://zips.z.cash/protocol/protocol.pdf#hsigcrh
pub(super) fn h_sig(
    random_seed: &[u8; 32],
    nf1: &[u8; 32],
    nf2: &[u8; 32],
    joinsplit_pub_key: &[u8; 32],
) -> [u8; 32] {
    let h_sig: [u8; 32] = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashComputehSig")
        .to_state()
        .update(random_seed)
        .update(nf1)
        .update(nf2)
        .update(joinsplit_pub_key)
        .finalize()
        .as_bytes()
        .try_into()
        .expect("32 byte array");
    h_sig
}

/// Create a Groth16 verification [`Item`] from a JoinSplit description and public key.
pub fn joinsplit_to_item(
    js: &zcash_primitives::transaction::components::sprout::JsDescription,
    joinsplit_pub_key: &[u8; 32],
) -> Result<Item, TransactionError> {
    let rt = js.anchor();
    let nf1 = &js.nullifiers()[0];
    let nf2 = &js.nullifiers()[1];
    let mac1 = &js.macs()[0];
    let mac2 = &js.macs()[1];
    let cm1 = &js.commitments()[0];
    let cm2 = &js.commitments()[1];

    let h_sig = h_sig(js.random_seed(), nf1, nf2, joinsplit_pub_key);

    let vpub_old = js.vpub_old().to_i64_le_bytes();
    let vpub_new = js.vpub_new().to_i64_le_bytes();

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
    let primary_inputs = multipack::compute_multipacking(&public_input);

    // V4 transactions always use Groth16 proofs for JoinSplits (V2/V3 use PHGR13).
    // The proof type is determined at deserialization time by the transaction version
    // (see zcash_primitives JsDescription::read), so a V4 sprout bundle will always
    // contain Groth16 proofs. This function must only be called for V4 transactions.
    let proof_bytes = js.groth_proof_bytes().ok_or_else(|| {
        TransactionError::MalformedGroth16(
            "expected Groth16 proof in JoinSplit but found PHGR13 (wrong transaction version?)"
                .into(),
        )
    })?;

    let proof = bellman::groth16::Proof::read(&proof_bytes[..])
        .map_err(|e| TransactionError::MalformedGroth16(e.to_string()))?;

    Ok(Item::from((proof, primary_inputs)))
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
