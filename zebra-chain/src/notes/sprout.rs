//!
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::{
    fmt,
    io::{self},
};

#[cfg(test)]
use proptest::{collection::vec, prelude::*};

use crate::serde_helpers;
use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

use super::{memo::Memo, *};

///
pub struct Note {
    // TODO: refine type as a SHA-256d output derived from a spending key.
    paying_key: [u8; 32],
    value: u64,
    // TODO: refine type as the input to the PRF that results in a nullifier.
    nullifier_seed: [u8; 32],
    note_commitment_randomness: NoteCommitmentRandomness,
}

/// The decrypted form of encrypted Sprout notes on the blockchain.
pub struct NotePlaintext {
    value: u64,
    // TODO: refine type
    rho: [u8; 32],
    // TODO: refine as jub-jub appropriate in the base field.
    note_commitment_randomness: NoteCommitmentRandomness,
    memo: Memo,
}

/// A ciphertext component for encrypted output notes.
#[derive(Serialize, Deserialize)]
pub struct EncryptedCiphertext(#[serde(with = "serde_helpers::BigArray")] pub [u8; 601]);

impl fmt::Debug for EncryptedCiphertext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("EncryptedCiphertext")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for EncryptedCiphertext {}

impl Clone for EncryptedCiphertext {
    fn clone(&self) -> Self {
        let mut bytes = [0; 601];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl PartialEq for EncryptedCiphertext {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for EncryptedCiphertext {}

impl ZcashSerialize for EncryptedCiphertext {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for EncryptedCiphertext {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 601];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
impl Arbitrary for EncryptedCiphertext {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 601))
            .prop_map(|v| {
                let mut bytes = [0; 601];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
proptest! {

    #[test]
    fn encrypted_ciphertext_roundtrip(ec in any::<EncryptedCiphertext>()) {

        let mut data = Vec::new();

        ec.zcash_serialize(&mut data).expect("EncryptedCiphertext should serialize");

        let ec2 = EncryptedCiphertext::zcash_deserialize(&data[..]).expect("randomized EncryptedCiphertext should deserialize");

        prop_assert_eq![ec, ec2];
    }
}
