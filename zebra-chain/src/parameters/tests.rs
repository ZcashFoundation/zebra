//! Consensus parameter tests for Zebra.

#![allow(clippy::unwrap_in_result)]

use std::collections::HashSet;

use crate::block;

use super::*;

use Network::*;
use NetworkUpgrade::*;

/// Check that the activation heights and network upgrades are unique.
#[test]
fn activation_bijective() {
    let _init_guard = zebra_test::init();

    let mainnet_activations = Mainnet.activation_list();
    let mainnet_heights: HashSet<&block::Height> = mainnet_activations.keys().collect();
    assert_eq!(MAINNET_ACTIVATION_HEIGHTS.len(), mainnet_heights.len());

    let mainnet_nus: HashSet<&NetworkUpgrade> = mainnet_activations.values().collect();
    assert_eq!(MAINNET_ACTIVATION_HEIGHTS.len(), mainnet_nus.len());

    let testnet_activations = Network::new_default_testnet().activation_list();
    let testnet_heights: HashSet<&block::Height> = testnet_activations.keys().collect();
    assert_eq!(TESTNET_ACTIVATION_HEIGHTS.len(), testnet_heights.len());

    let testnet_nus: HashSet<&NetworkUpgrade> = testnet_activations.values().collect();
    assert_eq!(TESTNET_ACTIVATION_HEIGHTS.len(), testnet_nus.len());
}

#[test]
fn activation_extremes_mainnet() {
    let _init_guard = zebra_test::init();
    activation_extremes(Mainnet)
}

#[test]
fn activation_extremes_testnet() {
    let _init_guard = zebra_test::init();
    activation_extremes(Network::new_default_testnet())
}

/// Test the activation_list, activation_height, current, and next functions
/// for `network` with extreme values.
fn activation_extremes(network: Network) {
    // The first three upgrades are Genesis, BeforeOverwinter, and Overwinter
    assert_eq!(
        network.activation_list().get(&block::Height(0)),
        Some(&Genesis)
    );
    assert_eq!(Genesis.activation_height(&network), Some(block::Height(0)));
    assert!(NetworkUpgrade::is_activation_height(
        &network,
        block::Height(0)
    ));

    assert_eq!(NetworkUpgrade::current(&network, block::Height(0)), Genesis);
    assert_eq!(
        NetworkUpgrade::next(&network, block::Height(0)),
        Some(BeforeOverwinter)
    );

    assert_eq!(
        network.activation_list().get(&block::Height(1)),
        Some(&BeforeOverwinter)
    );
    assert_eq!(
        BeforeOverwinter.activation_height(&network),
        Some(block::Height(1))
    );
    assert!(NetworkUpgrade::is_activation_height(
        &network,
        block::Height(1)
    ));

    assert_eq!(
        NetworkUpgrade::current(&network, block::Height(1)),
        BeforeOverwinter
    );
    assert_eq!(
        NetworkUpgrade::next(&network, block::Height(1)),
        Some(Overwinter)
    );

    assert!(!NetworkUpgrade::is_activation_height(
        &network,
        block::Height(2)
    ));

    // We assume that the last upgrade we know about continues forever
    // (even if we suspect that won't be true)
    assert_ne!(
        network.activation_list().get(&block::Height::MAX),
        Some(&Genesis)
    );
    assert!(!NetworkUpgrade::is_activation_height(
        &network,
        block::Height::MAX
    ));

    assert_ne!(
        NetworkUpgrade::current(&network, block::Height::MAX),
        Genesis
    );
    assert_eq!(NetworkUpgrade::next(&network, block::Height::MAX), None);
}

#[test]
fn activation_consistent_mainnet() {
    let _init_guard = zebra_test::init();
    activation_consistent(Mainnet)
}

#[test]
fn activation_consistent_testnet() {
    let _init_guard = zebra_test::init();
    activation_consistent(Network::new_default_testnet())
}

/// Check that the `activation_height`, `is_activation_height`,
/// `current`, and `next` functions are consistent for `network`.
fn activation_consistent(network: Network) {
    let activation_list = network.activation_list();
    let network_upgrades: HashSet<&NetworkUpgrade> = activation_list.values().collect();

    for &network_upgrade in network_upgrades {
        let height = network_upgrade
            .activation_height(&network)
            .expect("activations must have a height");
        assert!(NetworkUpgrade::is_activation_height(&network, height));

        if height > block::Height(0) {
            // Genesis is immediately followed by BeforeOverwinter,
            // but the other network upgrades have multiple blocks between them
            assert!(!NetworkUpgrade::is_activation_height(
                &network,
                (height + 1).unwrap()
            ));
        }

        assert_eq!(NetworkUpgrade::current(&network, height), network_upgrade);
        // Network upgrades don't repeat
        assert_ne!(
            NetworkUpgrade::next(&network, height),
            Some(network_upgrade)
        );
        assert_ne!(
            NetworkUpgrade::next(&network, block::Height(height.0 + 1)),
            Some(network_upgrade)
        );
        assert_ne!(
            NetworkUpgrade::next(&network, block::Height::MAX),
            Some(network_upgrade)
        );
    }
}

/// Check that the network upgrades and branch ids are unique.
#[test]
fn branch_id_bijective() {
    let _init_guard = zebra_test::init();

    let branch_id_list = NetworkUpgrade::branch_id_list();
    let nus: HashSet<&NetworkUpgrade> = branch_id_list.keys().collect();
    assert_eq!(CONSENSUS_BRANCH_IDS.len(), nus.len());

    let branch_ids: HashSet<&ConsensusBranchId> = branch_id_list.values().collect();
    assert_eq!(CONSENSUS_BRANCH_IDS.len(), branch_ids.len());
}

/// NU7's deployment ID must agree with the transaction parser and signing implementation.
#[test]
fn nu7_branch_id_and_transaction_formats() {
    use std::sync::Arc;

    use crate::{
        serialization::{ZcashDeserializeInto, ZcashSerialize},
        transaction::{HashType, Transaction},
    };
    use zcash_protocol::consensus::BranchId;

    let branch_id = Nu7.branch_id().expect("NU7 has an assigned branch ID");
    assert_eq!(u32::from(branch_id), 0x7719_0ad9);
    assert_eq!(BranchId::try_from(branch_id).unwrap(), BranchId::Nu7);
    assert_eq!(
        NetworkUpgrade::try_from(u32::from(BranchId::Nu7)).unwrap(),
        Nu7
    );
    assert_eq!(
        NetworkUpgrade::from(zcash_protocol::consensus::NetworkUpgrade::Nu7),
        Nu7
    );
    for obsolete_id in [0x7719_0ad8, 0xffff_fffe, 0xffff_ffff] {
        assert!(NetworkUpgrade::try_from(obsolete_id).is_err());
        assert!(BranchId::try_from(obsolete_id).is_err());
    }
    assert_eq!(Nu7.activation_height(&Mainnet), None);

    // Empty codec/digest fixtures from librustzcash PR 3047 at 517047de130e.
    // These are not spendable transactions. They pin the NU7 header and both
    // supported formats without rewriting bytes or substituting another branch.
    for (version, encoded, digest) in [
        (
            5,
            "050000800a27a726d90a197700000000010000000000000000",
            "328d975581fbf002206ef6f0b69fe57fb47f40ec31c59ac62c3565d4e259e128",
        ),
        (
            6,
            "0600008098b684d8d90a19770000000001000000000000000000",
            "78296c68a370c2e1f058d997c81c7b011c4562c52fc5ca2994ad59cd179fbe9e",
        ),
    ] {
        let bytes = hex::decode(encoded).unwrap();
        let expected_digest = <[u8; 32]>::from_hex(digest).unwrap();
        let transaction: Transaction = bytes.as_slice().zcash_deserialize_into().unwrap();
        assert_eq!(transaction.version(), version);
        assert_eq!(transaction.network_upgrade(), Some(Nu7));
        assert_eq!(transaction.zcash_serialize_to_vec().unwrap(), bytes);
        assert_eq!(transaction.hash().0, expected_digest);
        assert_eq!(
            transaction
                .sighash(Nu7, HashType::ALL, Arc::new(Vec::new()), None)
                .unwrap()
                .0,
            expected_digest,
        );
        assert!(matches!(
            transaction.sighash(Nu6_3, HashType::ALL, Arc::new(Vec::new()), None),
            Err(crate::Error::InvalidConsensusBranchId)
        ));
    }
}

/// Public Testnet changes consensus and SDK transaction signing together at ZIP 259's height.
#[test]
fn public_testnet_nu7_activation() -> Result<(), color_eyre::Report> {
    use chrono::Duration;
    use zcash_protocol::consensus::BranchId;

    use crate::{
        amount::Amount,
        parameters::subsidy::{
            additional_block_subsidy, block_subsidy, funding_stream_values, height_for_halving,
            nsm_value_balance_change, scheduled_block_subsidy, FundingStreamReceiver, SubsidyError,
        },
        value_balance::ValueBalance,
    };

    let activation = block::Height(4_465_026);
    let third_halving = block::Height(4_497_948);
    let reissuance = block::Height(7_305_222);

    // The default constructor and configured public-parameter path must derive the same rules.
    for network in [
        Network::new_default_testnet(),
        testnet::Parameters::build().to_network()?,
    ] {
        for (height, upgrade, branch, spacing, window) in [
            (activation.previous()?, Nu6_3, BranchId::Nu6_3, 75, 17),
            (activation, Nu7, BranchId::Nu7, 25, 102),
        ] {
            assert_eq!(NetworkUpgrade::current(&network, height), upgrade);
            assert_eq!(
                NetworkUpgrade::target_spacing_for_height(&network, height),
                Duration::seconds(spacing),
            );
            assert_eq!(
                NetworkUpgrade::averaging_window_for_height(&network, height),
                window,
            );
            assert_eq!(
                BranchId::try_from(ConsensusBranchId::current(&network, height).unwrap())?,
                branch,
            );
            // Use Zebra's Parameters adapter, not the SDK's independent activation table.
            assert_eq!(BranchId::for_height(&network, height.0.into()), branch);
            assert_eq!(
                NetworkUpgrade::minimum_difficulty_spacing_for_height(&network, height),
                Some(Duration::seconds(450)),
            );
            assert_eq!(
                nsm_value_balance_change(height, &network, ValueBalance::zero(), Amount::zero())?
                    .zatoshis(),
                if height == activation {
                    55_768_414_957
                } else {
                    0
                },
            );
        }

        assert_eq!(height_for_halving(3, &network), Some(third_halving));
        assert_eq!(network.nsm_reissuance_height(), Some(reissuance));
        // The old funding cutoff no longer ends payments; the adjusted third halving does.
        for height in [block::Height(4_476_000), third_halving.previous()?] {
            let subsidy = scheduled_block_subsidy(height, &network)?;
            assert_eq!(subsidy.zatoshis(), 52_083_333);
            assert_eq!(
                funding_stream_values(height, &network, subsidy)?
                    [&FundingStreamReceiver::MajorGrants]
                    .zatoshis(),
                4_166_666,
            );
        }
        let subsidy = scheduled_block_subsidy(third_halving, &network)?;
        assert_eq!(subsidy.zatoshis(), 26_041_666);
        assert!(funding_stream_values(third_halving, &network, subsidy)?.is_empty());

        let reserve = network.initial_nsm_value_balance();
        assert!(additional_block_subsidy(reissuance.previous()?, &network, reserve).is_zero());
        assert!(!additional_block_subsidy(reissuance, &network, reserve).is_zero());
        assert_eq!(
            block_subsidy(reissuance.previous()?, &network)?,
            scheduled_block_subsidy(reissuance.previous()?, &network)?,
        );
        assert!(matches!(
            block_subsidy(reissuance, &network),
            Err(SubsidyError::ParentChainValuePoolsRequired(height)) if height == reissuance
        ));
    }

    assert_eq!(Nu7.activation_height(&Mainnet), None);
    assert_eq!(Mainnet.nsm_reissuance_height(), None);
    Ok(())
}

#[test]
fn branch_id_extremes_mainnet() {
    let _init_guard = zebra_test::init();
    branch_id_extremes(Mainnet)
}

#[test]
fn branch_id_extremes_testnet() {
    let _init_guard = zebra_test::init();
    branch_id_extremes(Network::new_default_testnet())
}

/// Test the branch_id_list, branch_id, and current functions for `network` with
/// extreme values.
fn branch_id_extremes(network: Network) {
    // Branch ids were introduced in Overwinter
    assert_eq!(
        NetworkUpgrade::branch_id_list().get(&BeforeOverwinter),
        None
    );
    assert_eq!(ConsensusBranchId::current(&network, block::Height(0)), None);
    assert_eq!(
        NetworkUpgrade::branch_id_list().get(&Overwinter).cloned(),
        Overwinter.branch_id()
    );

    // We assume that the last upgrade we know about continues forever
    // (even if we suspect that won't be true)
    assert_ne!(
        NetworkUpgrade::branch_id_list()
            .get(&NetworkUpgrade::current(&network, block::Height::MAX)),
        None
    );
    assert_ne!(
        ConsensusBranchId::current(&network, block::Height::MAX),
        None
    );
}

#[test]
fn branch_id_consistent_mainnet() {
    let _init_guard = zebra_test::init();
    branch_id_consistent(Mainnet)
}

#[test]
fn branch_id_consistent_testnet() {
    let _init_guard = zebra_test::init();
    branch_id_consistent(Network::new_default_testnet())
}

/// Check that the branch_id and current functions are consistent for `network`.
fn branch_id_consistent(network: Network) {
    let branch_id_list = NetworkUpgrade::branch_id_list();
    let network_upgrades: HashSet<&NetworkUpgrade> = branch_id_list.keys().collect();

    for &network_upgrade in network_upgrades {
        let height = network_upgrade.activation_height(&network);

        // Skip network upgrades that don't have activation heights yet
        if let Some(height) = height {
            assert_eq!(
                ConsensusBranchId::current(&network, height),
                network_upgrade.branch_id()
            );
        }
    }
}

// TODO: split this file in unit.rs and prop.rs
use hex::{FromHex, ToHex};
use proptest::prelude::*;

proptest! {
    #[test]
    fn branch_id_hex_roundtrip(nu in any::<NetworkUpgrade>()) {
        let _init_guard = zebra_test::init();

        if let Some(branch) = nu.branch_id() {
            let hex_branch: String = branch.encode_hex();
            let new_branch = ConsensusBranchId::from_hex(hex_branch.clone()).expect("hex branch_id should parse");
            prop_assert_eq!(branch, new_branch);
            prop_assert_eq!(hex_branch, new_branch.to_string());
        }
    }
}

/// A list of network upgrades in the order that they must be activated.
const NETWORK_UPGRADES_IN_ORDER: &[NetworkUpgrade] = &[
    Genesis,
    BeforeOverwinter,
    Overwinter,
    Sapling,
    Blossom,
    Heartwood,
    Canopy,
    Nu5,
    Nu6,
    Nu6_1,
    Nu6_2,
    #[cfg(any(test, feature = "zebra-test"))]
    Nu6_3,
    #[cfg(any(test, feature = "zebra-test"))]
    Nu7,
];

#[test]
fn network_upgrade_iter_matches_order_constant() {
    let iter_upgrades: Vec<NetworkUpgrade> = NetworkUpgrade::iter().collect();
    let expected_upgrades: Vec<NetworkUpgrade> = NETWORK_UPGRADES_IN_ORDER.to_vec();

    assert_eq!(iter_upgrades, expected_upgrades);
}

#[test]
fn full_activation_list_contains_all_upgrades() {
    let network = Network::Mainnet;
    let full_list = network.full_activation_list();

    // NU7 is unscheduled on Mainnet (no activation height committed), so it is absent from the
    // full activation list even though it is always present in the iter.
    assert_eq!(full_list.len(), NetworkUpgrade::iter().count() - 1);
}

/// Minimum difficulty keeps its historical start gate and strict timestamp boundary through NU7.
#[test]
fn minimum_difficulty_spacing_boundaries() {
    use chrono::{DateTime, Duration};

    use super::testnet::{ConfiguredActivationHeights, Parameters};

    let _init_guard = zebra_test::init();
    let testnet = Parameters::build()
        .with_slow_start_interval(block::Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(300_000),
            nu7: Some(400_000),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");
    let previous_time = DateTime::from_timestamp(1_600_000_000, 0).unwrap();

    for (network, height, threshold) in [
        (&Mainnet, block::Height::MAX, None),
        (&testnet, block::Height(299_187), None),
        (&testnet, block::Height(299_188), Some(900)),
        (&testnet, block::Height(299_999), Some(900)),
        (&testnet, block::Height(300_000), Some(450)),
        (&testnet, block::Height(399_999), Some(450)),
        (&testnet, block::Height(400_000), Some(450)),
        (&testnet, block::Height(400_001), Some(450)),
    ] {
        assert_eq!(
            NetworkUpgrade::minimum_difficulty_spacing_for_height(network, height),
            threshold.map(Duration::seconds),
            "{network:?}, candidate height {height:?}",
        );
        for gap in [threshold.unwrap_or(900), threshold.unwrap_or(900) + 1] {
            assert_eq!(
                NetworkUpgrade::is_testnet_min_difficulty_block(
                    network,
                    height,
                    previous_time + Duration::seconds(gap),
                    previous_time,
                ),
                threshold.is_some_and(|threshold| gap > threshold),
                "{network:?}, candidate height {height:?}, gap {gap}",
            );
        }
    }
}

/// The closed-form duration between heights matches a per-block sum across spacing changes.
#[test]
fn duration_between_heights_matches_per_block_sum() {
    use chrono::Duration;

    use super::testnet::{ConfiguredActivationHeights, Parameters};

    let _init_guard = zebra_test::init();
    let testnet = Parameters::build()
        .with_slow_start_interval(block::Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(300_000),
            nu7: Some(400_000),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid");

    let per_block_sum = |low: u32, high: u32| {
        (low + 1..=high)
            .map(|h| NetworkUpgrade::target_spacing_for_height(&testnet, block::Height(h)))
            .fold(Duration::zero(), |sum, spacing| sum + spacing)
    };
    let between = |low, high| {
        NetworkUpgrade::duration_between_heights(&testnet, block::Height(low), block::Height(high))
    };

    // Pre-Blossom, across Blossom, across NU7, and across both.
    for (low, high) in [
        (100_000, 100_010),
        (299_990, 300_010),
        (399_990, 400_010),
        (299_990, 400_010),
    ] {
        assert_eq!(
            between(low, high),
            per_block_sum(low, high),
            "{low}..={high}"
        );
    }
    assert_eq!(between(400_010, 299_990), -per_block_sum(299_990, 400_010));
    assert_eq!(between(400_000, 400_000), Duration::zero());

    // Mainnet has no NU7 height, so 84 days is exactly 84 * 1152 post-Blossom blocks.
    let release = block::Height(3_444_000);
    assert_eq!(
        NetworkUpgrade::duration_between_heights(
            &Mainnet,
            release,
            block::Height(release.0 + 84 * 1152)
        ),
        Duration::days(84)
    );
}
