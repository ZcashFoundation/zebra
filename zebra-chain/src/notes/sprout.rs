//!
#![allow(dead_code)]

use super::{memo::Memo, *};
use crate::serde_helpers;
use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::types::amount::{Amount, NonNegative};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt,
    io::{self},
};

///
pub struct Note {
    // TODO: refine type as a SHA-256d output derived from a spending key.
    paying_key: [u8; 32],
    value: Amount<NonNegative>,
    // TODO: refine type as the input to the PRF that results in a nullifier.
    nullifier_seed: [u8; 32],
    note_commitment_randomness: NoteCommitmentRandomness,
}

impl Note {
    pub fn note_commitment(&self) -> NoteCommitment {
        let leading_byte: u8 = 0xB0;
        let mut hasher = Sha256::default();
        hasher.input([leading_byte]);
        hasher.input(self.paying_key);
        hasher.input(self.value.to_bytes());
        hasher.input(self.nullifier_seed);
        hasher.input(self.note_commitment_randomness);
        let hash = hasher.result().into();
        NoteCommitment { hash }
    }
}

pub struct NoteCommitment {
    hash: [u8; 32],
}

/// The decrypted form of encrypted Sprout notes on the blockchain.
pub struct NotePlaintext {
    value: Amount<NonNegative>,
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
use proptest::{collection::vec, prelude::*};

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
