use futures::future::Either;

use crate::{
    amount::Amount,
    primitives::redjubjub::{Binding, Signature},
    sapling::{Nullifier, Output, Spend, ValueCommitment},
    serialization::serde_helpers,
};

/// A bundle of [`Spend`] and [`Output`] descriptions and signature data.
///
/// Spend and Output descriptions are optional, but Zcash transactions must
/// include a binding signature if and only if there is at least one Spend *or*
/// Output description. This wrapper type bundles at least one Spend or Output
/// description with the required signature data, so that an
/// `Option<ShieldedData>` correctly models the presence or absence of any
/// shielded data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShieldedData {
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
    pub first: Either<Spend, Output>,
    /// The rest of the [`Spend`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::spends`] method provides an iterator
    /// over all spend descriptions.
    pub rest_spends: Vec<Spend>,
    /// The rest of the [`Output`]s for this transaction.
    ///
    /// Note that the [`ShieldedData::outputs`] method provides an iterator
    /// over all output descriptions.
    pub rest_outputs: Vec<Output>,
    /// A signature on the transaction hash.
    pub binding_sig: Signature<Binding>,
}

impl ShieldedData {
    /// Iterate over the [`Spend`]s for this transaction.
    pub fn spends(&self) -> impl Iterator<Item = &Spend> {
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
    pub fn nullifiers(&self) -> Vec<Nullifier> {
        self.spends().map(|spend| spend.nullifier).collect()
    }

    /// Collect the cm_u's for this transaction, if it contains [`Output`]s.
    pub fn note_commitments(&self) -> Vec<jubjub::Fq> {
        self.outputs().map(|output| output.cm_u).collect()
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
    /// https://zips.z.cash/protocol/canopy.pdf#saplingbalance
    pub fn binding_verification_key(
        &self,
        value_balance: Amount,
    ) -> redjubjub::VerificationKeyBytes<Binding> {
        let cv_old: ValueCommitment = self.spends().map(|spend| spend.cv).sum();
        let cv_new: ValueCommitment = self.outputs().map(|output| output.cv).sum();
        let cv_balance: ValueCommitment = ValueCommitment::new(jubjub::Fr::zero(), value_balance);

        let key_bytes: [u8; 32] = (cv_old - cv_new - cv_balance).into();

        key_bytes.into()
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
