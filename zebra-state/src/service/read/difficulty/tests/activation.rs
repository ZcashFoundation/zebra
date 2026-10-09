//! Minimum-difficulty activation uses the candidate height, not the parent height.

use super::*;
use zebra_chain::work::difficulty::ParameterDifficulty as _;

#[test]
fn minimum_difficulty_activation_uses_candidate_height() {
    let network = Network::new_default_testnet();
    let gap = 6 * 150;
    let mut result = chain_info(PREV + gap);

    adjust_difficulty_and_time_for_testnet(
        &mut result,
        &network,
        Height(299_187),
        recent_block_data(&network),
    );

    assert_eq!(result.max_time, DateTime32::from(PREV + gap));
    assert_eq!(result.cur_time, DateTime32::from(PREV + gap));
    assert_eq!(result.expected_difficulty, CompactDifficulty::default());

    let mut past_threshold = chain_info(PREV + gap + 1);
    adjust_difficulty_and_time_for_testnet(
        &mut past_threshold,
        &network,
        Height(299_187),
        recent_block_data(&network),
    );
    assert_eq!(past_threshold.min_time, DateTime32::from(PREV + gap + 1));
    assert_eq!(
        past_threshold.expected_difficulty,
        network.target_difficulty_limit().to_compact()
    );

    let mut before_activation = chain_info(PREV + gap + 1);
    adjust_difficulty_and_time_for_testnet(
        &mut before_activation,
        &network,
        Height(299_186),
        recent_block_data(&network),
    );
    assert_eq!(
        before_activation.expected_difficulty,
        CompactDifficulty::default()
    );
    assert_eq!(before_activation.cur_time, DateTime32::from(PREV + gap + 1));
    assert_eq!(
        before_activation.max_time,
        DateTime32::from(PREV + BLOCK_MAX_TIME_SINCE_MEDIAN)
    );
}
