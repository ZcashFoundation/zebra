#![allow(clippy::unit_arg)]
#![allow(dead_code)]

use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

use super::*;
use crate::{
    keys::sapling::{Diversifier, TransmissionKey},
    serde_helpers,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    types::amount::{Amount, NonNegative},
};

/// A Nullifier for Sapling transactions
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct Nullifier([u8; 32]);

impl From<[u8; 32]> for Nullifier {
    fn from(buf: [u8; 32]) -> Self {
        Self(buf)
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
pub struct CommitmentRandomness(redjubjub::Randomizer);

/// A Note represents that a value is spendable by the recipient who
/// holds the spending key corresponding to a given shielded payment
/// address.
pub struct Note {
    diversifier: Diversifier,
    transmission_key: TransmissionKey,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
}

impl Note {
    /// Construct a “windowed” Pedersen commitment by reusing a
    /// Perderson hash constructon, and adding a randomized point on
    /// the Jubjub curve.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#concretewindowedcommit
    pub fn commit(&self) -> NoteCommitment {
        unimplemented!()
    }
}

///
#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct NoteCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for NoteCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("NoteCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for NoteCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl Eq for NoteCommitment {}

impl From<NoteCommitment> for [u8; 32] {
    fn from(cm: NoteCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

impl ZcashSerialize for NoteCommitment {
    // The u-coordinate of the note commitment, for the output note
    // LEBS2OSP256(cm_u) where cm_u = Extract_J(r)(cm). ???
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for NoteCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
}

/// The decrypted form of encrypted Sapling notes on the blockchain.
pub struct NotePlaintext {
    diversifier: Diversifier,
    value: Amount<NonNegative>,
    rcm: CommitmentRandomness,
    memo: memo::Memo,
}

/// A ciphertext component for encrypted output notes.
#[derive(Deserialize, Serialize)]
pub struct EncryptedCiphertext(#[serde(with = "serde_helpers::BigArray")] pub [u8; 580]);

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
        let mut bytes = [0; 580];
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
        let mut bytes = [0; 580];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
impl Arbitrary for EncryptedCiphertext {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 580))
            .prop_map(|v| {
                let mut bytes = [0; 580];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A ciphertext component for encrypted output notes.
#[derive(Deserialize, Serialize)]
pub struct OutCiphertext(#[serde(with = "serde_helpers::BigArray")] pub [u8; 80]);

impl fmt::Debug for OutCiphertext {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("OutCiphertext")
            .field(&hex::encode(&self.0[..]))
            .finish()
    }
}

// These impls all only exist because of array length restrictions.

impl Copy for OutCiphertext {}

impl Clone for OutCiphertext {
    fn clone(&self) -> Self {
        let mut bytes = [0; 80];
        bytes[..].copy_from_slice(&self.0[..]);
        Self(bytes)
    }
}

impl PartialEq for OutCiphertext {
    fn eq(&self, other: &Self) -> bool {
        self.0[..] == other.0[..]
    }
}

impl Eq for OutCiphertext {}

impl ZcashSerialize for OutCiphertext {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0[..])?;
        Ok(())
    }
}

impl ZcashDeserialize for OutCiphertext {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut bytes = [0; 80];
        reader.read_exact(&mut bytes[..])?;
        Ok(Self(bytes))
    }
}

#[cfg(test)]
impl Arbitrary for OutCiphertext {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (vec(any::<u8>(), 80))
            .prop_map(|v| {
                let mut bytes = [0; 80];
                bytes.copy_from_slice(v.as_slice());
                Self(bytes)
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A Homomorphic Pedersen commitment to the value of a note, used in
/// Spend and Output Descriptions.
///
/// https://zips.z.cash/protocol/protocol.pdf#concretehomomorphiccommit
#[derive(Clone, Deserialize, PartialEq, Serialize)]
//#[cfg_attr(test, derive(proptest_derive::Arbitrary))]
pub struct ValueCommitment(#[serde(with = "serde_helpers::AffinePoint")] pub jubjub::AffinePoint);

impl fmt::Debug for ValueCommitment {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("ValueCommitment")
            .field("u", &hex::encode(self.0.get_u().to_bytes()))
            .field("v", &hex::encode(self.0.get_v().to_bytes()))
            .finish()
    }
}

impl From<[u8; 32]> for ValueCommitment {
    fn from(bytes: [u8; 32]) -> Self {
        Self(jubjub::AffinePoint::from_bytes(bytes).unwrap())
    }
}

impl Eq for ValueCommitment {}

impl From<ValueCommitment> for [u8; 32] {
    fn from(cm: ValueCommitment) -> [u8; 32] {
        cm.0.to_bytes()
    }
}

/// LEBS2OSP256(repr_J(cv))
///
/// https://zips.z.cash/protocol/protocol.pdf#spendencoding
/// https://zips.z.cash/protocol/protocol.pdf#jubjub
impl ZcashSerialize for ValueCommitment {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        writer.write_all(&self.0.to_bytes())?;
        Ok(())
    }
}

impl ZcashDeserialize for ValueCommitment {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(Self(
            jubjub::AffinePoint::from_bytes(reader.read_32_bytes()?).unwrap(),
        ))
    }
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

    #[test]
    fn out_ciphertext_roundtrip(oc in any::<OutCiphertext>()) {

        let mut data = Vec::new();

        oc.zcash_serialize(&mut data).expect("OutCiphertext should serialize");

        let oc2 = OutCiphertext::zcash_deserialize(&data[..]).expect("randomized OutCiphertext should deserialize");

        prop_assert_eq![oc, oc2];
    }
}
