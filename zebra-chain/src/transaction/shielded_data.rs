use futures::future::Either;

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, collection::vec, prelude::*};

// XXX this name seems too long?
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::notes::sapling;
use crate::proofs::Groth16Proof;
use crate::redjubjub::{self, Binding, SpendAuth};
use crate::serde_helpers;

/// A _Spend Description_, as described in [protocol specification §7.3][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#spendencoding
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
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
impl Arbitrary for Spend {
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
                |(cv_bytes, anchor, nullifier_bytes, rpk_bytes, proof, sig_bytes)| Self {
                    anchor,
                    cv: cv_bytes,
                    nullifier: nullifier_bytes,
                    rk: redjubjub::PublicKeyBytes::from(rpk_bytes),
                    zkproof: proof,
                    spend_auth_sig: redjubjub::Signature::from({
                        let mut b = [0u8; 64];
                        b.copy_from_slice(sig_bytes.as_slice());
                        b
                    }),
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A _Output Description_, as described in [protocol specification §7.4][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Output {
    /// A value commitment to the value of the input note.
    ///
    /// XXX refine to a specific type.
    pub cv: [u8; 32],
    /// The u-coordinate of the note commitment for the output note.
    ///
    /// XXX refine to a specific type.
    pub cmu: [u8; 32],
    /// An encoding of an ephemeral Jubjub public key.
    #[serde(with = "serde_helpers::AffinePoint")]
    pub ephemeral_key: jubjub::AffinePoint,
    /// A ciphertext component for the encrypted output note.
    pub enc_ciphertext: sapling::EncryptedCiphertext,
    /// A ciphertext component for the encrypted output note.
    pub out_ciphertext: sapling::OutCiphertext,
    /// The ZK output proof.
    pub zkproof: Groth16Proof,
}

impl Eq for Output {}

#[cfg(test)]
impl Arbitrary for Output {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()).prop_filter("Valid jubjub::AffinePoint", |b| {
                jubjub::AffinePoint::from_bytes(*b).is_some().unwrap_u8() == 1
            }),
            any::<sapling::EncryptedCiphertext>(),
            any::<sapling::OutCiphertext>(),
            any::<Groth16Proof>(),
        )
            .prop_map(
                |(cv, cmu, ephemeral_key_bytes, enc_ciphertext, out_ciphertext, zkproof)| Self {
                    cv,
                    cmu,
                    ephemeral_key: jubjub::AffinePoint::from_bytes(ephemeral_key_bytes).unwrap(),
                    enc_ciphertext,
                    out_ciphertext,
                    zkproof,
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// Sapling-on-Groth16 spend and output descriptions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShieldedData {
    /// Either a spend or output description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no spends or outputs.
    ///
    /// However, it's not necessary to access or process `first` and `rest`
    /// separately, as the [`ShieldedData::spends`] and [`ShieldedData::outputs`]
    /// methods provide iterators over all of the [`SpendDescription`]s and
    /// [`Output`]s.
    #[serde(with = "serde_helpers::Either")]
    pub first: Either<Spend, Output>,
    /// The rest of the [`Spend`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::spends`] method provides an iterator
    /// over all spend descriptions.
    pub rest_spends: Vec<Spend>,
    /// The rest of the [`Output`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::outputs`] method provides an iterator
    /// over all output descriptions.
    pub rest_outputs: Vec<Output>,
    /// A signature on the transaction hash.
    pub binding_sig: redjubjub::Signature<Binding>,
}

impl ShieldedData {
    /// Iterate over the [`Spend`]s for this transaction.
    pub fn spends(&self) -> impl Iterator<Item = &Spend> {
        match self.first {
            Either::Left(ref spend) => Some(spend),
            Either::Right(_) => None,
        }
        .into_iter()
        .chain(self.rest_spends.iter())
    }

    /// Iterate over the [`Output`]s for this transaction.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
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
                any::<Spend>().prop_map(Either::Left),
                any::<Output>().prop_map(Either::Right)
            ],
            vec(any::<Spend>(), 0..10),
            vec(any::<Output>(), 0..10),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest_spends, rest_outputs, sig_bytes)| Self {
                first,
                rest_spends,
                rest_outputs,
                binding_sig: redjubjub::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
