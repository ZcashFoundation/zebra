//! Consensus parameter tests for Zebra.

use std::collections::HashSet;

use crate::block;

use super::*;

use Network::*;
use NetworkUpgrade::*;

/// Check that the activation heights and network upgrades are unique.
#[test]
fn activation_bijective() {
    zebra_test::init();

    if std::env::var_os("TEST_FAKE_ACTIVATION_HEIGHTS").is_some() {
        eprintln!("Skipping activation_bijective() since $TEST_FAKE_ACTIVATION_HEIGHTS is set");
        return;
    }

    let mainnet_activations = NetworkUpgrade::activation_list(Mainnet);
    let mainnet_heights: HashSet<&block::Height> = mainnet_activations.keys().collect();
    assert_eq!(MAINNET_ACTIVATION_HEIGHTS.len(), mainnet_heights.len());

    let mainnet_nus: HashSet<&NetworkUpgrade> = mainnet_activations.values().collect();
    assert_eq!(MAINNET_ACTIVATION_HEIGHTS.len(), mainnet_nus.len());

    let testnet_activations = NetworkUpgrade::activation_list(Testnet);
    let testnet_heights: HashSet<&block::Height> = testnet_activations.keys().collect();
    assert_eq!(TESTNET_ACTIVATION_HEIGHTS.len(), testnet_heights.len());

    let testnet_nus: HashSet<&NetworkUpgrade> = testnet_activations.values().collect();
    assert_eq!(TESTNET_ACTIVATION_HEIGHTS.len(), testnet_nus.len());
}

#[test]
fn activation_extremes_mainnet() {
    zebra_test::init();
    activation_extremes(Mainnet)
}

#[test]
fn activation_extremes_testnet() {
    zebra_test::init();
    activation_extremes(Testnet)
}

/// Test the activation_list, activation_height, current, and next functions
/// for `network` with extreme values.
fn activation_extremes(network: Network) {
    // The first three upgrades are Genesis, BeforeOverwinter, and Overwinter
    assert_eq!(
        NetworkUpgrade::activation_list(network).get(&block::Height(0)),
        Some(&Genesis)
    );
    assert_eq!(Genesis.activation_height(network), Some(block::Height(0)));
    assert!(NetworkUpgrade::is_activation_height(
        network,
        block::Height(0)
    ));

    assert_eq!(NetworkUpgrade::current(network, block::Height(0)), Genesis);
    assert_eq!(
        NetworkUpgrade::next(network, block::Height(0)),
        Some(BeforeOverwinter)
    );

    assert_eq!(
        NetworkUpgrade::activation_list(network).get(&block::Height(1)),
        Some(&BeforeOverwinter)
    );
    assert_eq!(
        BeforeOverwinter.activation_height(network),
        Some(block::Height(1))
    );
    assert!(NetworkUpgrade::is_activation_height(
        network,
        block::Height(1)
    ));

    assert_eq!(
        NetworkUpgrade::current(network, block::Height(1)),
        BeforeOverwinter
    );
    assert_eq!(
        NetworkUpgrade::next(network, block::Height(1)),
        Some(Overwinter)
    );

    assert!(!NetworkUpgrade::is_activation_height(
        network,
        block::Height(2)
    ));

    // We assume that the last upgrade we know about continues forever
    // (even if we suspect that won't be true)
    assert_ne!(
        NetworkUpgrade::activation_list(network).get(&block::Height::MAX),
        Some(&Genesis)
    );
    assert!(!NetworkUpgrade::is_activation_height(
        network,
        block::Height::MAX
    ));

    assert_ne!(
        NetworkUpgrade::current(network, block::Height::MAX),
        Genesis
    );
    assert_eq!(NetworkUpgrade::next(network, block::Height::MAX), None);
}

#[test]
fn activation_consistent_mainnet() {
    zebra_test::init();
    activation_consistent(Mainnet)
}

#[test]
fn activation_consistent_testnet() {
    zebra_test::init();
    activation_consistent(Testnet)
}

/// Check that the `activation_height`, `is_activation_height`,
/// `current`, and `next` functions are consistent for `network`.
fn activation_consistent(network: Network) {
    let activation_list = NetworkUpgrade::activation_list(network);
    let network_upgrades: HashSet<&NetworkUpgrade> = activation_list.values().collect();

    for &network_upgrade in network_upgrades {
        let height = network_upgrade
            .activation_height(network)
            .expect("activations must have a height");
        assert!(NetworkUpgrade::is_activation_height(network, height));

        if height > block::Height(0) {
            // Genesis is immediately followed by BeforeOverwinter,
            // but the other network upgrades have multiple blocks between them
            assert!(!NetworkUpgrade::is_activation_height(
                network,
                (height + 1).unwrap()
            ));
        }

        assert_eq!(NetworkUpgrade::current(network, height), network_upgrade);
        // Network upgrades don't repeat
        assert_ne!(NetworkUpgrade::next(network, height), Some(network_upgrade));
        assert_ne!(
            NetworkUpgrade::next(network, block::Height(height.0 + 1)),
            Some(network_upgrade)
        );
        assert_ne!(
            NetworkUpgrade::next(network, block::Height::MAX),
            Some(network_upgrade)
        );
    }
}

/// Check that the network upgrades and branch ids are unique.
#[test]
fn branch_id_bijective() {
    zebra_test::init();

    let branch_id_list = NetworkUpgrade::branch_id_list();
    let nus: HashSet<&NetworkUpgrade> = branch_id_list.keys().collect();
    assert_eq!(CONSENSUS_BRANCH_IDS.len(), nus.len());

    let branch_ids: HashSet<&ConsensusBranchId> = branch_id_list.values().collect();
    assert_eq!(CONSENSUS_BRANCH_IDS.len(), branch_ids.len());
}

#[test]
fn branch_id_extremes_mainnet() {
    zebra_test::init();
    branch_id_extremes(Mainnet)
}

#[test]
fn branch_id_extremes_testnet() {
    zebra_test::init();
    branch_id_extremes(Testnet)
}

/// Test the branch_id_list, branch_id, and current functions for `network` with
/// extreme values.
fn branch_id_extremes(network: Network) {
    // Branch ids were introduced in Overwinter
    assert_eq!(
        NetworkUpgrade::branch_id_list().get(&BeforeOverwinter),
        None
    );
    assert_eq!(ConsensusBranchId::current(network, block::Height(0)), None);
    assert_eq!(
        NetworkUpgrade::branch_id_list().get(&Overwinter).cloned(),
        Overwinter.branch_id()
    );

    // We assume that the last upgrade we know about continues forever
    // (even if we suspect that won't be true)
    assert_ne!(
        NetworkUpgrade::branch_id_list().get(&NetworkUpgrade::current(network, block::Height::MAX)),
        None
    );
    assert_ne!(
        ConsensusBranchId::current(network, block::Height::MAX),
        None
    );
}

#[test]
fn branch_id_consistent_mainnet() {
    zebra_test::init();
    branch_id_consistent(Mainnet)
}

#[test]
fn branch_id_consistent_testnet() {
    zebra_test::init();
    branch_id_consistent(Testnet)
}

/// Check that the branch_id and current functions are consistent for `network`.
fn branch_id_consistent(network: Network) {
    let branch_id_list = NetworkUpgrade::branch_id_list();
    let network_upgrades: HashSet<&NetworkUpgrade> = branch_id_list.keys().collect();

    for &network_upgrade in network_upgrades {
        let height = network_upgrade.activation_height(network);

        // Skip network upgrades that don't have activation heights yet
        if let Some(height) = height {
            assert_eq!(
                ConsensusBranchId::current(network, height),
                network_upgrade.branch_id()
            );
        }
    }
}
