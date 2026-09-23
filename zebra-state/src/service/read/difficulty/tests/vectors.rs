//! Testnet template timestamp intervals at the NU7 activation boundary.

use zebra_chain::parameters::testnet::{ConfiguredActivationHeights, Parameters};

use super::*;

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
        (NU7_HEIGHT - 1, 150, 450),
        (NU7_HEIGHT - 1, 151, 450),
        (NU7_HEIGHT - 1, 450, 450),
        (NU7_HEIGHT - 1, 451, 450),
        (NU7_HEIGHT, 150, 150),
        (NU7_HEIGHT, 151, 150),
        (NU7_HEIGHT + 1, 150, 150),
        (NU7_HEIGHT + 1, 151, 150),
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
