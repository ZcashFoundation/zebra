//! Fixed test vectors for state contextual validation checks.

use chrono::{DateTime, Duration};

use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    work::difficulty::ParameterDifficulty,
};

use super::super::*;

#[test]
fn test_orphan_consensus_check() {
    let _init_guard = zebra_test::init();

    let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap()
        .coinbase_height()
        .unwrap();

    block_is_not_orphaned(block::Height(0), height).expect("tip is lower so it should be fine");
    block_is_not_orphaned(block::Height(347498), height)
        .expect("tip is lower so it should be fine");
    block_is_not_orphaned(block::Height(347499), height)
        .expect_err("tip is equal so it should error");
    block_is_not_orphaned(block::Height(500000), height)
        .expect_err("tip is higher so it should error");
}

#[test]
fn test_sequential_height_check() {
    let _init_guard = zebra_test::init();

    let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap()
        .coinbase_height()
        .unwrap();

    height_one_more_than_parent_height(block::Height(0), height)
        .expect_err("block is much lower, should panic");
    height_one_more_than_parent_height(block::Height(347497), height)
        .expect_err("parent height is 2 less, should panic");
    height_one_more_than_parent_height(block::Height(347498), height)
        .expect("parent height is 1 less, should be good");
    height_one_more_than_parent_height(block::Height(347499), height)
        .expect_err("parent height is equal, should panic");
    height_one_more_than_parent_height(block::Height(347500), height)
        .expect_err("parent height is way more, should panic");
    height_one_more_than_parent_height(block::Height(500000), height)
        .expect_err("parent height is way more, should panic");
}

#[test]
fn header_daa_accepts_valid_threshold_with_full_context() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let previous_block_height = block::Height(99);
    let candidate_time = DateTime::from_timestamp(15_000, 0).expect("test timestamp is in-range");
    let relevant_headers = daa_context(&network, previous_block_height, candidate_time);
    let expected = AdjustedDifficulty::new_from_header_time(
        candidate_time,
        previous_block_height,
        &network,
        relevant_headers.clone(),
    )
    .expected_difficulty_threshold();
    let mut candidate = *zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes")
        .header
        .as_ref();
    candidate.time = candidate_time;
    candidate.difficulty_threshold = expected;

    header_is_valid_for_recent_chain(
        &candidate,
        previous_block_height,
        &network,
        relevant_headers,
    )
    .expect("expected DAA threshold is accepted");
}

#[test]
fn header_daa_rejects_bad_threshold_with_full_context() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let previous_block_height = block::Height(99);
    let candidate_time = DateTime::from_timestamp(15_000, 0).expect("test timestamp is in-range");
    let relevant_headers = daa_context(&network, previous_block_height, candidate_time);
    let mut candidate = *zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes")
        .header
        .as_ref();
    candidate.time = candidate_time;
    candidate.difficulty_threshold = network.target_difficulty_limit().to_compact();

    header_is_valid_for_recent_chain(
        &candidate,
        previous_block_height,
        &network,
        relevant_headers,
    )
    .expect_err("unexpected DAA threshold is rejected");
}

#[test]
fn height_one_header_skips_max_time_limit_but_later_mainnet_headers_do_not() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis block deserializes");
    let block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 1 deserializes");
    let mut candidate = *block1.header;
    candidate.time = genesis.header.time + Duration::hours(24);
    let context = [(genesis.header.difficulty_threshold, genesis.header.time)];

    header_is_valid_for_recent_chain(&candidate, block::Height(0), &network, context)
        .expect("height 1 is outside the Mainnet max-time consensus rule");

    let block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("block 2 deserializes");
    let mut candidate = *block2.header;
    candidate.time = block1.header.time + Duration::hours(24);
    let context = [
        (block1.header.difficulty_threshold, block1.header.time),
        (genesis.header.difficulty_threshold, genesis.header.time),
    ];

    assert!(matches!(
        header_is_valid_for_recent_chain(&candidate, block::Height(1), &network, context),
        Err(ValidateContextError::TimeTooLate { .. })
    ));
}

#[test]
fn short_context_early_height_uses_pow_limit_threshold() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let genesis = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .expect("genesis block deserializes");
    let candidate_time =
        genesis.header.time + NetworkUpgrade::target_spacing_for_height(&network, block::Height(1));
    let context = [(genesis.header.difficulty_threshold, genesis.header.time)];

    let expected = difficulty::AdjustedDifficulty::new_from_header_time(
        candidate_time,
        block::Height(0),
        &network,
        context,
    )
    .expected_difficulty_threshold();

    assert_eq!(expected, network.target_difficulty_limit().to_compact());
}

fn daa_context(
    network: &Network,
    previous_block_height: block::Height,
    candidate_time: DateTime<chrono::Utc>,
) -> Vec<(
    zebra_chain::work::difficulty::CompactDifficulty,
    DateTime<chrono::Utc>,
)> {
    let candidate_height = previous_block_height
        .next()
        .expect("test candidate height is valid");
    let target_spacing = NetworkUpgrade::target_spacing_for_height(network, candidate_height);
    let difficulty = network.target_difficulty_limit().to_compact();

    (0..difficulty::POW_ADJUSTMENT_BLOCK_SPAN)
        .map(|offset| {
            let offset = i32::try_from(offset + 1).expect("test offset fits in i32");
            (difficulty, candidate_time - target_spacing * offset)
        })
        .collect()
}
