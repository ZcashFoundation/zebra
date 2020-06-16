#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
use serde::{Deserialize, Serialize};

use crate::{ed25519_zebra, notes::sprout, proofs::ZkSnarkProof};

/// A _JoinSplit Description_, as described in [protocol specification §7.2][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JoinSplit<P: ZkSnarkProof> {
    /// A value that the JoinSplit transfer removes from the transparent value
    /// pool.
    ///
    /// XXX refine to an Amount
    pub vpub_old: u64,
    /// A value that the JoinSplit transfer inserts into the transparent value
    /// pool.
    ///
    /// XXX refine to an Amount
    pub vpub_new: u64,
    /// A root of the Sprout note commitment tree at some block height in the
    /// past, or the root produced by a previous JoinSplit transfer in this
    /// transaction.
    ///
    /// XXX refine type
    pub anchor: [u8; 32],
    /// A nullifier for the input notes.
    ///
    /// XXX refine type to [T; 2] -- there are two nullifiers
    pub nullifiers: [[u8; 32]; 2],
    /// A note commitment for this output note.
    ///
    /// XXX refine type to [T; 2] -- there are two commitments
    pub commitments: [[u8; 32]; 2],
    /// An X25519 public key.
    pub ephemeral_key: x25519_dalek::PublicKey,
    /// A 256-bit seed that must be chosen independently at random for each
    /// JoinSplit description.
    pub random_seed: [u8; 32],
    /// A message authentication tag.
    ///
    /// XXX refine type to [T; 2] -- there are two macs
    pub vmacs: [[u8; 32]; 2],
    /// A ZK JoinSplit proof, either a
    /// [`Groth16Proof`](crate::proofs::Groth16Proof) or a
    /// [`Bctv14Proof`](crate::proofs::Bctv14Proof).
    #[serde(bound(serialize = "P: ZkSnarkProof", deserialize = "P: ZkSnarkProof"))]
    pub zkproof: P,
    /// A ciphertext component for this output note.
    pub enc_ciphertexts: [sprout::EncryptedCiphertext; 2],
}

// Because x25519_dalek::PublicKey does not impl PartialEq
impl<P: ZkSnarkProof> PartialEq for JoinSplit<P> {
    fn eq(&self, other: &Self) -> bool {
        self.vpub_old == other.vpub_old
            && self.vpub_new == other.vpub_new
            && self.anchor == other.anchor
            && self.nullifiers == other.nullifiers
            && self.commitments == other.commitments
            && self.ephemeral_key.as_bytes() == other.ephemeral_key.as_bytes()
            && self.random_seed == other.random_seed
            && self.vmacs == other.vmacs
            && self.zkproof == other.zkproof
            && self.enc_ciphertexts == other.enc_ciphertexts
    }
}

// Because x25519_dalek::PublicKey does not impl Eq
impl<P: ZkSnarkProof> Eq for JoinSplit<P> {}

#[cfg(test)]
impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplit<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<u64>(),
            any::<u64>(),
            array::uniform32(any::<u8>()),
            array::uniform2(array::uniform32(any::<u8>())),
            array::uniform2(array::uniform32(any::<u8>())),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
            array::uniform2(array::uniform32(any::<u8>())),
            any::<P>(),
            array::uniform2(any::<sprout::EncryptedCiphertext>()),
        )
            .prop_map(
                |(
                    vpub_old,
                    vpub_new,
                    anchor,
                    nullifiers,
                    commitments,
                    ephemeral_key_bytes,
                    random_seed,
                    vmacs,
                    zkproof,
                    enc_ciphertexts,
                )| {
                    Self {
                        vpub_old,
                        vpub_new,
                        anchor,
                        nullifiers,
                        commitments,
                        ephemeral_key: x25519_dalek::PublicKey::from(ephemeral_key_bytes),
                        random_seed,
                        vmacs,
                        zkproof,
                        enc_ciphertexts,
                    }
                },
            )
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A bundle of JoinSplit descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSplitData<P: ZkSnarkProof> {
    /// The first JoinSplit description, using proofs of type `P`.
    ///
    /// Storing this separately from `rest` ensures that it is impossible
    /// to construct an invalid `JoinSplitData` with no `JoinSplit`s.
    ///
    /// However, it's not necessary to access or process `first` and `rest`
    /// separately, as the [`JoinSplitData::joinsplits`] method provides an
    /// iterator over all of the `JoinSplit`s.
    #[serde(bound(
        serialize = "JoinSplit<P>: Serialize",
        deserialize = "JoinSplit<P>: Deserialize<'de>"
    ))]
    pub first: JoinSplit<P>,
    /// The rest of the JoinSplit descriptions, using proofs of type `P`.
    ///
    /// The [`JoinSplitData::joinsplits`] method provides an iterator over
    /// all `JoinSplit`s.
    #[serde(bound(
        serialize = "JoinSplit<P>: Serialize",
        deserialize = "JoinSplit<P>: Deserialize<'de>"
    ))]
    pub rest: Vec<JoinSplit<P>>,
    /// The public key for the JoinSplit signature.
    pub pub_key: ed25519_zebra::VerificationKeyBytes,
    /// The JoinSplit signature.
    pub sig: ed25519_zebra::Signature,
}

impl<P: ZkSnarkProof> JoinSplitData<P> {
    /// Iterate over the [`JoinSplit`]s in `self`.
    pub fn joinsplits(&self) -> impl Iterator<Item = &JoinSplit<P>> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }
}

#[cfg(test)]
impl<P: ZkSnarkProof + Arbitrary + 'static> Arbitrary for JoinSplitData<P> {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<JoinSplit<P>>(),
            vec(any::<JoinSplit<P>>(), 0..10),
            array::uniform32(any::<u8>()),
            vec(any::<u8>(), 64),
        )
            .prop_map(|(first, rest, pub_key_bytes, sig_bytes)| Self {
                first,
                rest,
                pub_key: ed25519_zebra::VerificationKeyBytes::from(pub_key_bytes),
                sig: ed25519_zebra::Signature::from({
                    let mut b = [0u8; 64];
                    b.copy_from_slice(sig_bytes.as_slice());
                    b
                }),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}
