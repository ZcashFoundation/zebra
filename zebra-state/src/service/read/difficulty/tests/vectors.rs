//! Template timestamp bounds and Testnet minimum-difficulty intervals.

use zebra_chain::{
    block::Block,
    parameters::{
        testnet::{ConfiguredActivationHeights, Parameters},
        TESTNET_MAX_TIME_START_HEIGHT,
    },
    serialization::ZcashDeserializeInto,
};

use super::*;

/// Header-only queries must retain genesis and follow the best fork's timestamps.
#[test]
fn header_context_queries_preserve_genesis_and_fork_ancestry() {
    use crate::{
        arbitrary::Prepare,
        tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    };

    let _init_guard = zebra_test::init();
    let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
    let db = &finalized_state.db;
    let network = Network::Mainnet;
    let genesis_time = DateTime32::try_from(genesis.block.header.time).unwrap();
    let genesis_info = get_block_template_chain_info(&non_finalized_state, db, &network).unwrap();
    assert_eq!(genesis_info.tip_hash, genesis.hash);
    assert_eq!(genesis_info.tip_height, Height(0));
    assert_eq!(
        genesis_info.min_time,
        genesis_time
            .checked_add(Duration32::from_seconds(1))
            .unwrap()
    );
    assert_eq!(
        read::next_median_time_past(&non_finalized_state, db).unwrap(),
        genesis_time
    );

    let base = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();
    let base_time = base.header.time;
    non_finalized_state
        .commit_new_chain(base.clone().prepare(), db)
        .unwrap();
    let mut best = base.make_fake_child().set_work(100);
    Arc::make_mut(&mut Arc::make_mut(&mut best).header).time =
        base_time + chrono::Duration::seconds(10);
    let mut side = base.make_fake_child().set_work(50);
    Arc::make_mut(&mut Arc::make_mut(&mut side).header).time =
        base_time + chrono::Duration::seconds(30);
    let mut best_tip = best.make_fake_child();
    Arc::make_mut(&mut Arc::make_mut(&mut best_tip).header).time =
        base_time + chrono::Duration::seconds(20);
    for block in [best, best_tip.clone(), side] {
        non_finalized_state
            .commit_block(block.prepare(), db)
            .unwrap();
    }

    let expected_median = DateTime32::try_from(base_time + chrono::Duration::seconds(10)).unwrap();
    let info = get_block_template_chain_info(&non_finalized_state, db, &network).unwrap();
    assert_eq!(info.tip_hash, best_tip.hash());
    assert_eq!(info.tip_height, Height(3));
    assert_eq!(
        info.min_time,
        expected_median
            .checked_add(Duration32::from_seconds(1))
            .unwrap()
    );
    assert_eq!(
        read::next_median_time_past(&non_finalized_state, db).unwrap(),
        expected_median
    );
}

#[test]
fn nu7_template_times_match_difficulty_across_activation() {
    let _init_guard = zebra_test::init();
    const NU7_HEIGHT: u32 = 400_000;
    let network = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(NU7_HEIGHT),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .with_slow_start_interval(Height(0))
        .to_network()
        .expect("configured Testnet parameters are valid");
    assert!(!network.is_regtest());

    let limit = network.target_difficulty_limit().to_compact();
    let threshold = (network.target_difficulty_limit() / 8_u64).to_compact();
    let context: Vec<_> = (0..MAX_POW_ADJUSTMENT_BLOCK_SPAN)
        .map(|index| {
            let time = PREV - u32::try_from(index).unwrap() * 75;
            (threshold, DateTime32::from(time).into())
        })
        .collect();

    for (candidate_height, gap, standard_gap) in [
        (NU7_HEIGHT - 1, 450, 450),
        (NU7_HEIGHT - 1, 451, 450),
        (NU7_HEIGHT, 450, 450),
        (NU7_HEIGHT, 451, 450),
        (NU7_HEIGHT + 1, 450, 450),
        (NU7_HEIGHT + 1, 451, 450),
    ] {
        let parent_height = Height(candidate_height - 1);
        let difficulty_at = |time: DateTime32| {
            AdjustedDifficulty::new_from_header_time(
                time.into(),
                parent_height,
                &network,
                context.iter().cloned(),
            )
            .expected_difficulty_threshold()
        };
        let standard = difficulty_at(DateTime32::from(PREV + standard_gap));
        assert_ne!(
            standard, limit,
            "ordinary retargeting must not produce powLimit"
        );

        let mut result = chain_info(PREV + gap);
        result.tip_height = parent_height;
        result.expected_difficulty = difficulty_at(result.cur_time);
        adjust_difficulty_and_time_for_testnet(
            &mut result,
            &network,
            parent_height,
            context.clone(),
        );

        assert_eq!(result.cur_time, DateTime32::from(PREV + gap));
        if gap <= standard_gap {
            assert_eq!(result.expected_difficulty, standard);
            assert_eq!(result.max_time, DateTime32::from(PREV + standard_gap));
        } else {
            assert_eq!(result.expected_difficulty, limit);
            assert_eq!(result.min_time, DateTime32::from(PREV + standard_gap + 1));
        }

        // Difficulty is constant on each side of the time threshold: checking
        // both endpoints verifies that no advertised timestamp crosses it.
        for time in [result.min_time, result.cur_time, result.max_time] {
            assert_eq!(
                result.expected_difficulty,
                difficulty_at(time),
                "candidate height {candidate_height}, initial gap {gap}, advertised time {time:?}"
            );
        }
    }
}

/// Templates below the Testnet time gate retain the context-free two-hour window.
#[test]
fn template_max_time_respects_network_height_gate() {
    let _init_guard = zebra_test::init();
    let custom_testnet = Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(2),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .with_slow_start_interval(Height(0))
        .to_network()
        .expect("configured Testnet parameters are valid");
    let now = DateTime32::from(PREV + 24 * 60 * 60);

    for (network, candidate_height, enforced) in [
        (custom_testnet, Height(100), false),
        (
            Network::new_default_testnet(),
            (TESTNET_MAX_TIME_START_HEIGHT - 1).unwrap(),
            false,
        ),
        (
            Network::new_default_testnet(),
            TESTNET_MAX_TIME_START_HEIGHT,
            true,
        ),
        (Network::Mainnet, Height(2_000_000), true),
        (Network::new_regtest(Default::default()), Height(100), false),
    ] {
        let context = template_block_context(&network, PREV);
        let result = difficulty_time_and_history_tree(
            context,
            (candidate_height - 1).unwrap(),
            Hash([0; 32]),
            &network,
            Arc::new(HistoryTree::default()),
            #[cfg(zcash_unstable = "zip234")]
            zebra_chain::value_balance::ValueBalance::zero(),
            now,
        )
        .expect("the template has a valid timestamp range");

        if enforced {
            let median = PREV - u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;
            assert_eq!(
                result.max_time,
                DateTime32::from(median + BLOCK_MAX_TIME_SINCE_MEDIAN),
            );
        } else {
            assert_eq!(
                result.max_time,
                now.checked_add(Duration32::from_hours(2)).unwrap(),
                "candidate height {candidate_height:?}",
            );
        }
        assert!(result.min_time <= result.cur_time);
        assert!(result.cur_time <= result.max_time);
        if network.is_regtest() {
            assert_eq!(result.cur_time, result.min_time);
        }
    }
}

/// Future median times must not move the independent local-clock upper bound.
#[test]
fn template_times_respect_local_clock_bound() {
    let _init_guard = zebra_test::init();
    let now = DateTime32::from(PREV);
    let max_time = now.checked_add(Duration32::from_hours(2)).unwrap();
    let median_offset = u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;
    let mut header = *zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block fixture must deserialize")
        .header;

    for (network, candidate_height) in [
        (Network::Mainnet, Height(2_000_000)),
        (Network::new_default_testnet(), Height(100)),
        (Network::new_regtest(Default::default()), Height(100)),
    ] {
        for future_median_seconds in [60 * 60, 2 * 60 * 60 - 1, 2 * 60 * 60] {
            let context =
                template_block_context(&network, PREV + future_median_seconds + median_offset);
            let result = difficulty_time_and_history_tree(
                context,
                (candidate_height - 1).unwrap(),
                Hash([0; 32]),
                &network,
                Arc::new(HistoryTree::default()),
                #[cfg(zcash_unstable = "zip234")]
                zebra_chain::value_balance::ValueBalance::zero(),
                now,
            );

            if future_median_seconds == 2 * 60 * 60 {
                assert!(
                    result.is_err(),
                    "no timestamp is valid until the clock advances"
                );
                continue;
            }

            let result = result.expect("there is a timestamp inside the local-clock bound");
            assert_eq!(result.max_time, max_time);
            assert_eq!(
                result.min_time,
                DateTime32::from(PREV + future_median_seconds + 1),
            );
            assert!(result.min_time <= result.cur_time);
            assert!(result.cur_time <= result.max_time);

            for time in [result.min_time, result.cur_time, result.max_time] {
                header.time = time.into();
                header
                    .time_is_valid_at(now.into(), &candidate_height, &Hash([0; 32]))
                    .expect("every advertised timestamp must pass context-free time validation");
            }
        }
    }
}

/// An unrepresentable minimum-difficulty threshold leaves the entire valid range standard.
#[test]
fn template_times_near_timestamp_ceiling_stay_standard_difficulty() {
    let _init_guard = zebra_test::init();
    let network = Network::new_default_testnet();
    let median_offset = u32::try_from(POW_MEDIAN_BLOCK_SPAN / 2).unwrap() * 75;

    // Overflow the parent-plus-gap sum, then isolate overflow of its strict successor.
    for previous_time in [u32::MAX - 100, u32::MAX - GAP] {
        let context = template_block_context(&network, previous_time);
        let difficulty_at = |time: DateTime32| {
            AdjustedDifficulty::new_from_header_time(
                time.into(),
                ACTIVE_HEIGHT,
                &network,
                context.iter().cloned(),
            )
            .expected_difficulty_threshold()
        };
        let now = DateTime32::from(previous_time);
        let standard = difficulty_at(now);
        assert_ne!(standard, network.target_difficulty_limit().to_compact());

        let result = difficulty_time_and_history_tree(
            context.clone(),
            ACTIVE_HEIGHT,
            Hash([0; 32]),
            &network,
            Arc::new(HistoryTree::default()),
            #[cfg(zcash_unstable = "zip234")]
            zebra_chain::value_balance::ValueBalance::zero(),
            now,
        )
        .expect("standard-difficulty timestamps remain representable");

        assert_eq!(
            result.min_time,
            DateTime32::from(previous_time - median_offset + 1),
        );
        assert_eq!(result.cur_time, now);
        assert_eq!(result.max_time, DateTime32::from(u32::MAX));
        assert_eq!(result.expected_difficulty, standard);

        for time in [result.min_time, result.cur_time, result.max_time] {
            assert_eq!(
                difficulty_at(time),
                result.expected_difficulty,
                "parent time {previous_time}, advertised time {time:?}",
            );
        }
    }
}

/// Header difficulties and times for template calculation, newest first.
fn template_block_context(
    network: &Network,
    previous_time: u32,
) -> Vec<(CompactDifficulty, DateTime<Utc>)> {
    let threshold = (network.target_difficulty_limit() / 8_u64).to_compact();
    (0..MAX_POW_ADJUSTMENT_BLOCK_SPAN)
        .map(|index| {
            let time = DateTime32::from(previous_time - u32::try_from(index).unwrap() * 75);
            (threshold, time.into())
        })
        .collect()
}
