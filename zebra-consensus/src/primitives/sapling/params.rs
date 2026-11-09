//! Pinned Sapling verifying keys and an output-only, lazy coinbase prover.

use std::{convert::Infallible, io::Read, sync::LazyLock};

use bls12_381::{Bls12, Scalar};
use sapling_crypto::{
    circuit::{self, OutputParameters, OutputVerifyingKey, SpendParameters, SpendVerifyingKey},
    keys::EphemeralSecretKey,
    prover::{OutputProver, SpendProver},
    value::{NoteValue, ValueCommitTrapdoor},
    Diversifier, MerklePath, PaymentAddress, ProofGenerationKey, Rseed,
};

// Extracted from the official Sapling parameter files, authenticated by their BLAKE2b-512
// digests. The equality test checks these exact bytes against the bundled official parameters.
pub(super) const SPEND_VK: &[u8] = include_bytes!("sapling-spend.vk");
pub(super) const OUTPUT_VK: &[u8] = include_bytes!("sapling-output.vk");

/// The Sapling spend and output verifying keys, without any proving parameters.
pub(crate) static VERIFYING_KEYS: LazyLock<(SpendVerifyingKey, OutputVerifyingKey)> =
    LazyLock::new(|| {
        // sapling-crypto does not expose verifying-key readers. Its parameter reader accepts a
        // verifying key followed by five empty proving-vector lengths. No proving bytes are read
        // or retained; point encodings in the pinned keys are checked by the reader.
        let empty_vectors = [0u8; 20];
        let spend = SpendParameters::read(SPEND_VK.chain(&empty_vectors[..]), true)
            .expect("pinned Sapling spend verifying key must deserialize");
        let output = OutputParameters::read(OUTPUT_VK.chain(&empty_vectors[..]), true)
            .expect("pinned Sapling output verifying key must deserialize");
        (spend.verifying_key(), output.verifying_key())
    });

/// Output proving parameters, touched only when a Sapling coinbase output is actually proved.
static OUTPUT_PARAMETERS: LazyLock<OutputParameters> = LazyLock::new(|| {
    #[cfg(test)]
    super::tests::OUTPUT_PARAMETERS_LOADED.store(true, std::sync::atomic::Ordering::Relaxed);
    // Use only the output chunk: Zebra never proves Sapling spends. Keep the upstream
    // parameter-file integrity check, including the ceremony transcript after the parameters.
    let bytes = wagyu_zcash_parameters_6::load_partial_parameters();
    assert_eq!(
        blake2b_simd::blake2b(&bytes).to_hex().as_str(),
        "657e3d38dbb5cb5e7dd2970e8b03d69b4787dd907285b5a7f0790dcc8072f60bf593b32cc2d1c030e00ff5ae64bf84c5c3beb84ddc841d48264b4a171744d028",
        "bundled Sapling output parameters must match the official BLAKE2b-512 digest"
    );
    OutputParameters::read(&bytes[..], false)
        .expect("authenticated bundled Sapling output parameters must deserialize")
});

/// A Sapling prover restricted to coinbase transactions, which cannot contain shielded spends.
///
/// The output proving parameters remain untouched until an output proof is requested. This
/// allows transparent, Orchard, and Ironwood coinbases to use the same transaction builder
/// without loading Sapling proving parameters.
pub struct CoinbaseProver;

impl SpendProver for CoinbaseProver {
    type Proof = Infallible;

    fn prepare_circuit(
        _: ProofGenerationKey,
        _: Diversifier,
        _: Rseed,
        _: NoteValue,
        _: jubjub::Fr,
        _: ValueCommitTrapdoor,
        _: Scalar,
        _: MerklePath,
    ) -> Option<circuit::Spend> {
        unreachable!("coinbase transactions cannot contain Sapling spends")
    }

    fn create_proof<R: rand::Rng>(&self, _: circuit::Spend, _: &mut R) -> Self::Proof {
        unreachable!("coinbase transactions cannot contain Sapling spends")
    }

    fn encode_proof(proof: Self::Proof) -> sapling_crypto::bundle::GrothProofBytes {
        match proof {}
    }
}

impl OutputProver for CoinbaseProver {
    type Proof = groth16::Proof<Bls12>;

    fn prepare_circuit(
        esk: &EphemeralSecretKey,
        payment_address: PaymentAddress,
        rcm: jubjub::Fr,
        value: NoteValue,
        rcv: ValueCommitTrapdoor,
    ) -> circuit::Output {
        OutputParameters::prepare_circuit(esk, payment_address, rcm, value, rcv)
    }

    fn create_proof<R: rand::Rng>(&self, circuit: circuit::Output, rng: &mut R) -> Self::Proof {
        OUTPUT_PARAMETERS.create_proof(circuit, rng)
    }

    fn encode_proof(proof: Self::Proof) -> sapling_crypto::bundle::GrothProofBytes {
        OutputParameters::encode_proof(proof)
    }
}
