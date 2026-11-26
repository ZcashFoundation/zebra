//! Tests for the verification rules epochs of transaction verification.

use std::{
    collections::BTreeSet,
    ops::Bound::{Excluded, Unbounded},
};

use proptest::prelude::*;

use zebra_chain::{
    block::Height,
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        Network, NetworkUpgrade,
    },
};

use crate::transaction::VerificationRulesEpoch;

/// Returns the networks of the epoch tests: Mainnet, the default Testnet, a Regtest that activates
/// several network upgrades at one height, and a Testnet that activates the soft fork that
/// temporarily disables Orchard actions at another height inside NU6.1.
fn networks() -> Vec<Network> {
    let testnet = Network::new_default_testnet();
    let nu6_1 = NetworkUpgrade::Nu6_1
        .activation_height(&testnet)
        .expect("the default Testnet activates NU6.1");
    let soft_fork = Height(nu6_1.0 + 10);
    assert_eq!(
        NetworkUpgrade::current(&testnet, soft_fork),
        NetworkUpgrade::Nu6_1,
        "the configured soft fork activates inside NU6.1"
    );
    assert_ne!(
        testnet.temporary_orchard_disabling_soft_fork_height(),
        Some(soft_fork)
    );
    let configured = Parameters::build()
        .with_temporary_orchard_disabling_soft_fork_height(soft_fork)
        .to_network()
        .expect("the configured Testnet is valid");

    vec![
        Network::Mainnet,
        testnet,
        Network::new_regtest(
            ConfiguredActivationHeights {
                nu5: Some(1),
                nu6: Some(1),
                ..Default::default()
            }
            .into(),
        ),
        configured,
    ]
}

/// Returns the heights at which an epoch starts on `network`: its network upgrade activations, and
/// the activation of the soft fork that temporarily disables Orchard actions.
fn epoch_starts(network: &Network) -> BTreeSet<Height> {
    network
        .activation_list()
        .into_keys()
        .chain(network.temporary_orchard_disabling_soft_fork_height())
        .collect()
}

/// Each epoch starts at the last network upgrade activation or soft fork activation at or below
/// its heights, and ends before the next one.
#[test]
fn epochs_start_at_each_rule_activation() {
    let _init_guard = zebra_test::init();

    for network in networks() {
        let starts = epoch_starts(&network);
        for &activation in &starts {
            let heights = [
                activation.previous().ok(),
                Some(activation),
                activation.next().ok(),
            ];
            for height in heights.into_iter().flatten() {
                let epoch = VerificationRulesEpoch::containing(&network, height);
                let context = format!("{network:?} at {height:?}");

                assert!(epoch.contains(height), "{context}");
                assert_eq!(
                    epoch.network_upgrade(),
                    NetworkUpgrade::current(&network, height),
                    "{context}"
                );
                assert_eq!(
                    Some(epoch.start()),
                    starts.range(..=height).next_back().copied(),
                    "{context}"
                );
                assert_eq!(
                    epoch.last(),
                    starts
                        .range((Excluded(height), Unbounded))
                        .next()
                        .map(|end| end
                            .previous()
                            .expect("a start above another height is above the minimum height")),
                    "{context}"
                );

                // The heights next to the epoch are in other epochs.
                if let Ok(before) = epoch.start().previous() {
                    assert!(!epoch.contains(before), "{context}");
                }
                if let Some(after) = epoch.last().and_then(|last| last.next().ok()) {
                    assert!(!epoch.contains(after), "{context}");
                }
            }
        }
    }
}

proptest! {
    /// Test that the heights of an epoch have the same network upgrade and the same Orchard
    /// rules, which are the rules on `Network` that depend on the height inside a network upgrade.
    ///
    /// The heights are near a height where these rules change, so that they are often on both
    /// sides of it.
    #[test]
    fn epochs_only_contain_heights_with_the_same_rules(
        network in any::<Network>(),
        change in any::<proptest::sample::Index>(),
        first_offset in -2_000..=2_000_i64,
        second_offset in -2_000..=2_000_i64,
    ) {
        let _init_guard = zebra_test::init();

        let changes: Vec<_> = epoch_starts(&network).into_iter().collect();
        let change = i64::from(change.get(&changes).0);
        let height = |offset: i64| u32::try_from(change + offset).ok().map(Height);
        let (Some(first), Some(second)) = (height(first_offset), height(second_offset)) else {
            return Ok(());
        };

        if VerificationRulesEpoch::containing(&network, first).contains(second) {
            prop_assert_eq!(
                NetworkUpgrade::current(&network, first),
                NetworkUpgrade::current(&network, second)
            );
            prop_assert_eq!(
                network.is_orchard_temporarily_disabled(first),
                network.is_orchard_temporarily_disabled(second)
            );
            prop_assert_eq!(
                network.orchard_canonical_proof_size_rule_active(first),
                network.orchard_canonical_proof_size_rule_active(second)
            );
        }
    }
}
