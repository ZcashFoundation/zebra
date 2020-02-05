use std::{
    fmt,
    io::{self},
};

#[cfg(test)]
use proptest::{array, collection::vec, prelude::*};
#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::{
    proofs::ZkSnarkProof,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// A _JoinSplit Description_, as described in [protocol specification §7.2][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Arbitrary))]
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
    ///
    /// XXX refine to an x25519-dalek type?
    pub ephemeral_key: [u8; 32],
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
    pub zkproof: P,
    /// A ciphertext component for this output note.
    pub enc_ciphertexts: [EncryptedCiphertext; 2],
}

/// A bundle of JoinSplit descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitData<P: ZkSnarkProof> {
    /// The first JoinSplit description, using proofs of type `P`.
    ///
    /// Storing this separately from `rest` ensures that it is impossible
    /// to construct an invalid `JoinSplitData` with no `JoinSplit`s.
    ///
    /// However, it's not necessary to access or process `first` and `rest`
    /// separately, as the [`JoinSplitData::joinsplits`] method provides an
    /// iterator over all of the `JoinSplit`s.
    pub first: JoinSplit<P>,
    /// The rest of the JoinSplit descriptions, using proofs of type `P`.
    ///
    /// The [`JoinSplitData::joinsplits`] method provides an iterator over
    /// all `JoinSplit`s.
    pub rest: Vec<JoinSplit<P>>,
    /// The public key for the JoinSplit signature.
    pub pub_key: ed25519_zebra::PublicKeyBytes,
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
            .prop_map(|(first, rest, pub_key_bytes, sig_bytes)| {
                return Self {
                    first,
                    rest,
                    pub_key: ed25519_zebra::PublicKeyBytes::from(pub_key_bytes),
                    sig: ed25519_zebra::Signature::from({
                        let mut b = [0u8; 64];
                        b.copy_from_slice(sig_bytes.as_slice());
                        b
                    }),
                };
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

/// A ciphertext component for encrypted output notes.
// XXX move as part of #181 (note encryption implementation)
pub struct EncryptedCiphertext(pub [u8; 601]);

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
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), SerializationError> {
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
                return Self(bytes);
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
