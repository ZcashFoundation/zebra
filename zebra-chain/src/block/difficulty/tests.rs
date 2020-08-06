//! Tests for difficulty and work

use super::*;

use crate::block::Block;
use crate::serialization::ZcashDeserialize;

use color_eyre::eyre::Report;
use proptest::prelude::*;
use std::sync::Arc;

// Alias the struct constants here, so the code is easier to read.
const PRECISION: u32 = CompactDifficulty::PRECISION;
const SIGN_BIT: u32 = CompactDifficulty::SIGN_BIT;
const UNSIGNED_MANTISSA_MASK: u32 = CompactDifficulty::UNSIGNED_MANTISSA_MASK;
const OFFSET: i32 = CompactDifficulty::OFFSET;

impl Arbitrary for ExpandedDifficulty {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>())
            .prop_map(|v| ExpandedDifficulty(U256::from_little_endian(&v)))
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

impl Arbitrary for Work {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<u128>()).prop_map(Work).boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

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
        "ExpandedDifficulty(\"0100000000000000000000000000000000000000000000000000000000000000\")"
    );
    assert_eq!(
        format!("{:?}", ExpandedDifficulty(U256::MAX)),
        "ExpandedDifficulty(\"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\")"
    );

    assert_eq!(format!("{:?}", Work(0)), "Work(0x0, 0, -inf)");
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
    let small_zero_max = CompactDifficulty(UNSIGNED_MANTISSA_MASK);
    assert_eq!(small_zero_max.to_expanded(), None);

    // Special-cased zeroes, negative in the floating-point representation
    let sc_zero = CompactDifficulty(SIGN_BIT);
    assert_eq!(sc_zero.to_expanded(), None);
    let sc_zero_next = CompactDifficulty(SIGN_BIT + 1);
    assert_eq!(sc_zero_next.to_expanded(), None);
    let sc_zero_high = CompactDifficulty((1 << PRECISION) - 1);
    assert_eq!(sc_zero_high.to_expanded(), None);
    let sc_zero_max = CompactDifficulty(u32::MAX);
    assert_eq!(sc_zero_max.to_expanded(), None);
}

/// Test extreme values for CompactDifficulty.
#[test]
fn compact_extremes() {
    zebra_test::init();

    // Values equal to one
    let expanded_one = Some(ExpandedDifficulty(U256::one()));
    let work_one = None;

    let one = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + 1);
    assert_eq!(one.to_expanded(), expanded_one);
    assert_eq!(one.to_work(), work_one);
    let another_one = CompactDifficulty((1 << PRECISION) + (1 << 16));
    assert_eq!(another_one.to_expanded(), expanded_one);

    // Maximum mantissa
    let expanded_mant = Some(ExpandedDifficulty(UNSIGNED_MANTISSA_MASK.into()));
    let work_mant = None;

    let mant = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
    assert_eq!(mant.to_expanded(), expanded_mant);
    assert_eq!(mant.to_work(), work_mant);

    // Maximum valid exponent
    let exponent: U256 = (31 * 8).into();
    let u256_exp = U256::from(2).pow(exponent);
    let expanded_exp = Some(ExpandedDifficulty(u256_exp));
    let work_exp = Some(Work(
        ((U256::MAX - u256_exp) / (u256_exp + 1) + 1).as_u128(),
    ));

    let exp = CompactDifficulty((31 + OFFSET as u32) * (1 << PRECISION) + 1);
    assert_eq!(exp.to_expanded(), expanded_exp);
    assert_eq!(exp.to_work(), work_exp);

    // Maximum valid mantissa and exponent
    let exponent: U256 = (29 * 8).into();
    let u256_me = U256::from(UNSIGNED_MANTISSA_MASK) * U256::from(2).pow(exponent);
    let expanded_me = Some(ExpandedDifficulty(u256_me));
    let work_me = Some(Work((!u256_me / (u256_me + 1) + 1).as_u128()));

    let me = CompactDifficulty((31 + 1) * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
    assert_eq!(me.to_expanded(), expanded_me);
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
}

/// Test blocks using CompactDifficulty.
#[test]
#[spandoc::spandoc]
fn block_difficulty() -> Result<(), Report> {
    zebra_test::init();

    let mut blockchain = Vec::new();
    for b in &[
        &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_2_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_3_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_4_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_5_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_6_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_7_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_8_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_9_BYTES[..],
        &zebra_test::vectors::BLOCK_MAINNET_10_BYTES[..],
    ] {
        let block = Arc::<Block>::zcash_deserialize(*b)?;
        let hash: BlockHeaderHash = block.as_ref().into();
        blockchain.push((block.clone(), block.coinbase_height().unwrap(), hash));
    }

    let zero = ExpandedDifficulty(U256::zero());
    let one = ExpandedDifficulty(U256::one());
    let max_value = ExpandedDifficulty(U256::MAX);
    for (block, height, hash) in blockchain {
        /// SPANDOC: Calculate the threshold for mainnet block {?height}
        let threshold = block
            .header
            .difficulty_threshold
            .to_expanded()
            .expect("Chain blocks have valid difficulty thresholds.");

        /// SPANDOC: Check the difficulty for mainnet block {?height, ?threshold, ?hash}
        {
            assert!(hash <= threshold);
            // also check the comparison operators work
            assert!(hash > zero);
            assert!(hash > one);
            assert!(hash < max_value);
        }

        /// SPANDOC: Calculate the work for mainnet block {?height}
        let _work = block
            .header
            .difficulty_threshold
            .to_work()
            .expect("Chain blocks have valid work.");

        // TODO: check work comparison operators and cumulative work addition
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

/// Test ExpandedDifficulty and BlockHeaderHash ordering
#[test]
#[spandoc::spandoc]
fn expanded_hash_order() -> Result<(), Report> {
    zebra_test::init();

    let ex_zero = ExpandedDifficulty(U256::zero());
    let ex_one = ExpandedDifficulty(U256::one());
    let ex_max = ExpandedDifficulty(U256::MAX);
    let hash_zero = BlockHeaderHash([0; 32]);
    let hash_max = BlockHeaderHash([0xff; 32]);

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

proptest! {
    /// Check that CompactDifficulty expands without panicking, and compares
    /// correctly. Also check that the work conversion does not panic.
   #[test]
   fn prop_compact_expand(compact in any::<CompactDifficulty>()) {
       // TODO: round-trip test, once we have ExpandedDifficulty::to_compact()
       let expanded = compact.to_expanded();

       let hash_zero = BlockHeaderHash([0; 32]);
       let hash_max = BlockHeaderHash([0xff; 32]);

       if let Some(expanded) = expanded {
           prop_assert!(expanded >= hash_zero);
           prop_assert!(expanded <= hash_max);
       }

       let _work = compact.to_work();
       // TODO: work comparison and addition
   }

   /// Check that a random ExpandedDifficulty compares correctly with fixed BlockHeaderHashes.
   #[test]
   fn prop_expanded_order(expanded in any::<ExpandedDifficulty>()) {
       // TODO: round-trip test, once we have ExpandedDifficulty::to_compact()
       let hash_zero = BlockHeaderHash([0; 32]);
       let hash_max = BlockHeaderHash([0xff; 32]);

       prop_assert!(expanded >= hash_zero);
       prop_assert!(expanded <= hash_max);
   }

   /// Check that ExpandedDifficulty compares correctly with a random BlockHeaderHash.
   #[test]
   fn prop_hash_order(hash in any::<BlockHeaderHash>()) {
       let ex_zero = ExpandedDifficulty(U256::zero());
       let ex_one = ExpandedDifficulty(U256::one());
       let ex_max = ExpandedDifficulty(U256::MAX);

       prop_assert!(hash >= ex_zero);
       prop_assert!(hash <= ex_max);
       prop_assert!(hash >= ex_one || hash == ex_zero);
   }

   /// Check that a random ExpandedDifficulty and BlockHeaderHash compare correctly.
   #[test]
   #[allow(clippy::double_comparisons)]
   fn prop_expanded_hash_order(expanded in any::<ExpandedDifficulty>(), hash in any::<BlockHeaderHash>()) {
       prop_assert!(expanded < hash || expanded > hash || expanded == hash);
   }
}
