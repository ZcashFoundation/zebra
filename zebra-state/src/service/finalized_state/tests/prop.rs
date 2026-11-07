//! Randomised property tests for the finalized state.

use std::env;

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::Height,
    parameters::{
        testnet::{ConfiguredActivationHeights, ParametersBuilder},
        Network, NetworkUpgrade,
    },
    LedgerState,
};
use zebra_test::prelude::*;

use crate::{
    config::Config,
    service::{arbitrary::PreparedChain, finalized_state::FinalizedState, ChainTipBlock},
    tests::FakeChainHelper,
    CheckpointVerifiedBlock,
};

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 1;

#[test]
fn blocks_with_v5_transactions() -> Result<()> {
    let _init_guard = zebra_test::init();
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, count, network, _history_tree) in PreparedChain::default())| {
            let mut state = FinalizedState::new(&Config::ephemeral(), &network).expect("opening an ephemeral database should succeed");
            let mut height = Height(0);
            // use `count` to minimize test failures, so they are easier to diagnose
            for block in chain.iter().take(count) {
                let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone());
                let (hash, _) = state.commit_finalized_direct(
                    checkpoint_verified.into(),
                    None,
                    "blocks_with_v5_transactions test"
                ).unwrap();
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                prop_assert_eq!(hash, block.hash);
                height = Height(height.0 + 1);
            }
    });

    Ok(())
}

/// Test if committing blocks from all upgrades work correctly, to make
/// sure the contextual validation done by the finalized state works.
/// Also test if a block with the wrong commitment is correctly rejected.
#[test]
#[allow(clippy::print_stderr)]
fn all_upgrades_and_wrong_commitments_with_fake_activation_heights() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (network, initial_nsm_value_balance) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    // Use no_shrink() because we're ignoring _count and there is nothing to actually shrink.
    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink())| {

            let mut state = FinalizedState::new(&Config::ephemeral(), &network).expect("opening an ephemeral database should succeed");
            let mut height = Height(0);
            let mut note_commitment_trees = None;
            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(&network).unwrap();
            let heartwood_height_plus1 = (heartwood_height + 1).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
            let nu5_height_plus1 = (nu5_height + 1).unwrap();

            let mut failure_count = 0;
            for block in chain.iter() {
                let block_hash = block.hash;
                let current_height = block.block.coinbase_height().unwrap();
                // For some specific heights, try to commit a block with
                // corrupted commitment.
                match current_height {
                    h if h == heartwood_height ||
                        h == heartwood_height_plus1 ||
                        h == nu5_height ||
                        h == nu5_height_plus1 => {
                            let block = block.block.clone().set_block_commitment([0x42; 32]);
                            let checkpoint_verified = CheckpointVerifiedBlock::from(block)
                                .with_precomputed_auth_data_root();
                            state.commit_finalized(
                                checkpoint_verified,
                                note_commitment_trees.clone(),
                            ).expect_err("Must fail commitment check");
                            prop_assert_eq!(state.finalized_tip_height(), Some(Height(height.0 - 1)));
                            failure_count += 1;
                        },
                    _ => {},
                }
                let checkpoint_verified = CheckpointVerifiedBlock::from(block.block.clone())
                    .with_precomputed_auth_data_root();
                let expected_tip = ChainTipBlock::from(checkpoint_verified.clone());
                let (tip, trees) = state.commit_finalized(
                    checkpoint_verified,
                    note_commitment_trees.take(),
                ).unwrap();
                prop_assert_eq!(tip, expected_tip);
                prop_assert_eq!(state.db.finalized_tip_hash(), block_hash);
                prop_assert_eq!(Some(height), state.finalized_tip_height());
                note_commitment_trees = Some(trees);
                height = Height(height.0 + 1);
            }
            // Make sure the failure path was triggered
            prop_assert_eq!(failure_count, 4);

            // The finalized state tracked the NSM value balance from NU7 activation: seeded, then
            // debited by each block's additional subsidy and credited with its ZIP 235 fee
            // contribution.
            {
                use zebra_chain::{
                    parameters::subsidy::{additional_block_subsidy, nsm_fee_contribution},
                    transparent::utxos_from_ordered_utxos,
                };

                let utxos = utxos_from_ordered_utxos(
                    chain.iter().flat_map(|block| block.new_outputs.iter().map(|(outpoint, utxo)| (*outpoint, utxo.clone()))),
                );

                let nu7_height = NetworkUpgrade::Nu7.activation_height(&network).unwrap();
                let tip_height = state.finalized_tip_height().unwrap();
                prop_assert!(tip_height >= nu7_height, "the chain must reach NU7");

                let mut expected = initial_nsm_value_balance;
                for height in (nu7_height.0..=tip_height.0).map(Height) {
                    let fees = chain[height.0 as usize].block.transaction_fees(&utxos).expect("valid fees");

                    expected = (expected - additional_block_subsidy(height, &network, expected))
                        .expect("the balance never goes negative");
                    expected = (expected + nsm_fee_contribution(height, &network, fees))
                        .expect("the balance stays a valid amount");
                }

                prop_assert_eq!(state.finalized_value_pool().nsm_amount(), expected);
                prop_assert!(expected > zebra_chain::amount::Amount::<zebra_chain::amount::NonNegative>::zero());
            }
    });

    Ok(())
}

/// Returns a network with every network upgrade activated within the first
/// `MAX_PARTIAL_CHAIN_BLOCKS` blocks, and its initial NSM value balance.
fn fake_activation_heights_network() -> (Network, Amount<NonNegative>) {
    let network = ParametersBuilder::default()
        .with_slow_start_interval(zebra_chain::block::Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            // These are dummy values. The particular values don't matter much,
            // as long as the nu7 one is smaller than the chains being generated
            // (MAX_PARTIAL_CHAIN_BLOCKS) to make sure all upgrades are exercised
            // in the test below. (The test will fail if that does not happen.)
            before_overwinter: Some(1),
            overwinter: Some(10),
            sapling: Some(15),
            blossom: Some(20),
            heartwood: Some(25),
            canopy: Some(30),
            nu5: Some(35),
            nu6: Some(40),
            nu6_1: Some(45),
            nu6_2: Some(47),
            nu6_3: Some(48),
            nu7: Some(50),
        })
        .expect("failed to set activation heights")
        .with_nsm_reissuance_height(Height(50))
        // These short chains have no historical funding-stream deposits to disburse.
        .clear_funding_streams()
        .with_lockbox_disbursements(vec![]);

    // Seed the NSM value balance at NU7 activation, so that committing blocks past it reissues
    // from the balance and debits it.
    let initial_nsm_value_balance =
        zebra_chain::amount::Amount::try_from(10 * zebra_chain::amount::COIN)
            .expect("valid amount");
    let network = network.with_initial_nsm_value_balance(initial_nsm_value_balance);

    let network = network
        .to_network()
        .expect("failed to build configured network");

    (network, initial_nsm_value_balance)
}

/// Test that the finalized tip cache carried between checkpoint commits always matches the
/// database, across every network upgrade and after failed commits.
#[test]
fn checkpoint_tip_cache_matches_database() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (network, _) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink())| {
            // Skip format upgrades, so the upgrade task can't mark them as finished on its own.
            let mut state = FinalizedState::new_with_debug(&Config::ephemeral(), &network, true, false)
                .expect("opening an ephemeral database should succeed");
            let mut note_commitment_trees = None;
            let mut tip_cache = None;
            let heartwood_height = NetworkUpgrade::Heartwood.activation_height(&network).unwrap();
            let nu5_height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();

            for block in chain.iter() {
                let current_height = block.block.coinbase_height().unwrap();

                // A failed commit drops the cache, like the block write task does.
                if current_height == heartwood_height || current_height == nu5_height {
                    let block = block.block.clone().set_block_commitment([0x42; 32]);
                    state.commit_checkpoint_verified(
                        CheckpointVerifiedBlock::from(block),
                        note_commitment_trees.clone(),
                        tip_cache.take(),
                        None,
                    ).expect_err("Must fail commitment check");
                }

                let (tip, trees, new_tip_cache) = state.commit_checkpoint_verified(
                    CheckpointVerifiedBlock::from(block.block.clone()),
                    note_commitment_trees.take(),
                    tip_cache.take(),
                    None,
                ).unwrap();
                note_commitment_trees = Some(trees);
                prop_assert_eq!(tip.hash, block.hash);

                if current_height.is_min() {
                    // There's no cache while format upgrades might be running.
                    prop_assert!(new_tip_cache.is_none());
                    state.db.mark_finished_format_upgrades();
                } else {
                    let new_tip_cache = new_tip_cache.expect("format upgrades have finished");
                    prop_assert_eq!(new_tip_cache.tip, (current_height, block.hash));
                    new_tip_cache.assert_matches_db(&state.db);
                    tip_cache = Some(new_tip_cache);
                }
            }

            let tip_cache = tip_cache.expect("the chain has blocks after genesis");
            prop_assert!(tip_cache.history_tree.is_some(), "the chain must reach Heartwood");
            prop_assert!(!tip_cache.value_pool.transparent_amount().is_zero());
    });

    Ok(())
}

/// Test that checkpoint commits with precomputed note commitment trees leave the same trees in
/// the database as commits that update the trees themselves, across every network upgrade.
#[test]
fn precomputed_trees_commit_like_sequential_trees() -> Result<()> {
    let _init_guard = zebra_test::init();

    let (network, _) = fake_activation_heights_network();
    let ledger_strategy =
        LedgerState::genesis_strategy(Some(network), NetworkUpgrade::Nu5, None, false);

    proptest!(ProptestConfig::with_cases(env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES)),
        |((chain, _count, network, _history_tree) in PreparedChain::default().with_ledger_strategy(ledger_strategy).with_valid_commitments().no_shrink())| {
            let mut sequential = FinalizedState::new(&Config::ephemeral(), &network)
                .expect("opening an ephemeral database should succeed");
            let mut precomputed = FinalizedState::new(&Config::ephemeral(), &network)
                .expect("opening an ephemeral database should succeed");
            let mut sequential_trees = None;
            let mut precomputed_trees: Option<zebra_chain::parallel::tree::NoteCommitmentTrees> = None;

            for block in chain.iter() {
                let (_, trees, _) = sequential.commit_checkpoint_verified(
                    CheckpointVerifiedBlock::from(block.block.clone()),
                    sequential_trees.take(),
                    None,
                    None,
                ).unwrap();
                sequential_trees = Some(trees);

                let mut next = precomputed_trees.clone().unwrap_or_default();
                let update = next.update_trees_parallel(&block.block).map(|()| next);
                let (_, trees, _) = precomputed.commit_checkpoint_verified(
                    CheckpointVerifiedBlock::from(block.block.clone()),
                    precomputed_trees.take(),
                    None,
                    Some(update),
                ).unwrap();
                precomputed_trees = Some(trees);

                prop_assert_eq!(&precomputed_trees, &sequential_trees);
                prop_assert_eq!(
                    precomputed.db.note_commitment_trees_for_tip(),
                    sequential.db.note_commitment_trees_for_tip()
                );
            }
    });

    Ok(())
}
