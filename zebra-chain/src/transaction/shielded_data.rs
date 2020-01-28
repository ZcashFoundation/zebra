use std::{fmt, io};

use futures::future::Either;
#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

// XXX this name seems too long?
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::proofs::Groth16Proof;
use crate::redjubjub::{self, Binding, SpendAuth};
use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

/// A _Spend Description_, as described in [protocol specification §7.3][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendDescription {
    /// A value commitment to the value of the input note.
    ///
    /// XXX refine to a specific type.
    pub cv: [u8; 32],
    /// A root of the Sapling note commitment tree at some block height in the past.
    pub anchor: SaplingNoteTreeRootHash,
    /// The nullifier of the input note.
    ///
    /// XXX refine to a specific type.
    pub nullifier: [u8; 32],
    /// The randomized public key for `spend_auth_sig`.
    pub rk: redjubjub::PublicKeyBytes<SpendAuth>,
    /// The ZK spend proof.
    pub zkproof: Groth16Proof,
    /// A signature authorizing this spend.
    pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
}

#[cfg(test)]
impl Arbitrary for SpendDescription {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            array::uniform32(any::<u8>()),
            any::<SaplingNoteTreeRootHash>(),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            any::<Groth16Proof>(),
            vec(any::<u8>(), 64),
        )
            .prop_map(
                |(cv_bytes, anchor, nullifier_bytes, rpk_bytes, proof, sig_bytes)| {
                    return Self {
                        cv: cv_bytes,
                        anchor: anchor,
                        nullifier: nullifier_bytes,
                        rk: redjubjub::PublicKeyBytes::from(rpk_bytes),
                        zkproof: proof,
                        spend_auth_sig: redjubjub::Signature::from({
                            let mut b = [0u8; 64];
                            b.copy_from_slice(sig_bytes.as_slice());
                            b
                        }),
                    };
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A _Output Description_, as described in [protocol specification §7.4][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct OutputDescription {
    /// A value commitment to the value of the input note.
    ///
    /// XXX refine to a specific type.
    pub cv: [u8; 32],
    /// The u-coordinate of the note commitment for the output note.
    ///
    /// XXX refine to a specific type.
    pub cmu: [u8; 32],
    /// An encoding of an ephemeral Jubjub public key.
    ///
    /// XXX refine to a Jubjub key agreement type, not RedJubjub.
    pub ephemeral_key: [u8; 32],
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: EncryptedCiphertext,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: OutCiphertext,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
}

/// Sapling-on-Groth16 spend and output descriptions.
#[derive(Clone, Debug)]
pub struct ShieldedData {
    /// Either a spend or output description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no spends or outputs.
    ///
    /// However, it's not necessary to access or process `first` and `rest`
    /// separately, as the [`ShieldedData::spends`] and [`ShieldedData::outputs`]
    /// methods provide iterators over all of the [`SpendDescription`]s and
    /// [`OutputDescription`]s.
    pub first: Either<SpendDescription, OutputDescription>,
    /// The rest of the [`SpendDescription`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::spends`] method provides an iterator
    /// over all spend descriptions.
    pub rest_spends: Vec<SpendDescription>,
    /// The rest of the [`OutputDescription`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::outputs`] method provides an iterator
    /// over all output descriptions.
    pub rest_outputs: Vec<OutputDescription>,
    /// A signature on the transaction hash.
    pub binding_sig: redjubjub::Signature<Binding>,
}

impl ShieldedData {
    /// Iterate over the [`SpendDescription`]s for this transaction.
    pub fn spends(&self) -> impl Iterator<Item = &SpendDescription> {
        match self.first {
            Either::Left(ref spend) => Some(spend),
            Either::Right(_) => None,
        }
        .into_iter()
        .chain(self.rest_spends.iter())
    }

    /// Iterate over the [`OutputDescription`]s for this transaction.
    pub fn outputs(&self) -> impl Iterator<Item = &OutputDescription> {
        match self.first {
            Either::Left(_) => None,
            Either::Right(ref output) => Some(output),
        }
        .into_iter()
        .chain(self.rest_outputs.iter())
    }
}

// Technically, it's possible to construct two equivalent representations
// of a ShieldedData with at least one spend and at least one output, depending
// on which goes in the `first` slot.  This is annoying but a smallish price to
// pay for structural validity.

impl std::cmp::PartialEq for ShieldedData {
    fn eq(&self, other: &Self) -> bool {
        // First check that the lengths match, so we know it is safe to use zip,
        // which truncates to the shorter of the two iterators.
        if self.spends().count() != other.spends().count() {
            return false;
        }
        if self.outputs().count() != other.outputs().count() {
            return false;
        }

        // Now check that the binding_sig, spends, outputs match.
        self.binding_sig == other.binding_sig
            && self.spends().zip(other.spends()).all(|(a, b)| a == b)
            && self.outputs().zip(other.outputs()).all(|(a, b)| a == b)
    }
}

impl std::cmp::Eq for ShieldedData {}

#[cfg(test)]
impl Arbitrary for ShieldedData {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            prop_oneof![
                any::<SpendDescription>().prop_map(Either::Left),
                any::<OutputDescription>().prop_map(Either::Right)
            ],
            vec(any::<SpendDescription>(), 0..10),
            vec(any::<OutputDescription>(), 0..10),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest_spends, rest_outputs, sig)| {
                return Self {
                    first: first,
                    rest_spends: rest_spends,
                    rest_outputs: rest_outputs,
                    binding_sig: redjubjub::Signature::from({
                        let mut b = [0u8; 64];
                        b.copy_from_slice(sig.as_slice());
                        b
                    }),
                };
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A ciphertext component for encrypted output notes.
pub struct EncryptedCiphertext(pub [u8; 580]);

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
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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
                return Self(bytes);
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A ciphertext component for encrypted output notes.
pub struct OutCiphertext(pub [u8; 80]);

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
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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
                return Self(bytes);
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
