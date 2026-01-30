//! Tachyon transaction bundles.

use std::vec::Vec;

use crate::Action;

/// A bundle of Tachyon [`Action`] descriptions and authorization data.
///
/// Unlike Orchard bundles which contain individual proofs per action,
/// Tachyon bundles contribute to a block-level aggregated Ragu proof.
///
/// This is a stub type for future implementation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle<A, V> {
    /// The list of actions in this bundle.
    actions: Vec<Action<A>>,
    /// The net value balance.
    value_balance: V,
    /// The authorization for this bundle.
    authorization: A,
}

impl<A, V> Bundle<A, V> {
    /// Returns the actions in this bundle.
    pub fn actions(&self) -> &[Action<A>] {
        &self.actions
    }

    /// Returns the value balance of this bundle.
    pub fn value_balance(&self) -> &V {
        &self.value_balance
    }

    /// Returns the authorization data for this bundle.
    pub fn authorization(&self) -> &A {
        &self.authorization
    }
}
