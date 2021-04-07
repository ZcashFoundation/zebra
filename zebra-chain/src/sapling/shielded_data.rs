//! Sapling shielded data for `V4` and `V5` `Transaction`s.
//!
//! Zebra uses a generic shielded data type for `V4` and `V5` transactions.
//! The `value_balance` change is handled using the default zero value.
//! The anchor change is handled using the `AnchorVariant` type trait.

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

/// Per-Spend Sapling anchors, used in Transaction V4 and the
/// `spends_per_anchor` method.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PerSpendAnchor {}

/// Shared Sapling anchors, used in Transaction V5.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SharedAnchor {}

/// This field is not present in this transaction version.
#[derive(Copy, Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FieldNotPresent;

impl AnchorVariant for PerSpendAnchor {
    type Shared = FieldNotPresent;
    type PerSpend = tree::Root;
}

impl AnchorVariant for SharedAnchor {
    type Shared = tree::Root;
    type PerSpend = FieldNotPresent;
}

/// A type trait to handle structural differences between V4 and V5 Sapling
/// Transaction anchors.
///
/// In Transaction V4, anchors are per-Spend. In Transaction V5, there is a
/// single transaction anchor for all Spends in a transaction.
pub trait AnchorVariant {
    /// The type of the shared anchor.
    type Shared: Clone + Debug + DeserializeOwned + Serialize + Eq + PartialEq;

    /// The type of the per-spend anchor.
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
///
/// # Differences between Transaction Versions
///
/// The Sapling `value_balance` field is optional in `Transaction::V5`, but
/// required in `Transaction::V4`. In both cases, if there is no `ShieldedData`,
/// then the field value must be zero. Therefore, only need to store
/// `value_balance` when there is some Sapling `ShieldedData`.
///
/// In `Transaction::V4`, each `Spend` has its own anchor. In `Transaction::V5`,
/// there is a single `shared_anchor` for the entire transaction. This
/// structural difference is modeled using the `AnchorVariant` type trait.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    /// The net value of Sapling spend transfers minus output transfers.
    pub value_balance: Amount,
    /// The shared anchor for all `Spend`s in this transaction.
    ///
    /// Some transaction versions do not have this field.
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

impl<AnchorV> ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
    Spend<PerSpendAnchor>: From<(Spend<AnchorV>, AnchorV::Shared)>,
{
    /// Iterate over the [`Spend`]s for this transaction.
    ///
    /// Returns `Spend<PerSpendAnchor>` regardless of the underlying transaction
    /// version, to allow generic verification over V4 and V5 transactions.
    ///
    /// # Correctness
    ///
    /// Do not use this function for serialization.
    pub fn spends_per_anchor(&self) -> impl Iterator<Item = Spend<PerSpendAnchor>> + '_ {
        self.spends()
            .cloned()
            .map(move |spend| Spend::<PerSpendAnchor>::from((spend, self.shared_anchor.clone())))
    }
}

impl<AnchorV> ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    /// Iterate over the [`Spend`]s for this transaction, returning them as
    /// their generic type.
    ///
    /// # Correctness
    ///
    /// Use this function for serialization.
    pub fn spends(&self) -> impl Iterator<Item = &Spend<AnchorV>> {
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
//
// A `ShieldedData<PerSpendAnchor>` can never be equal to a
// `ShieldedData<SharedAnchor>`, even if they have the same effects.

impl<AnchorV> std::cmp::PartialEq for ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone + PartialEq,
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

        // Now check that all the fields match
        self.value_balance == other.value_balance
            && self.shared_anchor == other.shared_anchor
            && self.binding_sig == other.binding_sig
            && self.spends().zip(other.spends()).all(|(a, b)| a == b)
            && self.outputs().zip(other.outputs()).all(|(a, b)| a == b)
    }
}

impl<AnchorV> std::cmp::Eq for ShieldedData<AnchorV> where AnchorV: AnchorVariant + Clone + PartialEq
{}
