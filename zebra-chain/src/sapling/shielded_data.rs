use futures::future::Either;

use crate::{
    amount::Amount,
    primitives::redjubjub::{Binding, Signature},
    sapling::{tree, Nullifier, Output, Spend, ValueCommitment},
    serialization::serde_helpers,
};

use serde::{de::DeserializeOwned, Serialize};
use std::{
    cmp::{Eq, PartialEq},
    fmt::Debug,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PerSpendAnchor {}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SharedAnchor {}

impl AnchorVariant for PerSpendAnchor {
    type Shared = ();
    type PerSpend = tree::Root;
}
impl AnchorVariant for SharedAnchor {
    type Shared = tree::Root;
    type PerSpend = ();
}

pub trait AnchorVariant {
    type Shared: Clone + Debug + DeserializeOwned + Serialize + Eq + PartialEq;
    type PerSpend: Clone + Debug + DeserializeOwned + Serialize + Eq + PartialEq;
}

/// A bundle of [`Spend`] and [`Output`] descriptions and signature data.
///
/// Spend and Output descriptions are optional, but Zcash transactions must
/// include a binding signature if and only if there is at least one Spend *or*
/// Output description. This wrapper type bundles at least one Spend or Output
/// description with the required signature data, so that an
/// `Option<ShieldedData>` correctly models the presence or absence of any
/// shielded data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShieldedData<AnchorV: AnchorVariant> {
    /// The net value of Sapling spend transfers minus output transfers.
    pub value_balance: Amount,
    /// The shared anchor for all `Spend`s in this transaction.
    pub shared_anchor: AnchorV::Shared,
    /// Either a spend or output description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no spends or outputs.
    ///
    /// However, it's not necessary to access or process `first` and `rest`
    /// separately, as the [`ShieldedData::spends`] and [`ShieldedData::outputs`]
    /// methods provide iterators over all of the [`Spend`]s and
    /// [`Output`]s.
    #[serde(with = "serde_helpers::Either")]
    pub first: Either<Spend<AnchorV>, Output>,
    /// The rest of the [`Spend`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::spends`] method provides an iterator
    /// over all spend descriptions.
    pub rest_spends: Vec<Spend<AnchorV>>,
    /// The rest of the [`Output`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::outputs`] method provides an iterator
    /// over all output descriptions.
    pub rest_outputs: Vec<Output>,
    /// A signature on the transaction hash.
    pub binding_sig: Signature<Binding>,
}

impl<T> ShieldedData<T>
where
    T: AnchorVariant,
{
    /// Iterate over the [`Spend`]s for this transaction.
    pub fn spends(&self) -> impl Iterator<Item = &Spend<T>> {
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

    /// Collect the [`Nullifier`]s for this transaction, if it contains
    /// [`Spend`]s.
    pub fn nullifiers(&self) -> impl Iterator<Item = &Nullifier> {
        self.spends().map(|spend| &spend.nullifier)
    }

    /// Collect the cm_u's for this transaction, if it contains [`Output`]s.
    pub fn note_commitments(&self) -> impl Iterator<Item = &jubjub::Fq> {
        self.outputs().map(|output| &output.cm_u)
    }

    /// Calculate the Spend/Output binding verification key.
    ///
    /// Getting the binding signature validating key from the Spend and Output
    /// description value commitments and the balancing value implicitly checks
    /// that the balancing value is consistent with the value transfered in the
    /// Spend and Output descriptions but also proves that the signer knew the
    /// randomness used for the Spend and Output value commitments, which
    /// prevents replays of Output descriptions.
    ///
    /// The net value of Spend transfers minus Output transfers in a transaction
    /// is called the balancing value, measured in zatoshi as a signed integer
    /// v_balance.
    ///
    /// Consistency of v_balance with the value commitments in Spend
    /// descriptions and Output descriptions is enforced by the binding
    /// signature.
    ///
    /// Instead of generating a key pair at random, we generate it as a function
    /// of the value commitments in the Spend descriptions and Output
    /// descriptions of the transaction, and the balancing value.
    ///
    /// https://zips.z.cash/protocol/protocol.pdf#saplingbalance
    pub fn binding_verification_key(&self) -> redjubjub::VerificationKeyBytes<Binding> {
        let cv_old: ValueCommitment = self.spends().map(|spend| spend.cv).sum();
        let cv_new: ValueCommitment = self.outputs().map(|output| output.cv).sum();
        let cv_balance: ValueCommitment =
            ValueCommitment::new(jubjub::Fr::zero(), self.value_balance);

        let key_bytes: [u8; 32] = (cv_old - cv_new - cv_balance).into();

        key_bytes.into()
    }
}

// Technically, it's possible to construct two equivalent representations
// of a ShieldedData with at least one spend and at least one output, depending
// on which goes in the `first` slot.  This is annoying but a smallish price to
// pay for structural validity.

impl<T> std::cmp::PartialEq for ShieldedData<T>
where
    T: AnchorVariant,
{
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

impl<T> std::cmp::Eq for ShieldedData<T> where T: AnchorVariant {}
