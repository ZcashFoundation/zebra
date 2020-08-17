use futures::future::Either;

use crate::{
    primitives::redjubjub::{Binding, Signature},
    sapling::{Output, Spend},
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
