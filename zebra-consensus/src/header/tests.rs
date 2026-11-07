//! Tests for context-free header verification.

use std::sync::Arc;

use chrono::{Duration, Utc};

use zebra_chain::{
    block::{self, Block, Height},
    parameters::{testnet::RegtestParameters, Network},
    serialization::ZcashDeserializeInto,
    work::difficulty::CompactDifficulty,
};

use super::{verify_headers, HeaderError};

/// Mainnet's contiguous test block headers from genesis.
fn mainnet_headers() -> Vec<Arc<block::Header>> {
    Network::Mainnet
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

#[tokio::test]
async fn test_vector_headers_are_valid() {
    let _init_guard = zebra_test::init();

    let headers = mainnet_headers();
    assert!(headers.len() > 2);

    let hashes = verify_headers(&Network::Mainnet, Height(0), headers.clone(), Utc::now())
        .await
        .expect("test vector headers are valid");

    assert_eq!(
        hashes,
        headers
            .iter()
            .map(|header| header.hash())
            .collect::<Vec<_>>()
    );
    assert_eq!(hashes[0], Network::Mainnet.genesis_hash());

    verify_headers(
        &Network::Mainnet,
        Height(3),
        headers[3..].to_vec(),
        Utc::now(),
    )
    .await
    .expect("a batch can start above genesis");
}

#[tokio::test]
async fn empty_and_non_contiguous_batches_are_rejected() {
    let _init_guard = zebra_test::init();

    let headers = mainnet_headers();

    assert!(matches!(
        verify_headers(&Network::Mainnet, Height(1), Vec::new(), Utc::now()).await,
        Err(HeaderError::Empty)
    ));

    let gap = vec![headers[1].clone(), headers[3].clone()];
    let error = verify_headers(&Network::Mainnet, Height(1), gap, Utc::now())
        .await
        .expect_err("a gap is not contiguous");
    assert!(
        matches!(
            error,
            HeaderError::NotContiguous {
                height: Height(2),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.is_misbehavior());
}

#[tokio::test]
async fn checkpoint_mismatch_is_rejected() {
    let _init_guard = zebra_test::init();

    let headers = mainnet_headers();

    // Header 1 claimed at the genesis height.
    let error = verify_headers(
        &Network::Mainnet,
        Height(0),
        headers[1..3].to_vec(),
        Utc::now(),
    )
    .await
    .expect_err("header 1 is not the genesis checkpoint");
    assert!(
        matches!(
            error,
            HeaderError::CheckpointMismatch {
                height: Height(0),
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.is_misbehavior());
}

#[tokio::test]
async fn bad_difficulty_filter_is_rejected() {
    let _init_guard = zebra_test::init();

    let mut header = *mainnet_headers()[1];
    // A tiny target that the header's hash can't meet.
    header.difficulty_threshold = CompactDifficulty::from_bytes_in_display_order(&[3, 0, 0, 1])
        .expect("the threshold encoding is valid");

    let error = verify_headers(
        &Network::Mainnet,
        Height(1),
        vec![Arc::new(header)],
        Utc::now(),
    )
    .await
    .expect_err("the hash fails the filter");
    assert!(matches!(error, HeaderError::Difficulty { .. }), "{error:?}");
    assert!(error.is_misbehavior());
}

/// A header whose hash still passes the difficulty filter, but whose nonce no
/// longer matches its Equihash solution.
fn header_with_bad_solution() -> block::Header {
    let mut header = *mainnet_headers()[1];
    let threshold = header
        .difficulty_threshold
        .to_expanded()
        .expect("the test vector threshold is valid");

    for byte in 0..=u8::MAX {
        for index in 0..header.nonce.len() {
            let mut candidate = header;
            candidate.nonce[index] ^= byte.wrapping_add(1);
            if candidate.hash() <= threshold {
                return candidate;
            }
        }
        header.nonce[0] = header.nonce[0].wrapping_add(1);
    }

    panic!("about 1 in 8,192 nonces pass the Mainnet limit, so one of these does");
}

#[tokio::test]
async fn bad_equihash_solution_is_rejected() {
    let _init_guard = zebra_test::init();

    let header = header_with_bad_solution();
    let error = verify_headers(
        &Network::Mainnet,
        Height(1),
        vec![Arc::new(header)],
        Utc::now(),
    )
    .await
    .expect_err("the solution is invalid");
    assert!(matches!(error, HeaderError::Equihash { .. }), "{error:?}");
    assert!(error.is_misbehavior());
}

#[tokio::test]
async fn disable_pow_skips_the_proof_of_work() {
    let _init_guard = zebra_test::init();

    let regtest = Network::new_regtest(RegtestParameters::default());
    assert!(regtest.disable_pow());

    let header = header_with_bad_solution();
    verify_headers(&regtest, Height(1), vec![Arc::new(header)], Utc::now())
        .await
        .expect("Regtest skips the filter and Equihash, as the block verifier does");
}

#[tokio::test]
async fn future_time_is_rejected_without_blame() {
    let _init_guard = zebra_test::init();

    let header = mainnet_headers()[1].clone();
    let now = header.time - Duration::hours(3);

    let error = verify_headers(&Network::Mainnet, Height(1), vec![header], now)
        .await
        .expect_err("the header is more than 2 hours in the future");
    assert!(matches!(error, HeaderError::Time { .. }), "{error:?}");
    assert!(!error.is_misbehavior());
}
