use crate::proofs::ZkSnarkProof;

/// A _JoinSplit Description_, as described in [protocol specification §7.2][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// A ZK JoinSplit proof, either a [`Groth16Proof`] or a [`Bctv14Proof`].
    pub zkproof: P,
    /// A ciphertext component for this output note.
    ///
    /// XXX refine type to [T; 2] -- there are two ctxts
    /// XXX this should be a [[u8; 601]; 2] but we need trait impls.
    pub enc_ciphertexts: [Vec<u8>; 2],
}

/// A bundle of JoinSplit descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitData<P: ZkSnarkProof> {
    /// The first JoinSplit description, using proofs of type `P`.
    ///
    /// Storing this separately from `rest` ensures that it is impossible
    /// to construct an invalid `JoinSplitData` with no `JoinSplit`s.
    pub first: JoinSplit<P>,
    /// The rest of the JoinSplit descriptions, using proofs of type `P`.
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
