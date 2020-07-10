//!
#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use std::{fmt, io};

use byteorder::{ByteOrder, LittleEndian};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    keys::sprout::{PayingKey, SpendingKey},
    serde_helpers,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    types::amount::{Amount, NonNegative},
};

use super::memo::Memo;

/// PRF^nf is used to derive a Sprout nullifer from the receiver's
/// spending key a_sk and a nullifier seed ρ, instantiated using the
/// SHA-256 compression function.
///
/// https://zips.z.cash/protocol/protocol.pdf#abstractprfs
/// https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
fn prf_nf(a_sk: [u8; 32], rho: [u8; 32]) -> [u8; 32] {
    let mut state = [0u32; 8];
    let mut block = [0u8; 64];

    block[0..32].copy_from_slice(&a_sk[..]);
    // The first four bits –i.e. the most signicant four bits of the
    // first byte– are used to separate distinct uses
    // ofSHA256Compress, ensuring that the functions are independent.
    block[0] |= 0b1110_0000;

    block[32..].copy_from_slice(&rho[..]);

    sha2::compress256(&mut state, &block);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

/// Nullifier seed, named rho in the [spec][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents
#[derive(Clone, Copy)]
struct NullifierSeed([u8; 32]);

impl AsRef<[u8]> for NullifierSeed {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; 32]> for NullifierSeed {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// A Nullifier for Sprout transactions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Nullifier([u8; 32]);

impl From<NullifierSeed> for [u8; 32] {
    fn from(rho: NullifierSeed) -> Self {
        rho.0
    }
}

impl From<(SpendingKey, NullifierSeed)> for Nullifier {
    fn from((a_sk, rho): (SpendingKey, NullifierSeed)) -> Self {
        Self(prf_nf(a_sk.into(), rho.into()))
    }
}

impl ZcashDeserialize for Nullifier {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let bytes = reader.read_32_bytes()?;

        Ok(Self(bytes))
    }
}

impl ZcashSerialize for Nullifier {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])
    }
}

/// The randomness used in the Pedersen Hash for note commitment.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NoteCommitmentRandomness(pub [u8; 32]);

impl AsRef<[u8]> for NoteCommitmentRandomness {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
pub struct Note {
    paying_key: PayingKey,
    value: Amount<NonNegative>,
    nullifier_seed: NullifierSeed,
    note_commitment_randomness: NoteCommitmentRandomness,
}

impl Note {
    pub fn commitment(&self) -> NoteCommitment {
        let leading_byte: u8 = 0xB0;
        let mut hasher = Sha256::default();
        hasher.input([leading_byte]);
        hasher.input(self.paying_key);
        hasher.input(self.value.to_bytes());
        hasher.input(self.nullifier_seed);
        hasher.input(self.note_commitment_randomness);

        NoteCommitment(hasher.result().into())
    }
}

///
pub struct NoteCommitment([u8; 32]);

/// The decrypted form of encrypted Sprout notes on the blockchain.
pub struct NotePlaintext {
    value: Amount<NonNegative>,
    // TODO: refine type
    rho: [u8; 32],
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
