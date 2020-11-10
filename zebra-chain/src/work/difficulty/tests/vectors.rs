use color_eyre::eyre::Report;
use std::sync::Arc;

use crate::block::Block;
use crate::serialization::ZcashDeserialize;

use super::super::*;

// Alias the struct constants here, so the code is easier to read.
const PRECISION: u32 = CompactDifficulty::PRECISION;
const SIGN_BIT: u32 = CompactDifficulty::SIGN_BIT;
const UNSIGNED_MANTISSA_MASK: u32 = CompactDifficulty::UNSIGNED_MANTISSA_MASK;
const OFFSET: i32 = CompactDifficulty::OFFSET;

/// Test debug formatting.
#[test]
fn debug_format() {
    zebra_test::init();

    assert_eq!(
        format!("{:?}", CompactDifficulty(0)),
        "CompactDifficulty(0x00000000)"
    );
    assert_eq!(
        format!("{:?}", CompactDifficulty(1)),
        "CompactDifficulty(0x00000001)"
    );
    assert_eq!(
        format!("{:?}", CompactDifficulty(u32::MAX)),
        "CompactDifficulty(0xffffffff)"
    );

    assert_eq!(
        format!("{:?}", ExpandedDifficulty(U256::zero())),
        "ExpandedDifficulty(\"0000000000000000000000000000000000000000000000000000000000000000\")"
    );
    assert_eq!(
        format!("{:?}", ExpandedDifficulty(U256::one())),
        "ExpandedDifficulty(\"0000000000000000000000000000000000000000000000000000000000000001\")"
    );
    assert_eq!(
        format!("{:?}", ExpandedDifficulty(U256::MAX)),
        "ExpandedDifficulty(\"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\")"
    );

    assert_eq!(format!("{:?}", Work(0)), "Work(0x0, 0, -inf)");
    assert_eq!(format!("{:?}", Work(1)), "Work(0x1, 1, 0.00000)");
    assert_eq!(
        format!("{:?}", Work(u8::MAX as u128)),
        "Work(0xff, 255, 7.99435)"
    );
    assert_eq!(
        format!("{:?}", Work(u64::MAX as u128)),
        "Work(0xffffffffffffffff, 18446744073709551615, 64.00000)"
    );
    assert_eq!(
        format!("{:?}", Work(u128::MAX)),
        "Work(0xffffffffffffffffffffffffffffffff, 340282366920938463463374607431768211455, 128.00000)"
    );
}

/// Test zero values for CompactDifficulty.
#[test]
fn compact_zero() {
    zebra_test::init();

    let natural_zero = CompactDifficulty(0);
    assert_eq!(natural_zero.to_expanded(), None);
    assert_eq!(natural_zero.to_work(), None);

    // Small value zeroes
    let small_zero_1 = CompactDifficulty(1);
    assert_eq!(small_zero_1.to_expanded(), None);
    assert_eq!(small_zero_1.to_work(), None);
    let small_zero_max = CompactDifficulty(UNSIGNED_MANTISSA_MASK);
    assert_eq!(small_zero_max.to_expanded(), None);
    assert_eq!(small_zero_max.to_work(), None);

    // Special-cased zeroes, negative in the floating-point representation
    let sc_zero = CompactDifficulty(SIGN_BIT);
    assert_eq!(sc_zero.to_expanded(), None);
    assert_eq!(sc_zero.to_work(), None);
    let sc_zero_next = CompactDifficulty(SIGN_BIT + 1);
    assert_eq!(sc_zero_next.to_expanded(), None);
    assert_eq!(sc_zero_next.to_work(), None);
    let sc_zero_high = CompactDifficulty((1 << PRECISION) - 1);
    assert_eq!(sc_zero_high.to_expanded(), None);
    assert_eq!(sc_zero_high.to_work(), None);
    let sc_zero_max = CompactDifficulty(u32::MAX);
    assert_eq!(sc_zero_max.to_expanded(), None);
    assert_eq!(sc_zero_max.to_work(), None);
}

/// Test extreme values for CompactDifficulty.
#[test]
fn compact_extremes() {
    zebra_test::init();

    // Values equal to one
    let expanded_one = Some(ExpandedDifficulty(U256::one()));
    let work_one = None;

    let canonical_one = CompactDifficulty((1 << PRECISION) + (1 << 16));
    assert_eq!(canonical_one.to_expanded(), expanded_one);
    assert_eq!(
        canonical_one.to_expanded().unwrap().to_compact(),
        canonical_one
    );
    assert_eq!(canonical_one.to_work(), work_one);

    let another_one = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + 1);
    assert_eq!(another_one.to_expanded(), expanded_one);
    assert_eq!(
        another_one.to_expanded().unwrap().to_compact(),
        canonical_one
    );
    assert_eq!(another_one.to_work(), work_one);

    // Maximum mantissa
    let expanded_mant = Some(ExpandedDifficulty(UNSIGNED_MANTISSA_MASK.into()));
    let work_mant = None;

    let mant = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
    assert_eq!(mant.to_expanded(), expanded_mant);
    assert_eq!(mant.to_expanded().unwrap().to_compact(), mant);
    assert_eq!(mant.to_work(), work_mant);

    // Maximum valid exponent
    let exponent: U256 = (31 * 8).into();
    let u256_exp = U256::from(2).pow(exponent);
    let expanded_exp = Some(ExpandedDifficulty(u256_exp));
    let work_exp = Some(Work(
        ((U256::MAX - u256_exp) / (u256_exp + 1) + 1).as_u128(),
    ));

    let canonical_exp =
        CompactDifficulty(((31 + OFFSET - 2) as u32) * (1 << PRECISION) + (1 << 16));
    let another_exp = CompactDifficulty((31 + OFFSET as u32) * (1 << PRECISION) + 1);
    assert_eq!(canonical_exp.to_expanded(), expanded_exp);
    assert_eq!(another_exp.to_expanded(), expanded_exp);
    assert_eq!(
        canonical_exp.to_expanded().unwrap().to_compact(),
        canonical_exp
    );
    assert_eq!(
        another_exp.to_expanded().unwrap().to_compact(),
        canonical_exp
    );
    assert_eq!(canonical_exp.to_work(), work_exp);
    assert_eq!(another_exp.to_work(), work_exp);

    // Maximum valid mantissa and exponent
    let exponent: U256 = (29 * 8).into();
    let u256_me = U256::from(UNSIGNED_MANTISSA_MASK) * U256::from(2).pow(exponent);
    let expanded_me = Some(ExpandedDifficulty(u256_me));
    let work_me = Some(Work((!u256_me / (u256_me + 1) + 1).as_u128()));

    let me = CompactDifficulty((31 + 1) * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
    assert_eq!(me.to_expanded(), expanded_me);
    assert_eq!(me.to_expanded().unwrap().to_compact(), me);
    assert_eq!(me.to_work(), work_me);

    // Maximum value, at least according to the spec
    //
    // According to ToTarget() in the spec, this value is
    // `(2^23 - 1) * 256^253`, which is larger than the maximum expanded
    // value. Therefore, a block can never pass with this threshold.
    //
    // zcashd rejects these blocks without comparing the hash.
    let difficulty_max = CompactDifficulty(u32::MAX & !SIGN_BIT);
    assert_eq!(difficulty_max.to_expanded(), None);
    assert_eq!(difficulty_max.to_work(), None);

    // Bitcoin test vectors for CompactDifficulty
    // See https://developer.bitcoin.org/reference/block_chain.html#target-nbits
    // These values are not in the table below, because they do not fit in u128
    //
    // The minimum difficulty on the bitcoin mainnet and testnet
    let difficulty_btc_main = CompactDifficulty(0x1d00ffff);
    let u256_btc_main = U256::from(0xffff) << 208;
    let expanded_btc_main = Some(ExpandedDifficulty(u256_btc_main));
    let work_btc_main = Some(Work(0x100010001));
    assert_eq!(difficulty_btc_main.to_expanded(), expanded_btc_main);
    assert_eq!(
        difficulty_btc_main.to_expanded().unwrap().to_compact(),
        difficulty_btc_main
    );
    assert_eq!(difficulty_btc_main.to_work(), work_btc_main);

    // The minimum difficulty in bitcoin regtest
    // This is also the easiest respesentable difficulty
    let difficulty_btc_reg = CompactDifficulty(0x207fffff);
    let u256_btc_reg = U256::from(0x7fffff) << 232;
    let expanded_btc_reg = Some(ExpandedDifficulty(u256_btc_reg));
    let work_btc_reg = Some(Work(0x2));
    assert_eq!(difficulty_btc_reg.to_expanded(), expanded_btc_reg);
    assert_eq!(
        difficulty_btc_reg.to_expanded().unwrap().to_compact(),
        difficulty_btc_reg
    );
    assert_eq!(difficulty_btc_reg.to_work(), work_btc_reg);
}

/// Bitcoin test vectors for CompactDifficulty, and their corresponding
/// ExpandedDifficulty and Work values.
/// See https://developer.bitcoin.org/reference/block_chain.html#target-nbits
static COMPACT_DIFFICULTY_CASES: &[(u32, Option<u128>, Option<u128>)] = &[
    // These Work values will never happen in practice, because the corresponding
    // difficulties are extremely high. So it is ok for us to reject them.
    (0x01003456, None /* 0x00 */, None),
    (0x01123456, Some(0x12), None),
    (0x02008000, Some(0x80), None),
    (0x05009234, Some(0x92340000), None),
    (0x04923456, None /* -0x12345600 */, None),
    (0x04123456, Some(0x12345600), None),
];

/// Test Bitcoin test vectors for CompactDifficulty.
#[test]
#[spandoc::spandoc]
fn compact_bitcoin_test_vectors() {
    zebra_test::init();

    // We use two spans, so we can diagnose conversion panics, and mismatching results
    for (compact, expected_expanded, expected_work) in COMPACT_DIFFICULTY_CASES.iter().cloned() {
        /// SPANDOC: Convert compact to expanded and work {?compact, ?expected_expanded, ?expected_work}
        {
            let expected_expanded = expected_expanded.map(U256::from).map(ExpandedDifficulty);
            let expected_work = expected_work.map(Work);

            let compact = CompactDifficulty(compact);
            let actual_expanded = compact.to_expanded();
            let actual_work = compact.to_work();
            let canonical_compact = actual_expanded.map(|e| e.to_compact());
            let round_trip_expanded = canonical_compact.map(|c| c.to_expanded());

            /// SPANDOC: Test that compact produces the expected expanded and work {?compact, ?expected_expanded, ?actual_expanded, ?expected_work, ?actual_work, ?canonical_compact, ?round_trip_expanded}
            {
                assert_eq!(actual_expanded, expected_expanded);
                if expected_expanded.is_some() {
                    assert_eq!(round_trip_expanded.unwrap(), actual_expanded);
                }
                assert_eq!(actual_work, expected_work);
            }
        }
    }
}

/// Test blocks using CompactDifficulty.
#[test]
#[spandoc::spandoc]
fn block_difficulty() -> Result<(), Report> {
    zebra_test::init();

    let mut blockchain = Vec::new();
    for b in zebra_test::vectors::BLOCKS.iter() {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash = block.hash();
        blockchain.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    let diff_zero = ExpandedDifficulty(U256::zero());
    let diff_one = ExpandedDifficulty(U256::one());
    let diff_max = ExpandedDifficulty(U256::MAX);

    let work_zero = PartialCumulativeWork(0);
    let work_max = PartialCumulativeWork(u128::MAX);

    let mut cumulative_work = PartialCumulativeWork::default();
    let mut previous_cumulative_work = PartialCumulativeWork::default();
    for (block, height, hash) in blockchain {
        /// SPANDOC: Calculate the threshold for block {?height}
        let threshold = block
            .header
            .difficulty_threshold
            .to_expanded()
            .expect("Chain blocks have valid difficulty thresholds.");

        /// SPANDOC: Check the difficulty for block {?height, ?threshold, ?hash}
        {
            assert!(hash <= threshold);
            // also check the comparison operators work
            assert!(hash > diff_zero);
            assert!(hash > diff_one);
            assert!(hash < diff_max);
        }

        /// SPANDOC: Check compact round-trip for block {?height}
        {
            let canonical_compact = threshold.to_compact();

            assert_eq!(block.header.difficulty_threshold,
                       canonical_compact);
        }

        /// SPANDOC: Check the work for block {?height}
        {
            let work = block
                .header
                .difficulty_threshold
                .to_work()
                .expect("Chain blocks have valid work.");

            // also check the comparison operators work
            assert!(PartialCumulativeWork::from(work) > work_zero);
            assert!(PartialCumulativeWork::from(work) < work_max);

            cumulative_work += work;
            assert!(cumulative_work > work_zero);
            assert!(cumulative_work < work_max);

            assert!(cumulative_work > previous_cumulative_work);

            previous_cumulative_work = cumulative_work;
        }
    }

    Ok(())
}

/// Test that the genesis block threshold is PowLimit
#[test]
fn genesis_block_difficulty() -> Result<(), Report> {
    genesis_block_difficulty_for_network(Network::Mainnet)?;
    genesis_block_difficulty_for_network(Network::Testnet)?;

    Ok(())
}

#[spandoc::spandoc]
fn genesis_block_difficulty_for_network(network: Network) -> Result<(), Report> {
    zebra_test::init();

    let block = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.get(&0),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.get(&0),
    };

    let block = block.expect("test vectors contain the genesis block");
    let block = Block::zcash_deserialize(&block[..]).expect("block test vector should deserialize");
    let hash = block.hash();

    /// SPANDOC: Calculate the threshold for the genesis block {?network}
    let threshold = block
        .header
        .difficulty_threshold
        .to_expanded()
        .expect("Chain blocks have valid difficulty thresholds.");

    /// SPANDOC: Check the genesis PoWLimit {?network, ?threshold, ?hash}
    {
        assert_eq!(threshold, ExpandedDifficulty::target_difficulty_limit(network),
                   "genesis block difficulty thresholds must be equal to the PoWLimit");
    }

    Ok(())
}

/// Test ExpandedDifficulty ordering
#[test]
#[spandoc::spandoc]
#[allow(clippy::eq_op)]
fn expanded_order() -> Result<(), Report> {
    zebra_test::init();

    let zero = ExpandedDifficulty(U256::zero());
    let one = ExpandedDifficulty(U256::one());
    let max_value = ExpandedDifficulty(U256::MAX);

    assert!(zero < one);
    assert!(zero < max_value);
    assert!(one < max_value);

    assert_eq!(zero, zero);
    assert!(zero <= one);
    assert!(one >= zero);
    assert!(one > zero);

    Ok(())
}

/// Test ExpandedDifficulty and block::Hash ordering
#[test]
#[spandoc::spandoc]
fn expanded_hash_order() -> Result<(), Report> {
    zebra_test::init();

    let ex_zero = ExpandedDifficulty(U256::zero());
    let ex_one = ExpandedDifficulty(U256::one());
    let ex_max = ExpandedDifficulty(U256::MAX);
    let hash_zero = block::Hash([0; 32]);
    let hash_max = block::Hash([0xff; 32]);

    assert_eq!(hash_zero, ex_zero);
    assert!(hash_zero < ex_one);
    assert!(hash_zero < ex_max);

    assert!(hash_max > ex_zero);
    assert!(hash_max > ex_one);
    assert_eq!(hash_max, ex_max);

    assert!(ex_one > hash_zero);
    assert!(ex_one < hash_max);

    assert!(hash_zero >= ex_zero);
    assert!(ex_zero >= hash_zero);
    assert!(hash_zero <= ex_zero);
    assert!(ex_zero <= hash_zero);

    Ok(())
}
