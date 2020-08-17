use serde::{Deserialize, Serialize};

use crate::{
    primitives::{ed25519, ZkSnarkProof},
    sprout::JoinSplit,
};

/// A bundle of JoinSplit descriptions and signature data.
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
}
