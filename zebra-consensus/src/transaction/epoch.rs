//! The heights in which the rules for verifying a transaction outside its block stay the same.

use std::ops::Bound::{Excluded, Unbounded};

use zebra_chain::{
    block::Height,
    parameters::{Network, NetworkUpgrade},
};

/// A range of heights in which every rule for verifying a transaction is the same, apart from the
/// rules in [`check_block_context_rules`](super::check_block_context_rules).
///
/// The block verifier reuses the mempool's verification of a transaction only if the mempool
/// verified it for a height in the block's epoch, and checks the block context rules again at the
/// block's height. So the rules it skips must not change inside an epoch. One of them is the
/// consensus branch ID that a version 4 transaction's signatures commit to, which the transaction
/// doesn't contain, so the block context rules can't check it.
///
/// An epoch starts at a network upgrade activation, or at the activation of the soft fork that
/// temporarily disables Orchard actions, and ends before the next of these activations. The
/// mempool clears and verifies its transactions again at the same heights.
///
/// # Correctness
///
/// A new rule that changes inside a network upgrade must be checked in
/// `check_block_context_rules`, or its activation height must start an epoch in
/// [`rule_activations_inside_upgrades`]. Otherwise the block verifier reuses verifications from
/// before the rule activated in blocks after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerificationRulesEpoch {
    /// The network upgrade at every height in the epoch.
    network_upgrade: NetworkUpgrade,
    /// The first height in the epoch.
    start: Height,
    /// The first height after the epoch, or `None` if no later activation height is known.
    end: Option<Height>,
}

impl VerificationRulesEpoch {
    /// Returns the epoch that contains `height` on `network`.
    pub(crate) fn containing(network: &Network, height: Height) -> Self {
        // `activation_list()` builds the activation map, so build it once for both lookups.
        let activations = network.activation_list();
        let (&start, &network_upgrade) = activations
            .range(..=height)
            .next_back()
            .expect("every height has a current network upgrade");
        let end = activations
            .range((Excluded(height), Unbounded))
            .next()
            .map(|(&end, _)| end);

        rule_activations_inside_upgrades(network).fold(
            Self {
                network_upgrade,
                start,
                end,
            },
            |epoch, activation| epoch.split_at(activation, height),
        )
    }

    /// Returns the part of this epoch on the same side of `activation` as `height`.
    fn split_at(self, activation: Height, height: Height) -> Self {
        if !self.contains(activation) {
            self
        } else if activation <= height {
            Self {
                start: activation,
                ..self
            }
        } else {
            Self {
                end: Some(activation),
                ..self
            }
        }
    }

    /// Returns `true` if `height` is in this epoch.
    pub(crate) fn contains(&self, height: Height) -> bool {
        self.start <= height && self.end.is_none_or(|end| height < end)
    }

    /// Returns the network upgrade at every height in this epoch.
    pub(crate) fn network_upgrade(&self) -> NetworkUpgrade {
        self.network_upgrade
    }

    /// Returns the first height in this epoch.
    #[cfg(test)]
    pub(crate) fn start(&self) -> Height {
        self.start
    }

    /// Returns the last height in this epoch, or `None` if no later activation height is known.
    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<Height> {
        self.end.map(|end| {
            end.previous().expect(
                "an epoch ends after its first height, so its end is above the minimum height",
            )
        })
    }
}

/// Returns the activation heights of the soft forks that change the rules for verifying a
/// transaction inside a network upgrade.
///
/// The rule of the soft fork that temporarily disables Orchard actions is also checked in
/// `check_block_context_rules`. Epochs still split at its activation, because the mempool clears
/// and verifies its transactions again there, so the epochs end where the mempool's
/// verifications do.
fn rule_activations_inside_upgrades(network: &Network) -> impl Iterator<Item = Height> {
    network
        .temporary_orchard_disabling_soft_fork_height()
        .into_iter()
}
