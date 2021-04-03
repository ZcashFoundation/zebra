use std::io;

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::Groth16Proof,
    serialization::{
        serde_helpers, SerializationError, TrustedPreallocate, ZcashDeserialize, ZcashSerialize,
    },
};

use super::{commitment, keys, note};

/// A _Output Description_, as described in [protocol specification §7.4][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Output {
    /// A value commitment to the value of the input note.
    pub cv: commitment::ValueCommitment,
    /// The u-coordinate of the note commitment for the output note.
    #[serde(with = "serde_helpers::Fq")]
    pub cm_u: jubjub::Fq,
    /// An encoding of an ephemeral Jubjub public key.
    pub ephemeral_key: keys::EphemeralPublicKey,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: note::EncryptedNote,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: note::WrappedNoteKey,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
}

impl Output {
    /// Encodes the primary inputs for the proof statement as 5 Bls12_381 base
    /// field elements, to match bellman::groth16::verify_proof.
    ///
    /// NB: jubjub::Fq is a type alias for bls12_381::Scalar.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#cctsaplingoutput
    pub fn primary_inputs(&self) -> Vec<jubjub::Fq> {
        let mut inputs = vec![];

        let cv_affine = jubjub::AffinePoint::from_bytes(self.cv.into()).unwrap();
        inputs.push(cv_affine.get_u());
        inputs.push(cv_affine.get_v());

        let epk_affine = jubjub::AffinePoint::from_bytes(self.ephemeral_key.into()).unwrap();
        inputs.push(epk_affine.get_u());
        inputs.push(epk_affine.get_v());

        inputs.push(self.cm_u);

        inputs
    }
}

impl ZcashSerialize for Output {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        self.cv.zcash_serialize(&mut writer)?;
        writer.write_all(&self.cm_u.to_bytes())?;
        self.ephemeral_key.zcash_serialize(&mut writer)?;
        self.enc_ciphertext.zcash_serialize(&mut writer)?;
        self.out_ciphertext.zcash_serialize(&mut writer)?;
        self.zkproof.zcash_serialize(&mut writer)?;
        Ok(())
    }
}

impl ZcashDeserialize for Output {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Output {
            cv: commitment::ValueCommitment::zcash_deserialize(&mut reader)?,
            cm_u: jubjub::Fq::zcash_deserialize(&mut reader)?,
            ephemeral_key: keys::EphemeralPublicKey::zcash_deserialize(&mut reader)?,
            enc_ciphertext: note::EncryptedNote::zcash_deserialize(&mut reader)?,
            out_ciphertext: note::WrappedNoteKey::zcash_deserialize(&mut reader)?,
            zkproof: Groth16Proof::zcash_deserialize(&mut reader)?,
        })
    }
}
/// An output contains: a 32 byte cv, a 32 byte cmu, a 32 byte ephemeral key
/// a 580 byte encCiphertext, an 80 byte outCiphertext, and a 192 byte zkproof
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
const OUTPUT_SIZE: u64 = 32 + 32 + 32 + 580 + 80 + 192;

/// The maximum number of outputs in a valid Zcash on-chain transaction.
///
/// If a transaction contains more outputs than can fit in maximally large block, it might be
/// valid on the network and in the mempool, but it can never be mined into a block. So
/// rejecting these large edge-case transactions can never break consensus
impl TrustedPreallocate for Output {
    fn max_allocation() -> u64 {
        // Since a serialized Vec<Output> uses at least one byte for its length,
        // the max allocation can never exceed (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE
        (MAX_BLOCK_BYTES - 1) / OUTPUT_SIZE
    }
}

#[cfg(test)]
mod test_trusted_preallocate {
    use super::{Output, MAX_BLOCK_BYTES, OUTPUT_SIZE};
    use crate::serialization::{TrustedPreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Confirm that each output takes exactly OUTPUT_SIZE bytes when serialized.
        /// This verifies that our calculated `TrustedPreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn output_size_is_small_enough(output in Output::arbitrary_with(())) {
            let serialized = output.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 == OUTPUT_SIZE)
        }

    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        /// Verify that...
        /// 1. The smallest disallowed vector of `Outputs`s is too large to fit in a Zcash block
        /// 2. The largest allowed vector is small enough to fit in a legal Zcash block
        #[test]
        fn output_max_allocation_is_big_enough(output in Output::arbitrary_with(())) {

            let max_allocation: usize = Output::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Output::max_allocation()+1) {
                smallest_disallowed_vec.push(output.clone());
            }
            let smallest_disallowed_serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Output::max_allocation());
            // Check that our smallest_disallowed_vec is too big to be included in a valid block
            // Note that a serialized block always includes at least one byte for the number of transactions,
            // so any serialized Vec<Output> at least MAX_BLOCK_BYTES long is too large to fit in a block.
            prop_assert!((smallest_disallowed_serialized.len() as u64) >= MAX_BLOCK_BYTES);

            // Create largest_allowed_vec by removing one element from smallest_disallowed_vec without copying (for efficiency)
            smallest_disallowed_vec.pop();
            let largest_allowed_vec = smallest_disallowed_vec;
            let largest_allowed_serialized = largest_allowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our largest_allowed_vec contains the maximum number of Outputs
            prop_assert!((largest_allowed_vec.len() as u64) == Output::max_allocation());
            // Check that our largest_allowed_vec is small enough to fit in a Zcash block.
            prop_assert!((largest_allowed_serialized.len() as u64) < MAX_BLOCK_BYTES);
        }
    }
}
