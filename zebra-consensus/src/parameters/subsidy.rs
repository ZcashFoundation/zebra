//! Constants for Block Subsidy, Funding Streams, and Founders' Reward

use zebra_chain::{amount::COIN, block::Height};

/// An initial period from Genesis to this Height where the block subsidy is gradually incremented. [What is slow-start mining][slow-mining]
///
/// [slow-mining]: https://z.cash/support/faq/#what-is-slow-start-mining
pub const SLOW_START_INTERVAL: Height = Height(20_000);

/// `SlowStartShift()` as described in [protocol specification §7.7][7.7]
///
/// [7.7]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// This calculation is exact, because `SLOW_START_INTERVAL` is divisible by 2.
pub const SLOW_START_SHIFT: Height = Height(SLOW_START_INTERVAL.0 / 2);

/// The largest block subsidy, used before the first halving.
///
/// We use `25 / 2` instead of `12.5`, so that we can calculate the correct value without using floating-point.
/// This calculation is exact, because COIN is divisible by 2, and the division is done last.
pub const MAX_BLOCK_SUBSIDY: u64 = ((25 * COIN) / 2) as u64;

/// Used as a multiplier to get the new halving interval after Blossom.
///
/// Calculated as `PRE_BLOSSOM_POW_TARGET_SPACING / POST_BLOSSOM_POW_TARGET_SPACING`
/// in the Zcash specification.
pub const BLOSSOM_POW_TARGET_SPACING_RATIO: u64 = 2;

/// Halving is at about every 4 years, before Blossom block time is 150 seconds.
///
/// `(60 * 60 * 24 * 365 * 4) / 150 = 840960`
pub const PRE_BLOSSOM_HALVING_INTERVAL: Height = Height(840_000);

/// After Blossom the block time is reduced to 75 seconds but halving period should remain around 4 years.
pub const POST_BLOSSOM_HALVING_INTERVAL: Height =
    Height((PRE_BLOSSOM_HALVING_INTERVAL.0 as u64 * BLOSSOM_POW_TARGET_SPACING_RATIO) as u32);

/// The divisor used to calculate the FoundersFraction.
///
/// Derivation: FOUNDERS_FRACTION_DIVISOR = 1/FoundersFraction
///
/// Usage: founders_reward = block_subsidy / FOUNDERS_FRACTION_DIVISOR
pub const FOUNDERS_FRACTION_DIVISOR: u64 = 5;
