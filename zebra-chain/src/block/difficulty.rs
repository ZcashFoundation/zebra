//! Block difficulty data structures and calculations
//!
//! The block difficulty "target threshold" is stored in the block header as a
//! 32-bit `CompactDifficulty`. The `BlockHeaderHash` must be less than or equal
//! to the `ExpandedDifficulty` threshold, when represented as a 256-bit integer
//! in little-endian order.
//!
//! The target threshold is also used to calculate the `Work` for each block.
//! The block work is used to find the chain with the greatest total work. Each
//! block's work value depends on the fixed threshold in the block header, not
//! the actual work represented by the block header hash.

use primitive_types::U256;

#[cfg(test)]
use proptest::prelude::*;
#[cfg(test)]
use proptest_derive::Arbitrary;

/// A 32-bit "compact bits" value, which represents the difficulty threshold for
/// a block header.
///
/// Used for:
///   - checking the `difficulty_threshold` value in the block header,
///   - calculating the 256-bit `ExpandedDifficulty` threshold, for comparison
///     with the block header hash, and
///   - calculating the block work.
///
/// Details:
///
/// This is a floating-point encoding, with a 24-bit signed mantissa,
/// an 8-bit exponent, an offset of 3, and a radix of 256.
/// (IEEE 754 32-bit floating-point values use a separate sign bit, an implicit
/// leading mantissa bit, an offset of 127, and a radix of 2.)
///
/// The precise bit pattern of a `CompactDifficulty` value is
/// consensus-critical, because it is used for the `difficulty_threshold` field,
/// which is:
///   - part of the `BlockHeader`, which is used to create the
///     `BlockHeaderHash`, and
///   - bitwise equal to the median `ExpandedDifficulty` value of recent blocks,
///     when encoded to `CompactDifficulty` using the specified conversion
///     function.
///
/// Without these consensus rules, some `ExpandedDifficulty` values would have
/// multiple equivalent `CompactDifficulty` values, due to redundancy in the
/// floating-point format.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct CompactDifficulty(pub u32);

impl fmt::Debug for CompactDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("CompactDifficulty")
            // Use hex, because it's a float
            .field(&format_args!("{:#010x}", self.0))
            .finish()
    }
}

/// A 256-bit unsigned "expanded difficulty" value.
///
/// Used as a target threshold for the difficulty of a `BlockHeaderHash`.
///
/// Details:
///
/// The precise bit pattern of an `ExpandedDifficulty` value is
/// consensus-critical, because it is compared with the `BlockHeaderHash`.
///
/// Note that each `CompactDifficulty` value represents a range of
/// `ExpandedDifficulty` values, because the precision of the
/// floating-point format requires rounding on conversion.
///
/// Therefore, consensus-critical code must perform the specified
/// conversions to `CompactDifficulty`, even if the original
/// `ExpandedDifficulty` values are known.
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
pub struct ExpandedDifficulty(U256);

impl fmt::Debug for ExpandedDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut buf = [0; 32];
        // Use the same byte order as BlockHeaderHash
        self.0.to_little_endian(&mut buf);
        f.debug_tuple("ExpandedDifficulty")
            .field(&hex::encode(&buf))
            .finish()
    }
}

impl CompactDifficulty {
    /// CompactDifficulty exponent base.
    const BASE: u32 = 256;

    /// CompactDifficulty exponent offset.
    const OFFSET: i32 = 3;

    /// CompactDifficulty floating-point precision.
    const PRECISION: u32 = 24;

    /// CompactDifficulty sign bit, part of the signed mantissa.
    const SIGN_BIT: u32 = 1 << (CompactDifficulty::PRECISION - 1);

    /// CompactDifficulty unsigned mantissa mask.
    ///
    /// Also the maximum unsigned mantissa value.
    const UNSIGNED_MANTISSA_MASK: u32 = CompactDifficulty::SIGN_BIT - 1;

    /// Calculate the ExpandedDifficulty for a compact representation.
    ///
    /// See `ToTarget()` in the Zcash Specification, and `CheckProofOfWork()` in
    /// zcashd.
    ///
    /// Returns None for negative, zero, and overflow values. (zcashd rejects
    /// these values, before comparing the hash.)
    pub fn to_expanded(&self) -> Option<ExpandedDifficulty> {
        // The constants for this floating-point representation.
        // Alias the struct constants here, so the code is easier to read.
        const BASE: u32 = CompactDifficulty::BASE;
        const OFFSET: i32 = CompactDifficulty::OFFSET;
        const PRECISION: u32 = CompactDifficulty::PRECISION;
        const SIGN_BIT: u32 = CompactDifficulty::SIGN_BIT;
        const UNSIGNED_MANTISSA_MASK: u32 = CompactDifficulty::UNSIGNED_MANTISSA_MASK;

        // Negative values in this floating-point representation.
        // 0 if (x & 2^23 == 2^23)
        // zcashd rejects negative values without comparing the hash.
        if self.0 & SIGN_BIT == SIGN_BIT {
            return None;
        }

        // The components of the result
        // The fractional part of the floating-point number
        // x & (2^23 - 1)
        let mantissa = self.0 & UNSIGNED_MANTISSA_MASK;

        // The exponent for the multiplier in the floating-point number
        // 256^(floor(x/(2^24)) - 3)
        // The i32 conversion is safe, because we've just divided self by 2^24.
        let exponent = ((self.0 >> PRECISION) as i32) - OFFSET;

        // Normalise the mantissa and exponent before multiplying.
        //
        // zcashd rejects non-zero overflow values, but accepts overflows where
        // all the overflowing bits are zero. It also allows underflows.
        let (mantissa, exponent) = match (mantissa, exponent) {
            // Overflow: check for non-zero overflow bits
            //
            // If m is non-zero, overflow. If m is zero, invalid.
            (_, e) if (e >= 32) => return None,
            // If m is larger than the remaining bytes, overflow.
            // Otherwise, avoid overflows in base^exponent.
            (m, e) if (e == 31 && m > u8::MAX.into()) => return None,
            (m, e) if (e == 31 && m <= u8::MAX.into()) => (m << 16, e - 2),
            (m, e) if (e == 30 && m > u16::MAX.into()) => return None,
            (m, e) if (e == 30 && m <= u16::MAX.into()) => (m << 8, e - 1),

            // Underflow: perform the right shift.
            // The abs is safe, because we've just divided by 2^24, and offset
            // is small.
            (m, e) if (e < 0) => (m >> ((e.abs() * 8) as u32), 0),
            (m, e) => (m, e),
        };

        // Now calculate the result: mantissa*base^exponent
        // Earlier code should make sure all these values are in range.
        let mantissa: U256 = mantissa.into();
        let base: U256 = BASE.into();
        let exponent: U256 = exponent.into();
        let result = mantissa * base.pow(exponent);

        if result == U256::zero() {
            // zcashd rejects zero values, without comparing the hash
            None
        } else {
            Some(ExpandedDifficulty(result))
        }
    }
}

#[cfg(test)]
impl Arbitrary for ExpandedDifficulty {
    type Parameters = ();

    fn arbitrary_with(_args: ()) -> Self::Strategy {
        (any::<[u8; 32]>())
            .prop_map(|v| ExpandedDifficulty(U256::from_little_endian(&v)))
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::Report;
    use std::sync::Arc;

    use crate::block::{Block, BlockHeaderHash};
    use crate::serialization::ZcashDeserialize;

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

        assert_eq!(format!("{:?}", ExpandedDifficulty(U256::zero())), "ExpandedDifficulty(\"0000000000000000000000000000000000000000000000000000000000000000\")");
        assert_eq!(format!("{:?}", ExpandedDifficulty(U256::one())), "ExpandedDifficulty(\"0100000000000000000000000000000000000000000000000000000000000000\")");
        assert_eq!(format!("{:?}", ExpandedDifficulty(U256::MAX)), "ExpandedDifficulty(\"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff\")");
    }

    /// Test zero values for CompactDifficulty.
    #[test]
    fn compact_zero() {
        zebra_test::init();

        let natural_zero = CompactDifficulty(0);
        assert_eq!(natural_zero.to_expanded(), None);

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

        let one = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + 1);
        assert_eq!(one.to_expanded(), expanded_one);
        let another_one = CompactDifficulty((1 << PRECISION) + (1 << 16));
        assert_eq!(another_one.to_expanded(), expanded_one);

        // Maximum mantissa
        let expanded_mant = Some(ExpandedDifficulty(UNSIGNED_MANTISSA_MASK.into()));

        let mant = CompactDifficulty(OFFSET as u32 * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
        assert_eq!(mant.to_expanded(), expanded_mant);

        // Maximum valid exponent
        let exponent: U256 = (31 * 8).into();
        let expanded_exp = Some(ExpandedDifficulty(U256::from(2).pow(exponent)));

        let exp = CompactDifficulty((31 + OFFSET as u32) * (1 << PRECISION) + 1);
        assert_eq!(exp.to_expanded(), expanded_exp);

        // Maximum valid mantissa and exponent
        let exponent: U256 = (29 * 8).into();
        let expanded_me = U256::from(UNSIGNED_MANTISSA_MASK) * U256::from(2).pow(exponent);
        let expanded_me = Some(ExpandedDifficulty(expanded_me));

        let me = CompactDifficulty((31 + 1) * (1 << PRECISION) + UNSIGNED_MANTISSA_MASK);
        assert_eq!(me.to_expanded(), expanded_me);

        // Maximum value, at least according to the spec
        //
        // According to ToTarget() in the spec, this value is
        // `(2^23 - 1) * 256^253`, which is larger than the maximum expanded
        // value. Therefore, a block can never pass with this threshold.
        //
        // zcashd rejects these blocks without comparing the hash.
        let difficulty_max = CompactDifficulty(u32::MAX & !SIGN_BIT);
        assert_eq!(difficulty_max.to_expanded(), None);
    }

    /// Test blocks using CompactDifficulty.
    #[test]
    #[spandoc::spandoc]
    fn compact_blocks() -> Result<(), Report> {
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

        // Now verify each block
        for (block, height, hash) in blockchain {
            /// SPANDOC: Check the difficulty of a mainnet block {?height, ?hash}
            let threshold = block
                .header
                .difficulty_threshold
                .to_expanded()
                .expect("Chain blocks have valid difficulty thresholds.");

            // Check the difficulty of the block.
            //
            // Invert the "less than or equal" comparison, because we interpret
            // these values in little-endian order.
            // TODO: replace with PartialOrd implementation
            assert!(hash.0 >= threshold.0);
        }

        Ok(())
    }
}
