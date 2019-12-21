use futures::future::Either;

// XXX this name seems too long?
use crate::note_commitment_tree::SaplingNoteTreeRootHash;
use crate::proofs::Groth16Proof;
use crate::redjubjub::{self, Binding, SpendAuth};

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

/// A _Output Description_, as described in [protocol specification §7.4][ps].
///
/// https://zips.z.cash/protocol/protocol.pdf#outputencoding
#[derive(Clone, Debug, PartialEq, Eq)]
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
    ///
    /// XXX refine to a specific type.
    /// XXX this is a Vec<u8> rather than a [u8; 580] to get trait impls
    pub enc_ciphertext: Vec<u8>,
    /// A ciphertext component for the encrypted output note.
    ///
    /// XXX refine to a specific type.
    /// XXX this is a [u64; 10] rather than a [u8; 80] to get trait impls
    pub out_ciphertext: [u64; 10],
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
    pub first: Either<SpendDescription, OutputDescription>,
    /// The rest of the [`SpendDescription`]s for this transaction.
    pub rest_spends: Vec<SpendDescription>,
    /// The rest of the [`OutputDescription`]s for this transaction.
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
