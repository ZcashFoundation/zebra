use std::io;

use crate::{
    block::MAX_BLOCK_BYTES,
    primitives::Groth16Proof,
    serialization::{
        serde_helpers, SafePreallocate, SerializationError, ZcashDeserialize, ZcashSerialize,
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

/// We can never receive more outputs in a single message from an honest peer than would fit in a single block
impl SafePreallocate for Output {
    fn max_allocation() -> u64 {
        MAX_BLOCK_BYTES / OUTPUT_SIZE
    }
}

#[cfg(test)]
mod test_safe_preallocate {
    use super::{Output, MAX_BLOCK_BYTES, OUTPUT_SIZE};
    use crate::serialization::{SafePreallocate, ZcashSerialize};
    use proptest::prelude::*;
    use std::convert::TryInto;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Confirm that each output takes at least OUTPUT_SIZE bytes when serialized.
        /// This verifies that our calculated `SafePreallocate::max_allocation()` is indeed an upper bound.
        #[test]
        fn output_size_is_small_enough(output in Output::arbitrary_with(())) {
            let serialized = output.zcash_serialize_to_vec().expect("Serialization to vec must succeed");
            prop_assert!(serialized.len() as u64 == OUTPUT_SIZE)
        }

    }
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]
        #[test]
        fn output_max_allocation_is_big_enough(output in Output::arbitrary_with(())) {

            let max_allocation: usize = Output::max_allocation().try_into().unwrap();
            let mut smallest_disallowed_vec = Vec::with_capacity(max_allocation + 1);
            for _ in 0..(Output::max_allocation()+1) {
                smallest_disallowed_vec.push(output.clone());
            }
            let serialized = smallest_disallowed_vec.zcash_serialize_to_vec().expect("Serialization to vec must succeed");

            // Check that our smallest_disallowed_vec is only one item larger than the limit
            prop_assert!(((smallest_disallowed_vec.len() - 1) as u64) == Output::max_allocation());
            // Check that our smallest_disallowed_vec is too big to be included in a valid block
            prop_assert!(serialized.len() as u64 >= MAX_BLOCK_BYTES);
        }
    }
}
