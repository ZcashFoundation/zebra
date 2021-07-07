use serde::{Deserialize, Serialize};

use crate::{
    primitives::{ed25519, ZkSnarkProof},
    sprout::{self, JoinSplit, Nullifier},
};

/// A bundle of [`JoinSplit`] descriptions and signature data.
///
/// JoinSplit descriptions are optional, but Zcash transactions must include a
/// JoinSplit signature and verification key if and only if there is at least one
/// JoinSplit description. This wrapper type bundles at least one JoinSplit
/// description with the required signature data, so that an
/// `Option<JoinSplitData>` correctly models the presence or absence of any
/// JoinSplit data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinSplitData<P: ZkSnarkProof> {
    /// The first JoinSplit description, using proofs of type `P`.
    ///
    /// Storing this separately from `rest` ensures that it is impossible
    /// to construct an invalid `JoinSplitData` with no `JoinSplit`s.
    ///`
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
    pub pub_key: ed25519::VerificationKeyBytes,
    /// The JoinSplit signature.
    pub sig: ed25519::Signature,
}

impl<P: ZkSnarkProof> JoinSplitData<P> {
    /// Iterate over the [`JoinSplit`]s in `self`.
    pub fn joinsplits(&self) -> impl Iterator<Item = &JoinSplit<P>> {
        std::iter::once(&self.first).chain(self.rest.iter())
    }

    /// Iterate over the [`Nullifier`]s in `self`.
    pub fn nullifiers(&self) -> impl Iterator<Item = &Nullifier> {
        self.joinsplits()
            .flat_map(|joinsplit| joinsplit.nullifiers.iter())
    }

    /// Collect the Sprout note commitments  for this transaction, if it contains [`Output`]s.
    pub fn note_commitments(&self) -> impl Iterator<Item = &sprout::commitment::NoteCommitment> {
        self.joinsplits()
            .flat_map(|joinsplit| &joinsplit.commitments)
    }
}
