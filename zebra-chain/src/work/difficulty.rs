//! Block difficulty data structures and calculations
//!
//! The block difficulty "target threshold" is stored in the block header as a
//! 32-bit `CompactDifficulty`. The `block::Hash` must be less than or equal
//! to the `ExpandedDifficulty` threshold, when represented as a 256-bit integer
//! in little-endian order.
//!
//! The target threshold is also used to calculate the `Work` for each block.
//! The block work is used to find the chain with the greatest total work. Each
//! block's work value depends on the fixed threshold in the block header, not
//! the actual work represented by the block header hash.

use std::{
    cmp::{Ordering, PartialEq, PartialOrd},
    fmt,
    iter::Sum,
    ops::{Add, Div, Mul},
};

use hex::{FromHex, ToHex};

use crate::{block, parameters::Network, serialization::BytesInDisplayOrder, BoxError};

pub use crate::work::u256::U256;

#[cfg(any(test, feature = "proptest-impl"))]
mod arbitrary;
#[cfg(test)]
mod tests;

/// A 32-bit "compact bits" value, which represents the difficulty threshold for
/// a block header.
///
/// Used for:
///   - checking the `difficulty_threshold` value in the block header,
///   - calculating the 256-bit `ExpandedDifficulty` threshold, for comparison
///     with the block header hash, and
///   - calculating the block work.
///
/// # Consensus
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
///     `block::Hash`, and
///   - bitwise equal to the median `ExpandedDifficulty` value of recent blocks,
///     when encoded to `CompactDifficulty` using the specified conversion
///     function.
///
/// Without these consensus rules, some `ExpandedDifficulty` values would have
/// multiple equivalent `CompactDifficulty` values, due to redundancy in the
/// floating-point format.
///
/// > Deterministic conversions between a target threshold and a “compact" nBits value
/// > are not fully defined in the Bitcoin documentation, and so we define them here:
/// > (see equations in the Zcash Specification [section 7.7.4])
///
/// [section 7.7.4]: https://zips.z.cash/protocol/protocol.pdf#nbits
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Default))]
pub struct CompactDifficulty(pub(crate) u32);

/// An invalid CompactDifficulty value, for testing.
pub const INVALID_COMPACT_DIFFICULTY: CompactDifficulty = CompactDifficulty(u32::MAX);

/// A 256-bit unsigned "expanded difficulty" value.
///
/// Used as a target threshold for the difficulty of a `block::Hash`.
///
/// # Consensus
///
/// The precise bit pattern of an `ExpandedDifficulty` value is
/// consensus-critical, because it is compared with the `block::Hash`.
///
/// Note that each `CompactDifficulty` value can be converted from a
/// range of `ExpandedDifficulty` values, because the precision of
/// the floating-point format requires rounding on conversion.
///
/// Therefore, consensus-critical code must perform the specified
/// conversions to `CompactDifficulty`, even if the original
/// `ExpandedDifficulty` values are known.
///
/// Callers should avoid constructing `ExpandedDifficulty` zero
/// values, because they are rejected by the consensus rules,
/// and cause some conversion functions to panic.
///
/// > The difficulty filter is unchanged from Bitcoin, and is calculated using SHA-256d on the
/// > whole block header (including solutionSize and solution). The result is interpreted as a
/// > 256-bit integer represented in little-endian byte order, which MUST be less than or equal
/// > to the target threshold given by ToTarget(nBits).
///
/// Zcash Specification [section 7.7.2].
///
/// [section 7.7.2]: https://zips.z.cash/protocol/protocol.pdf#difficulty
//
// TODO: Use NonZeroU256, when available
#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ExpandedDifficulty(U256);

/// A 128-bit unsigned "Work" value.
///
/// Used to calculate the total work for each chain of blocks.
///
/// # Consensus
///
/// The relative value of `Work` is consensus-critical, because it is used to
/// choose the best chain. But its precise value and bit pattern are not
/// consensus-critical.
///
/// We calculate work values according to the Zcash specification, but store
/// them as u128, rather than the implied u256. We don't expect the total chain
/// work to ever exceed 2^128. The current total chain work for Zcash is 2^58,
/// and Bitcoin adds around 2^91 work per year. (Each extra bit represents twice
/// as much work.)
///
/// > a node chooses the “best” block chain visible to it by finding the chain of valid blocks
/// > with the greatest total work. The work of a block with value nBits for the nBits field in
/// > its block header is defined as `floor(2^256 / (ToTarget(nBits) + 1))`.
///
/// Zcash Specification [section 7.7.5].
///
/// [section 7.7.5]: https://zips.z.cash/protocol/protocol.pdf#workdef
#[derive(Clone, Copy, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct Work(u128);

impl Work {
    /// Returns a value representing no work.
    pub fn zero() -> Self {
        Self(0)
    }

    /// Return the inner `u128` value.
    pub fn as_u128(self) -> u128 {
        self.0
    }
}

impl fmt::Debug for Work {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // There isn't a standard way to show different representations of the
        // same value
        f.debug_tuple("Work")
            // Use hex, because expanded difficulty is in hex.
            .field(&format_args!("{:#x}", self.0))
            // Use decimal, to compare with zcashd
            .field(&format_args!("{}", self.0))
            // Use log2, to compare with zcashd
            .field(&format_args!("{:.5}", (self.0 as f64).log2()))
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
    /// zcashd:
    /// <https://zips.z.cash/protocol/protocol.pdf#nbits>
    ///
    /// Returns None for negative, zero, and overflow values. (zcashd rejects
    /// these values, before comparing the hash.)
    #[allow(clippy::unwrap_in_result)]
    pub fn to_expanded(self) -> Option<ExpandedDifficulty> {
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
        //
        // The i32 conversion is safe, because we've just divided self by 2^24.
        let exponent = i32::try_from(self.0 >> PRECISION).expect("fits in i32") - OFFSET;

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
            Some(result.into())
        }
    }

    /// Calculate the Work for a compact representation.
    ///
    /// See `Definition of Work` in the [Zcash Specification], and
    /// `GetBlockProof()` in zcashd.
    ///
    /// Returns None if the corresponding ExpandedDifficulty is None.
    /// Also returns None on Work overflow, which should be impossible on a
    /// valid chain.
    ///
    /// [Zcash Specification]: https://zips.z.cash/protocol/protocol.pdf#workdef
    pub fn to_work(self) -> Option<Work> {
        let expanded = self.to_expanded()?;
        Work::try_from(expanded).ok()
    }

    /// Return the difficulty bytes in big-endian byte-order.
    ///
    /// Zebra displays difficulties in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    pub fn bytes_in_display_order(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    /// Convert bytes in big-endian byte-order into a [`CompactDifficulty`].
    ///
    /// Zebra displays difficulties in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    ///
    /// Returns an error if the difficulty value is invalid.
    pub fn from_bytes_in_display_order(
        bytes_in_display_order: &[u8; 4],
    ) -> Result<CompactDifficulty, BoxError> {
        let internal_byte_order = u32::from_be_bytes(*bytes_in_display_order);

        let difficulty = CompactDifficulty(internal_byte_order);

        if difficulty.to_expanded().is_none() {
            return Err("invalid difficulty value".into());
        }

        Ok(difficulty)
    }

    /// Returns a floating-point number representing a difficulty as a multiple
    /// of the minimum difficulty for the provided network.
    // Copied from <https://github.com/zcash/zcash/blob/99ad6fdc3a549ab510422820eea5e5ce9f60a5fd/src/rpc/blockchain.cpp#L34-L74>
    // TODO: Explain here what this ported code is doing and why, request help to do so with the ECC team.
    pub fn relative_to_network(&self, network: &Network) -> f64 {
        let network_difficulty = network.target_difficulty_limit().to_compact();

        let [mut n_shift, ..] = self.0.to_be_bytes();
        let [n_shift_amount, ..] = network_difficulty.0.to_be_bytes();
        let mut d_diff = f64::from(network_difficulty.0 << 8) / f64::from(self.0 << 8);

        while n_shift < n_shift_amount {
            d_diff *= 256.0;
            n_shift += 1;
        }

        while n_shift > n_shift_amount {
            d_diff /= 256.0;
            n_shift -= 1;
        }

        d_diff
    }
}

impl fmt::Debug for CompactDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // There isn't a standard way to show different representations of the
        // same value
        f.debug_tuple("CompactDifficulty")
            // Use hex, because it's a float
            .field(&format_args!("{:#010x}", self.0))
            // Use expanded difficulty, for bitwise difficulty comparisons
            .field(&format_args!("{:?}", self.to_expanded()))
            .finish()
    }
}

impl fmt::Display for CompactDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl ToHex for &CompactDifficulty {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for CompactDifficulty {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for CompactDifficulty {
    type Error = BoxError;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes_in_display_order = <[u8; 4]>::from_hex(hex)?;

        CompactDifficulty::from_bytes_in_display_order(&bytes_in_display_order)
    }
}

impl TryFrom<ExpandedDifficulty> for Work {
    type Error = ();

    fn try_from(expanded: ExpandedDifficulty) -> Result<Self, Self::Error> {
        // Consensus:
        //
        // <https://zips.z.cash/protocol/protocol.pdf#workdef>
        //
        // We need to compute `2^256 / (expanded + 1)`, but we can't represent
        // 2^256, as it's too large for a u256. However, as 2^256 is at least as
        // large as `expanded + 1`, it is equal to
        // `((2^256 - expanded - 1) / (expanded + 1)) + 1`, or
        let result = (!expanded.0 / (expanded.0 + 1)) + 1;
        if result <= u128::MAX.into() {
            Ok(Work(result.as_u128()))
        } else {
            Err(())
        }
    }
}

impl From<ExpandedDifficulty> for CompactDifficulty {
    fn from(value: ExpandedDifficulty) -> Self {
        value.to_compact()
    }
}

impl BytesInDisplayOrder for ExpandedDifficulty {
    fn bytes_in_serialized_order(&self) -> [u8; 32] {
        self.0.to_big_endian()
    }

    fn from_bytes_in_serialized_order(bytes: [u8; 32]) -> Self {
        ExpandedDifficulty(U256::from_big_endian(&bytes))
    }
}

impl ExpandedDifficulty {
    /// Returns the difficulty of the hash.
    ///
    /// Used to implement comparisons between difficulties and hashes.
    ///
    /// Usage:
    ///
    /// Compare the hash with the calculated difficulty value, using Rust's
    /// standard comparison operators.
    ///
    /// Hashes are not used to calculate the difficulties of future blocks, so
    /// users of this module should avoid converting hashes into difficulties.
    pub(super) fn from_hash(hash: &block::Hash) -> ExpandedDifficulty {
        U256::from_little_endian(&hash.0).into()
    }

    /// Calculate the CompactDifficulty for an expanded difficulty.
    ///
    /// # Consensus
    ///
    /// See `ToCompact()` in the Zcash Specification, and `GetCompact()`
    /// in zcashd:
    /// <https://zips.z.cash/protocol/protocol.pdf#nbits>
    ///
    /// # Panics
    ///
    /// If `self` is zero.
    ///
    /// `ExpandedDifficulty` values are generated in two ways:
    ///   * conversion from `CompactDifficulty` values, which rejects zeroes, and
    ///   * difficulty adjustment calculations, which impose a non-zero minimum
    ///     `target_difficulty_limit`.
    ///
    /// Neither of these methods yield zero values.
    pub fn to_compact(self) -> CompactDifficulty {
        // The zcashd implementation supports negative and zero compact values.
        // These values are rejected by the protocol rules. Zebra is designed so
        // that invalid states are not representable. Therefore, this function
        // does not produce negative compact values, and panics on zero compact
        // values. (The negative compact value code in zcashd is unused.)
        assert!(self.0 > 0.into(), "Zero difficulty values are invalid");

        // The constants for this floating-point representation.
        // Alias the constants here, so the code is easier to read.
        const UNSIGNED_MANTISSA_MASK: u32 = CompactDifficulty::UNSIGNED_MANTISSA_MASK;
        const OFFSET: i32 = CompactDifficulty::OFFSET;

        // Calculate the final size, accounting for the sign bit.
        // This is the size *after* applying the sign bit adjustment in `ToCompact()`.
        let size = self.0.bits() / 8 + 1;

        // Make sure the mantissa is non-negative, by shifting down values that
        // would otherwise overflow into the sign bit
        let mantissa = if self.0 <= UNSIGNED_MANTISSA_MASK.into() {
            // Value is small, shift up if needed
            self.0 << (8 * (3 - size))
        } else {
            // Value is large, shift down
            self.0 >> (8 * (size - 3))
        };

        // This assertion also makes sure that size fits in its 8 bit compact field
        assert!(
            size < (31 + OFFSET) as _,
            "256^size (256^{size}) must fit in a u256, after the sign bit adjustment and offset"
        );
        let size = u32::try_from(size).expect("a 0-6 bit value fits in a u32");

        assert!(
            mantissa <= UNSIGNED_MANTISSA_MASK.into(),
            "mantissa {mantissa:x?} must fit in its compact field"
        );
        let mantissa = u32::try_from(mantissa).expect("a 0-23 bit value fits in a u32");

        if mantissa > 0 {
            CompactDifficulty(mantissa + (size << 24))
        } else {
            // This check catches invalid mantissas. Overflows and underflows
            // should also be unreachable, but they aren't caught here.
            unreachable!("converted CompactDifficulty values must be valid")
        }
    }
}

impl fmt::Display for ExpandedDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for ExpandedDifficulty {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("ExpandedDifficulty")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl ToHex for &ExpandedDifficulty {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for ExpandedDifficulty {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for ExpandedDifficulty {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let bytes_in_display_order = <[u8; 32]>::from_hex(hex)?;

        Ok(ExpandedDifficulty::from_bytes_in_display_order(
            &bytes_in_display_order,
        ))
    }
}

impl From<U256> for ExpandedDifficulty {
    fn from(value: U256) -> Self {
        ExpandedDifficulty(value)
    }
}

impl From<ExpandedDifficulty> for U256 {
    fn from(value: ExpandedDifficulty) -> Self {
        value.0
    }
}

impl Sum<ExpandedDifficulty> for ExpandedDifficulty {
    fn sum<I: Iterator<Item = ExpandedDifficulty>>(iter: I) -> Self {
        iter.map(|d| d.0).fold(U256::zero(), Add::add).into()
    }
}

impl<T> Div<T> for ExpandedDifficulty
where
    T: Into<U256>,
{
    type Output = ExpandedDifficulty;

    fn div(self, rhs: T) -> Self::Output {
        ExpandedDifficulty(self.0 / rhs)
    }
}

impl<T> Mul<T> for ExpandedDifficulty
where
    U256: Mul<T>,
    <U256 as Mul<T>>::Output: Into<U256>,
{
    type Output = ExpandedDifficulty;

    fn mul(self, rhs: T) -> ExpandedDifficulty {
        ExpandedDifficulty((self.0 * rhs).into())
    }
}

impl PartialEq<block::Hash> for ExpandedDifficulty {
    /// Is `self` equal to `other`?
    ///
    /// See `partial_cmp` for details.
    fn eq(&self, other: &block::Hash) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}

impl PartialOrd<block::Hash> for ExpandedDifficulty {
    /// # Consensus
    ///
    /// `block::Hash`es are compared with `ExpandedDifficulty` thresholds by
    /// converting the hash to a 256-bit integer in little-endian order.
    ///
    /// Greater values represent *less* work. This matches the convention in
    /// zcashd and bitcoin.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#workdef>
    fn partial_cmp(&self, other: &block::Hash) -> Option<Ordering> {
        self.partial_cmp(&ExpandedDifficulty::from_hash(other))
    }
}

impl PartialEq<ExpandedDifficulty> for block::Hash {
    /// Is `self` equal to `other`?
    ///
    /// See `<ExpandedDifficulty as PartialOrd<block::Hash>::partial_cmp`
    /// for details.
    fn eq(&self, other: &ExpandedDifficulty) -> bool {
        other.eq(self)
    }
}

impl PartialOrd<ExpandedDifficulty> for block::Hash {
    /// How does `self` compare to `other`?
    ///
    /// # Consensus
    ///
    /// See `<ExpandedDifficulty as PartialOrd<block::Hash>::partial_cmp`
    /// for details.
    #[allow(clippy::unwrap_in_result)]
    fn partial_cmp(&self, other: &ExpandedDifficulty) -> Option<Ordering> {
        Some(
            // Use the canonical implementation, but reverse the order
            other
                .partial_cmp(self)
                .expect("difficulties and hashes have a total order")
                .reverse(),
        )
    }
}

impl std::ops::Add for Work {
    type Output = PartialCumulativeWork;

    fn add(self, rhs: Work) -> PartialCumulativeWork {
        PartialCumulativeWork::from(self) + rhs
    }
}

/// Partial work used to track relative work in non-finalized chains
///
/// # Consensus
///
/// Use to choose the best chain with the most work.
///
/// Since it is only relative values that matter, Zebra uses the partial work from a shared
/// fork root block to find the best chain.
///
/// See [`Work`] for details.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct PartialCumulativeWork(u128);

impl PartialCumulativeWork {
    /// Returns a value representing no work.
    pub fn zero() -> Self {
        Self(0)
    }

    /// Return the inner `u128` value.
    pub fn as_u128(self) -> u128 {
        self.0
    }

    /// Returns a floating-point work multiplier that can be used for display.
    /// The returned value is the work as a multiple of the target difficulty limit for `network`.
    pub fn difficulty_multiplier_for_display(&self, network: Network) -> f64 {
        // This calculation is similar to the `getdifficulty` RPC, see that code for details.

        let pow_limit = network
            .target_difficulty_limit()
            .to_compact()
            .to_work()
            .expect("target difficult limit is valid work");

        // Convert to u128 then f64.
        let pow_limit = pow_limit.as_u128() as f64;
        let work = self.as_u128() as f64;

        work / pow_limit
    }

    /// Returns floating-point work bits that can be used for display.
    /// The returned value is the number of hash bits represented by the work.
    pub fn difficulty_bits_for_display(&self) -> f64 {
        // This calculation is similar to `zcashd`'s bits display in its logs.

        // Convert to u128 then f64.
        let work = self.as_u128() as f64;

        work.log2()
    }
}

/// Network methods related to Difficulty
pub trait ParameterDifficulty {
    /// Returns the easiest target difficulty allowed on `network`.
    ///
    /// # Consensus
    ///
    /// See `PoWLimit` in the Zcash specification:
    /// <https://zips.z.cash/protocol/protocol.pdf#constants>
    fn target_difficulty_limit(&self) -> ExpandedDifficulty;
}

impl ParameterDifficulty for Network {
    /// Returns the easiest target difficulty allowed on `network`.
    /// See [`ParameterDifficulty::target_difficulty_limit`]
    fn target_difficulty_limit(&self) -> ExpandedDifficulty {
        let limit: U256 = match self {
            // Mainnet PoWLimit is defined as `2^243 - 1` on page 73 of the protocol specification:
            // <https://zips.z.cash/protocol/protocol.pdf>
            Network::Mainnet => (U256::one() << 243) - 1,
            // 2^251 - 1 for the default testnet, see `testnet::ParametersBuilder::default`()
            Network::Testnet(params) => return params.target_difficulty_limit(),
        };

        // `zcashd` converts the PoWLimit into a compact representation before
        // using it to perform difficulty filter checks.
        //
        // The Zcash specification converts to compact for the default difficulty
        // filter, but not for testnet minimum difficulty blocks. (ZIP 205 and
        // ZIP 208 don't specify this conversion either.) See #1277 for details.
        ExpandedDifficulty(limit)
            .to_compact()
            .to_expanded()
            .expect("difficulty limits are valid expanded values")
    }
}

impl From<Work> for PartialCumulativeWork {
    fn from(work: Work) -> Self {
        PartialCumulativeWork(work.0)
    }
}

impl std::ops::Add<Work> for PartialCumulativeWork {
    type Output = PartialCumulativeWork;

    fn add(self, rhs: Work) -> Self::Output {
        let result = self
            .0
            .checked_add(rhs.0)
            .expect("Work values do not overflow");

        PartialCumulativeWork(result)
    }
}

impl std::ops::AddAssign<Work> for PartialCumulativeWork {
    fn add_assign(&mut self, rhs: Work) {
        *self = *self + rhs;
    }
}

impl std::ops::Sub<Work> for PartialCumulativeWork {
    type Output = PartialCumulativeWork;

    fn sub(self, rhs: Work) -> Self::Output {
        let result = self.0
            .checked_sub(rhs.0)
            .expect("PartialCumulativeWork values do not underflow: all subtracted Work values must have been previously added to the PartialCumulativeWork");

        PartialCumulativeWork(result)
    }
}

impl std::ops::SubAssign<Work> for PartialCumulativeWork {
    fn sub_assign(&mut self, rhs: Work) {
        *self = *self - rhs;
    }
}
