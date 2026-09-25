//! Constants for block subsidies.

pub(crate) mod mainnet;
pub(crate) mod testnet;

use crate::amount::COIN;
use crate::block::HeightDiff;

/// The largest block subsidy, used before the first halving.
///
/// We use `25 / 2` instead of `12.5`, so that we can calculate the correct value without using floating-point.
/// This calculation is exact, because COIN is divisible by 2, and the division is done last.
pub(crate) const MAX_BLOCK_SUBSIDY: u64 = ((25 * COIN) / 2) as u64;

/// Used as a multiplier to get the new halving interval after Blossom.
///
/// Calculated as `PRE_BLOSSOM_POW_TARGET_SPACING / POST_BLOSSOM_POW_TARGET_SPACING`
/// in the Zcash specification.
pub(crate) const BLOSSOM_POW_TARGET_SPACING_RATIO: u32 = 2;

/// Halving is at about every 4 years, before Blossom block time is 150 seconds.
///
/// `(60 * 60 * 24 * 365 * 4) / 150 = 840960`
pub(crate) const PRE_BLOSSOM_HALVING_INTERVAL: HeightDiff = 840_000;

/// After Blossom the block time is reduced to 75 seconds but halving period should remain around 4 years.
pub(crate) const POST_BLOSSOM_HALVING_INTERVAL: HeightDiff =
    PRE_BLOSSOM_HALVING_INTERVAL * (BLOSSOM_POW_TARGET_SPACING_RATIO as HeightDiff);

/// Used as a multiplier to get the new halving interval after NU7.
///
/// `NU7PoWTargetSpacingRatio` in [ZIP 218], calculated as
/// `POST_BLOSSOM_POW_TARGET_SPACING / POST_NU7_POW_TARGET_SPACING`.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
pub(crate) const NU7_POW_TARGET_SPACING_RATIO: u32 =
    crate::parameters::NU7_POW_TARGET_SPACING_RATIO;

/// After NU7 the block time is reduced to 25 seconds, so the halving interval is tripled again
/// to keep the halving period at around 4 years.
///
/// `PostNU7HalvingInterval` in [ZIP 218].
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
pub(crate) const POST_NU7_HALVING_INTERVAL: HeightDiff =
    POST_BLOSSOM_HALVING_INTERVAL * (NU7_POW_TARGET_SPACING_RATIO as HeightDiff);

/// Denominator as described in [protocol specification §7.10.1][7.10.1].
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
pub(crate) const FUNDING_STREAM_RECEIVER_DENOMINATOR: u64 = 100;

/// The specification for pre-NU6 funding stream receivers, a URL that links to [ZIP-214].
///
/// [ZIP-214]: https://zips.z.cash/zip-0214
pub(crate) const FUNDING_STREAM_SPECIFICATION: &str = "https://zips.z.cash/zip-0214";

/// The specification for post-NU6 funding stream and lockbox receivers, a URL that links to [ZIP-1015].
///
/// [ZIP-1015]: https://zips.z.cash/zip-1015
pub(crate) const LOCKBOX_SPECIFICATION: &str = "https://zips.z.cash/zip-1015";

/// The number of blocks contained in the post-NU6 funding streams height ranges on Mainnet or Testnet, as specified
/// in [ZIP-1015](https://zips.z.cash/zip-1015).
pub(crate) const POST_NU6_FUNDING_STREAM_NUM_BLOCKS: u32 = 420_000;

/// The numerator of the fraction of transaction fees removed from circulation into the NSM
/// reserve from NU7 activation.
///
/// `NSMFeeContribution(height) := floor(6 * TransactionFees(height) / 10)` in the NU7 deployment
/// ZIP, which takes precedence over [ZIP 235] for NU7.
///
/// [ZIP 235]: https://zips.z.cash/zip-0235
pub(crate) const NSM_FEE_NUMERATOR: u64 = 6;

/// The denominator of the fraction of transaction fees removed from circulation into the NSM
/// reserve from NU7 activation.
///
/// See [`NSM_FEE_NUMERATOR`].
pub(crate) const NSM_FEE_DENOMINATOR: u64 = 10;

/// ZIP 237's scaled logarithm, divided by the halving interval to obtain the release rate.
pub(crate) const NSM_LN2_SCALED: u64 = 6_931_680_000;

/// The denominator of the fraction of the NSM reserve reissued in each block.
///
/// See [`NSM_LN2_SCALED`].
pub(crate) const NSM_SUBSIDY_DENOMINATOR: u64 = 10_000_000_000;
