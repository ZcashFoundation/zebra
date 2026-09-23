//! Regression tests for difficulty averaging across NU7.

use zebra_chain::parameters::testnet::{ConfiguredActivationHeights, Parameters};

use super::*;

const NU7_HEIGHT: u32 = 400_000;
const PREVIOUS_TIME: i64 = 1_600_000_000;

fn testnet(nu7_height: u32) -> Network {
    Parameters::build()
        .with_activation_heights(ConfiguredActivationHeights {
            blossom: Some(1),
            nu7: Some(nu7_height),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .with_funding_streams(Vec::new())
        .to_network()
        .expect("configured Testnet parameters are valid")
}

fn recent_block_data(
    threshold: CompactDifficulty,
    spacing: i64,
) -> impl Iterator<Item = (CompactDifficulty, DateTime<Utc>)> {
    (0..MAX_POW_ADJUSTMENT_BLOCK_SPAN).map(move |index| {
        let seconds = PREVIOUS_TIME - i64::try_from(index).unwrap() * spacing;
        (threshold, DateTime::from_timestamp(seconds, 0).unwrap())
    })
}

#[test]
fn nu7_testnet_pow_limit_mean_does_not_overflow() {
    let _init_guard = zebra_test::init();
    let network = testnet(NU7_HEIGHT);
    let limit = network.target_difficulty_limit();
    assert!(!network.is_regtest());
    assert_eq!(U256::from(limit), U256::from(0x07ffff_u32) << 232);

    for candidate_height in [NU7_HEIGHT - 1, NU7_HEIGHT] {
        // All 113 ancestors use Testnet's real powLimit. The 451-second gaps
        // permit those easy targets before NU7, but the candidate's 25-second
        // gap must exercise ordinary retargeting rather than minimum difficulty.
        let adjustment = AdjustedDifficulty::new_from_header_time(
            DateTime::from_timestamp(PREVIOUS_TIME + 25, 0).unwrap(),
            block::Height(candidate_height - 1),
            &network,
            recent_block_data(limit.to_compact(), 451),
        );

        assert_eq!(adjustment.mean_target_difficulty(), limit);
        assert_eq!(
            adjustment.expected_difficulty_threshold(),
            limit.to_compact()
        );
        assert_eq!(
            adjustment.median_time_past(),
            DateTime::from_timestamp(PREVIOUS_TIME - 5 * 451, 0).unwrap()
        );
        let window = if candidate_height < NU7_HEIGHT {
            17
        } else {
            102
        };
        assert_eq!(
            adjustment.median_timespan(),
            Duration::seconds(window * 451)
        );
        let bounded_timespan = if candidate_height < NU7_HEIGHT {
            1_683
        } else {
            3_366
        };
        assert_eq!(
            adjustment.median_timespan_bounded(),
            Duration::seconds(bounded_timespan)
        );
    }
}

#[test]
fn nu7_testnet_mean_preserves_nondivisible_remainders() {
    let _init_guard = zebra_test::init();
    let network = testnet(NU7_HEIGHT);
    let limit = network.target_difficulty_limit();
    let lower = (limit / 2_u64).to_compact();
    let mut context: Vec<_> = recent_block_data(limit.to_compact(), 25).collect();
    context[101].0 = lower;

    let adjustment = AdjustedDifficulty::new_from_header_time(
        DateTime::from_timestamp(PREVIOUS_TIME + 25, 0).unwrap(),
        block::Height(NU7_HEIGHT - 1),
        &network,
        context,
    );

    // floor((101 * limit + lower) / 102) = limit - ceil((limit - lower) / 102).
    // This independent expression never constructs the overflowing target sum.
    let difference = U256::from(limit) - U256::from(lower.to_expanded().unwrap());
    let window = U256::from(102_u32);
    assert_ne!(difference % window, U256::zero());
    let expected = U256::from(limit) - (difference + window - U256::one()) / window;
    assert_eq!(
        adjustment.mean_target_difficulty(),
        ExpandedDifficulty::from(expected)
    );
}

#[test]
fn mean_target_uses_pow_limit_through_exact_window_height() {
    let _init_guard = zebra_test::init();
    let network = testnet(100);
    let limit = network.target_difficulty_limit();
    let threshold = (limit / 4_u64).to_compact();
    let target = threshold.to_expanded().unwrap();

    // Exercise both the original and NU7 windows, including their genesis
    // ancestor. The first height allowed to average targets is window + 1.
    for window in [17_u32, 102] {
        for candidate_height in [window - 1, window, window + 1] {
            let adjustment = AdjustedDifficulty::new_from_header_time(
                DateTime::from_timestamp(PREVIOUS_TIME + 25, 0).unwrap(),
                block::Height(candidate_height - 1),
                &network,
                recent_block_data(threshold, 25).take(usize::try_from(candidate_height).unwrap()),
            );
            assert_eq!(
                adjustment.mean_target_difficulty(),
                if candidate_height <= window {
                    limit
                } else {
                    target
                },
                "candidate height {candidate_height} with window {window}"
            );
        }
    }
}
