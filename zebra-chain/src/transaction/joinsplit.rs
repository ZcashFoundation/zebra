use crate::proofs::ZkSnarkProof;

/// Describes input notes to a Sprout transaction.
///
/// The [protocol specification §7.2][ps] describes these fields as being encoded
/// separately into two arrays of the same length. Instead, by bundling them
/// together into one structure, we can ensure that it's not possible to create a
/// JoinSplit description with mismatched array lengths. This means we do not
/// need to maintain any invariants about equal array lengths.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SproutInputNoteData {
    /// A nullifier for the input note.
    ///
    /// XXX refine type
    pub nullifier: [u8; 32],
    /// A message authentication tag.
    ///
    /// XXX refine type
    pub vmac: [u8; 32],
}

/// Describes output notes from a Sprout transaction.
///
/// The [protocol specification §7.2][ps] describes these fields as being encoded
/// separately into two arrays of the same length. Instead, by bundling them
/// together into one structure, we can ensure that it's not possible to create a
/// JoinSplit description with mismatched array lengths. This means we do not
/// need to maintain any invariants about equal array lengths.
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SproutOutputNoteData {
    /// A note commitment for this output note.
    ///
    /// XXX refine type
    pub commitment: [u8; 32],
    /// A ciphertext component for this output note.
    ///
    /// XXX refine type
    /// XXX this should be a [u8; 601] but we need trait impls.
    pub enc_ciphertext: Vec<u8>,
}

/// A _JoinSplit Description_, as described in [protocol specification §7.2][ps].
///
/// [ps]: https://zips.z.cash/protocol/protocol.pdf#joinsplitencoding
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplit<P: ZkSnarkProof> {
    /// A value that the JoinSplit transfer removes from the transparent value
    /// pool.
    ///
    /// XXX refine to an Amount
    vpub_old: u64,
    /// A value that the JoinSplit transfer inserts into the transparent value
    /// pool.
    ///
    /// XXX refine to an Amount
    vpub_new: u64,
    /// A root of the Sprout note commitment tree at some block height in the
    /// past, or the root produced by a previous JoinSplit transfer in this
    /// transaction.
    ///
    /// XXX refine type
    anchor: [u8; 32],
    /// An X25519 public key.
    ///
    /// XXX refine to an x25519-dalek type?
    ephemeral_key: [u8; 32],
    /// A 256-bit seed that must be chosen independently at random for each
    /// JoinSplit description.
    random_seed: [u8; 32],
    /// A sequence of input notes for this transaction.
    input_notes: Vec<SproutInputNoteData>,
    /// A sequence of output notes for this transaction.
    output_notes: Vec<SproutOutputNoteData>,
    /// A ZK JoinSplit proof, either a [`Groth16Proof`] or a [`Bctv14Proof`].
    zkproof: P,
}

/// A bundle of JoinSplit descriptions and signature data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinSplitData<P: ZkSnarkProof> {
    /// A sequence of JoinSplit descriptions using proofs of type `P`.
    pub joinsplits: Vec<JoinSplit<P>>,
    /// The public key for the JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 pubkey.
    pub pub_key: [u8; 32],
    /// The JoinSplit signature.
    // XXX refine to a Zcash-flavored Ed25519 signature.
    // for now it's [u64; 8] rather than [u8; 64] to get trait impls
    pub sig: [u64; 8],
}
