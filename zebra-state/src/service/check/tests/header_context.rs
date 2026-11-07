//! Tests for [`check_header_context`], the difficulty and time checks of headers
//! that arrive before their bodies.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::ZcashDeserializeInto,
    work::difficulty::{ExpandedDifficulty, ParameterDifficulty as _, U256},
};

use crate::{service::check::check_header_context, ValidateContextError};

/// The network's contiguous test block headers from genesis.
fn test_headers(network: &Network) -> Vec<Arc<block::Header>> {
    network
        .blockchain_map()
        .iter()
        .enumerate()
        .take_while(|(index, (height, _))| usize::try_from(**height) == Ok(*index))
        .map(|(_, (_, bytes))| {
            bytes
                .zcash_deserialize_into::<Block>()
                .expect("test data deserializes")
                .header
        })
        .collect()
}

/// The headers below `height`, in reverse height order.
fn context_below(headers: &[Arc<block::Header>], height: usize) -> Vec<Arc<block::Header>> {
    headers[..height].iter().rev().cloned().collect()
}

/// Every test vector header is valid for the headers below it, even with the
/// short context near genesis.
#[test]
fn test_vector_headers_are_valid() {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        let headers = test_headers(&network);
        assert!(headers.len() > 2, "{network} has test blocks");

        for height in 1..headers.len() {
            check_header_context(
                &context_below(&headers, height),
                &headers[height],
                Height(height.try_into().expect("test heights fit in u32")),
                &network,
            )
            .unwrap_or_else(|error| panic!("{network} header {height} is valid: {error:?}"));
        }
    }
}

/// A header with another difficulty threshold, or a time at or before the
/// median-time-past, or too far after it, is rejected.
#[test]
fn wrong_threshold_and_time_are_rejected() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let headers = test_headers(&network);
    let height = headers.len() - 1;
    let context = context_below(&headers, height);
    let candidate_height = Height(height.try_into().expect("test heights fit in u32"));

    let mut header = *headers[height];
    header.difficulty_threshold = context[0]
        .difficulty_threshold
        .to_expanded()
        .map(|expanded| (U256::from(expanded) >> 1).into())
        .map(|expanded: ExpandedDifficulty| expanded.to_compact())
        .expect("test thresholds are valid");
    assert!(matches!(
        check_header_context(&context, &header, candidate_height, &network),
        Err(ValidateContextError::InvalidDifficultyThreshold { .. })
    ));

    let mut header = *headers[height];
    header.time = DateTime::<Utc>::from_timestamp(0, 0).expect("the epoch is a valid time");
    assert!(matches!(
        check_header_context(&context, &header, candidate_height, &network),
        Err(ValidateContextError::TimeTooEarly { .. })
    ));

    let mut header = *headers[height];
    header.time = context[0].time + Duration::days(1);
    assert!(matches!(
        check_header_context(&context, &header, candidate_height, &network),
        Err(ValidateContextError::TimeTooLate { .. })
    ));
}

/// Genesis has no previous header, and an empty context can't be checked.
#[test]
fn genesis_and_empty_context_are_not_ready() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let headers = test_headers(&network);

    assert!(matches!(
        check_header_context(&[], &headers[1], Height(1), &network),
        Err(ValidateContextError::NotReadyToBeCommitted)
    ));
    assert!(matches!(
        check_header_context(&headers[..1], &headers[0], Height(0), &network),
        Err(ValidateContextError::NotReadyToBeCommitted)
    ));
}

/// On Testnet, a header more than six target spacings after its parent may use
/// the minimum difficulty, and one that comes sooner may not (ZIPs 205 and 208).
#[test]
fn testnet_minimum_difficulty_needs_a_late_header() {
    let _init_guard = zebra_test::init();

    let network = Network::new_default_testnet();
    let template = test_headers(&network)[1].clone();

    // A height well past the minimum difficulty start height and Blossom, so the
    // target spacing is 75 seconds.
    let height = Height(1_000_000);
    let spacing = Duration::seconds(75);
    let limit = network.target_difficulty_limit();
    let threshold = ExpandedDifficulty::from(U256::from(limit) >> 8).to_compact();
    let parent_time = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid time");

    let context: Vec<Arc<block::Header>> = (0..30)
        .map(|index| {
            let mut header = *template;
            header.difficulty_threshold = threshold;
            header.time = parent_time - spacing * index;
            Arc::new(header)
        })
        .collect();

    let mut late = *template;
    late.time = parent_time + spacing * 7;
    late.difficulty_threshold = limit.to_compact();
    check_header_context(&context, &late, height, &network)
        .expect("a late Testnet header may use the minimum difficulty");

    let mut early = late;
    early.time = parent_time + spacing;
    assert!(matches!(
        check_header_context(&context, &early, height, &network),
        Err(ValidateContextError::InvalidDifficultyThreshold { .. })
    ));
}
