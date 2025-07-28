//! Sapling shielded data for `V4` and `V5` `Transaction`s.
//!
//! Zebra uses a generic shielded data type for `V4` and `V5` transactions.
//! The `value_balance` change is handled using the default zero value.
//! The anchor change is handled using the `AnchorVariant` type trait.

use std::{
    cmp::{max, Eq, PartialEq},
    fmt::{self, Debug},
};

use derive_getters::Getters;
use itertools::Itertools;
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use serde::{de::DeserializeOwned, Serialize};

use crate::{
    amount::Amount,
    primitives::{
        redjubjub::{Binding, Signature},
        Groth16Proof,
    },
    sapling::{
        output::OutputPrefixInTransactionV5, spend::SpendPrefixInTransactionV5, tree, Nullifier,
        Output, Spend, ValueCommitment,
    },
    serialization::{AtLeastOne, TrustedPreallocate},
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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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
/// then the field value must be zero. Therefore, we only need to store
/// `value_balance` when there is some Sapling `ShieldedData`.
///
/// In `Transaction::V4`, each `Spend` has its own anchor. In `Transaction::V5`,
/// there is a single `shared_anchor` for the entire transaction, which is only
/// present when there is at least one spend. These structural differences are
/// modeled using the `AnchorVariant` type trait and `TransferData` enum.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Getters)]
pub struct ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    /// The net value of Sapling spend transfers minus output transfers.
    /// Denoted as `valueBalanceSapling` in the spec.
    pub value_balance: Amount,

    /// A bundle of spends and outputs, containing at least one spend or
    /// output, in the order they appear in the transaction.
    ///
    /// In V5 transactions, also contains a shared anchor, if there are any
    /// spends.
    pub transfers: TransferData<AnchorV>,

    /// A signature on the transaction hash.
    /// Denoted as `bindingSigSapling` in the spec.
    pub binding_sig: Signature<Binding>,
}

/// A bundle of [`Spend`] and [`Output`] descriptions, and a shared anchor.
///
/// This wrapper type bundles at least one Spend or Output description with
/// the required anchor data, so that an `Option<ShieldedData>` (which contains
/// this type) correctly models the presence or absence of any spends and
/// shielded data, across both V4 and V5 transactions.
///
/// Specifically, TransferData ensures that:
/// * there is at least one spend or output, and
/// * the shared anchor is only present when there are spends.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TransferData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    /// A bundle containing at least one spend, and the shared spend anchor.
    /// There can also be zero or more outputs.
    ///
    /// In Transaction::V5, if there are any spends, there must also be a shared
    /// spend anchor.
    SpendsAndMaybeOutputs {
        /// The shared anchor for all `Spend`s in this transaction.
        ///
        /// The anchor is the root of the Sapling note commitment tree in a previous
        /// block. This root should be in the best chain for a transaction to be
        /// mined, and it must be in the relevant chain for a transaction to be
        /// valid.
        ///
        /// Some transaction versions have a per-spend anchor, rather than a shared
        /// anchor.
        ///
        /// Use the `shared_anchor` method to access this field.
        shared_anchor: AnchorV::Shared,

        /// At least one spend.
        ///
        /// Use the [`ShieldedData::spends`] method to get an iterator over the
        /// [`Spend`]s in this `TransferData`.
        spends: AtLeastOne<Spend<AnchorV>>,

        /// Maybe some outputs (can be empty), in the order they appear in the
        /// transaction.
        ///
        /// Use the [`ShieldedData::outputs`] method to get an iterator over the
        /// [`Output`]s in this `TransferData`.
        maybe_outputs: Vec<Output>,
    },

    /// A bundle containing at least one output, with no spends and no shared
    /// spend anchor.
    ///
    /// In Transaction::V5, if there are no spends, there must not be a shared
    /// anchor.
    JustOutputs {
        /// At least one output, in the order they appear in the transaction.
        ///
        /// Use the [`ShieldedData::outputs`] method to get an iterator over the
        /// [`Output`]s in this `TransferData`.
        outputs: AtLeastOne<Output>,
    },
}

impl<AnchorV> fmt::Display for ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut fmter = f.debug_struct(
            format!(
                "sapling::ShieldedData<{}>",
                std::any::type_name::<AnchorV>()
            )
            .as_str(),
        );

        fmter.field("spends", &self.transfers.spends().count());
        fmter.field("outputs", &self.transfers.outputs().count());
        fmter.field("value_balance", &self.value_balance);

        fmter.finish()
    }
}

impl<AnchorV> ShieldedData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
    Spend<PerSpendAnchor>: From<(Spend<AnchorV>, AnchorV::Shared)>,
{
    /// Iterate over the [`Spend`]s for this transaction, returning deduplicated
    /// [`tree::Root`]s, regardless of the underlying transaction version.
    pub fn anchors(&self) -> impl Iterator<Item = tree::Root> + '_ {
        // TODO: use TransferData::shared_anchor to improve performance for V5 transactions
        self.spends_per_anchor()
            .map(|spend| spend.per_spend_anchor)
            .unique_by(|raw| raw.0.to_bytes())
    }

    /// Iterate over the [`Spend`]s for this transaction, returning
    /// `Spend<PerSpendAnchor>` regardless of the underlying transaction version.
    ///
    /// # Correctness
    ///
    /// Do not use this function for serialization.
    pub fn spends_per_anchor(&self) -> impl Iterator<Item = Spend<PerSpendAnchor>> + '_ {
        self.transfers.spends_per_anchor()
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
        self.transfers.spends()
    }

    /// Iterate over the [`Output`]s for this transaction, in the order they
    /// appear in it.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.transfers.outputs()
    }

    /// Provide the shared anchor for this transaction, if present.
    ///
    /// The shared anchor is only present if:
    /// * there is at least one spend, and
    /// * this is a `V5` transaction.
    pub fn shared_anchor(&self) -> Option<AnchorV::Shared> {
        self.transfers.shared_anchor()
    }

    /// Collect the [`Nullifier`]s for this transaction, if it contains
    /// [`Spend`]s.
    pub fn nullifiers(&self) -> impl Iterator<Item = &Nullifier> {
        self.spends().map(|spend| &spend.nullifier)
    }

    /// Collect the cm_u's for this transaction, if it contains [`Output`]s,
    /// in the order they appear in the transaction.
    pub fn note_commitments(&self) -> impl Iterator<Item = &jubjub::Fq> {
        self.outputs().map(|output| &output.cm_u)
    }

    /// Calculate the Spend/Output binding verification key.
    ///
    /// Getting the binding signature validating key from the Spend and Output
    /// description value commitments and the balancing value implicitly checks
    /// that the balancing value is consistent with the value transferred in the
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
    /// <https://zips.z.cash/protocol/protocol.pdf#saplingbalance>
    pub fn binding_verification_key(&self) -> redjubjub::VerificationKeyBytes<Binding> {
        let cv_old: ValueCommitment = self.spends().map(|spend| spend.cv.into()).sum();
        let cv_new: ValueCommitment = self.outputs().map(|output| output.cv.into()).sum();
        let cv_balance: ValueCommitment =
            ValueCommitment::new(jubjub::Fr::zero(), self.value_balance);

        let key_bytes: [u8; 32] = (cv_old - cv_new - cv_balance).into();

        key_bytes.into()
    }
}

impl<AnchorV> TransferData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
    Spend<PerSpendAnchor>: From<(Spend<AnchorV>, AnchorV::Shared)>,
{
    /// Iterate over the [`Spend`]s for this transaction, returning
    /// `Spend<PerSpendAnchor>` regardless of the underlying transaction version.
    ///
    /// Allows generic operations over V4 and V5 transactions, including:
    /// * spend verification, and
    /// * non-malleable transaction ID generation.
    ///
    /// # Correctness
    ///
    /// Do not use this function for serialization.
    pub fn spends_per_anchor(&self) -> impl Iterator<Item = Spend<PerSpendAnchor>> + '_ {
        self.spends().cloned().map(move |spend| {
            Spend::<PerSpendAnchor>::from((
                spend,
                self.shared_anchor()
                    .expect("shared anchor must be Some if there are any spends"),
            ))
        })
    }
}

impl<AnchorV> TransferData<AnchorV>
where
    AnchorV: AnchorVariant + Clone,
{
    /// Iterate over the [`Spend`]s for this transaction, returning them as
    /// their generic type.
    pub fn spends(&self) -> impl Iterator<Item = &Spend<AnchorV>> {
        use TransferData::*;

        let spends = match self {
            SpendsAndMaybeOutputs { spends, .. } => Some(spends.iter()),
            JustOutputs { .. } => None,
        };

        // this awkward construction avoids returning a newtype struct or
        // type-erased boxed iterator
        spends.into_iter().flatten()
    }

    /// Iterate over the [`Output`]s for this transaction.
    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        use TransferData::*;

        match self {
            SpendsAndMaybeOutputs { maybe_outputs, .. } => maybe_outputs,
            JustOutputs { outputs, .. } => outputs.as_vec(),
        }
        .iter()
    }

    /// Provide the shared anchor for this transaction, if present.
    ///
    /// The shared anchor is only present if:
    /// * there is at least one spend, and
    /// * this is a `V5` transaction.
    pub fn shared_anchor(&self) -> Option<AnchorV::Shared> {
        use TransferData::*;

        match self {
            SpendsAndMaybeOutputs { shared_anchor, .. } => Some(shared_anchor.clone()),
            JustOutputs { .. } => None,
        }
    }
}

impl TrustedPreallocate for Groth16Proof {
    fn max_allocation() -> u64 {
        // Each V5 transaction proof array entry must have a corresponding
        // spend or output prefix. We use the larger limit, so we don't reject
        // any valid large blocks.
        //
        // TODO: put a separate limit on proofs in spends and outputs
        max(
            SpendPrefixInTransactionV5::max_allocation(),
            OutputPrefixInTransactionV5::max_allocation(),
        )
    }
}
