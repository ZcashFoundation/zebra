//! Calculations for Block Subsidy and Funding Streams
//!
//! This module contains the consensus parameters which are required for
//! verification.
//!
//! Some consensus parameters change based on network upgrades. Each network
//! upgrade happens at a particular block height. Some parameters have a value
//! (or function) before the upgrade height, at the upgrade height, and after
//! the upgrade height. (For example, the value of the reserved field in the
//! block header during the Heartwood upgrade.)
//!
//! Typically, consensus parameters are accessed via a function that takes a
//! `Network` and `block::Height`.

pub(crate) mod constants;

use std::collections::HashMap;

use crate::{
    amount::{self, Amount, NonNegative},
    block::{Height, HeightDiff},
    parameters::{Network, NetworkUpgrade},
    transparent,
};

#[cfg(zcash_unstable = "zip234alt")]
use crate::amount::MAX_MONEY;

use constants::{
    regtest, testnet, BLOSSOM_POW_TARGET_SPACING_RATIO, FUNDING_STREAM_RECEIVER_DENOMINATOR,
    FUNDING_STREAM_SPECIFICATION, LOCKBOX_SPECIFICATION, MAX_BLOCK_SUBSIDY,
    POST_BLOSSOM_HALVING_INTERVAL, PRE_BLOSSOM_HALVING_INTERVAL,
};

/// The funding stream receiver categories.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FundingStreamReceiver {
    /// The Electric Coin Company (Bootstrap Foundation) funding stream.
    #[serde(rename = "ECC")]
    Ecc,

    /// The Zcash Foundation funding stream.
    ZcashFoundation,

    /// The Major Grants (Zcash Community Grants) funding stream.
    MajorGrants,

    /// The deferred pool contribution, see [ZIP-1015](https://zips.z.cash/zip-1015) for more details.
    Deferred,
}

impl FundingStreamReceiver {
    /// Returns a human-readable name and a specification URL for the receiver, as described in
    /// [ZIP-1014] and [`zcashd`] before NU6. After NU6, the specification is in the [ZIP-1015].
    ///
    /// [ZIP-1014]: https://zips.z.cash/zip-1014#abstract
    /// [`zcashd`]: https://github.com/zcash/zcash/blob/3f09cfa00a3c90336580a127e0096d99e25a38d6/src/consensus/funding.cpp#L13-L32
    /// [ZIP-1015]: https://zips.z.cash/zip-1015
    pub fn info(&self, is_post_nu6: bool) -> (&'static str, &'static str) {
        if is_post_nu6 {
            (
                match self {
                    FundingStreamReceiver::Ecc => "Electric Coin Company",
                    FundingStreamReceiver::ZcashFoundation => "Zcash Foundation",
                    FundingStreamReceiver::MajorGrants => "Zcash Community Grants NU6",
                    FundingStreamReceiver::Deferred => "Lockbox NU6",
                },
                LOCKBOX_SPECIFICATION,
            )
        } else {
            (
                match self {
                    FundingStreamReceiver::Ecc => "Electric Coin Company",
                    FundingStreamReceiver::ZcashFoundation => "Zcash Foundation",
                    FundingStreamReceiver::MajorGrants => "Major Grants",
                    FundingStreamReceiver::Deferred => "Lockbox NU6",
                },
                FUNDING_STREAM_SPECIFICATION,
            )
        }
    }

    /// Returns true if this [`FundingStreamReceiver`] is [`FundingStreamReceiver::Deferred`].
    pub fn is_deferred(&self) -> bool {
        matches!(self, Self::Deferred)
    }
}

/// Funding stream recipients and height ranges.
#[derive(Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct FundingStreams {
    /// Start and end Heights for funding streams
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    height_range: std::ops::Range<Height>,
    /// Funding stream recipients by [`FundingStreamReceiver`].
    recipients: HashMap<FundingStreamReceiver, FundingStreamRecipient>,
}

impl FundingStreams {
    /// Creates a new [`FundingStreams`].
    pub fn new(
        height_range: std::ops::Range<Height>,
        recipients: HashMap<FundingStreamReceiver, FundingStreamRecipient>,
    ) -> Self {
        Self {
            height_range,
            recipients,
        }
    }

    /// Creates a new empty [`FundingStreams`] representing no funding streams.
    pub fn empty() -> Self {
        Self::new(Height::MAX..Height::MAX, HashMap::new())
    }

    /// Returns height range where these [`FundingStreams`] should apply.
    pub fn height_range(&self) -> &std::ops::Range<Height> {
        &self.height_range
    }

    /// Returns recipients of these [`FundingStreams`].
    pub fn recipients(&self) -> &HashMap<FundingStreamReceiver, FundingStreamRecipient> {
        &self.recipients
    }

    /// Returns a recipient with the provided receiver.
    pub fn recipient(&self, receiver: FundingStreamReceiver) -> Option<&FundingStreamRecipient> {
        self.recipients.get(&receiver)
    }

    /// Accepts a target number of addresses that all recipients of this funding stream
    /// except the [`FundingStreamReceiver::Deferred`] receiver should have.
    ///
    /// Extends the addresses for all funding stream recipients by repeating their
    /// existing addresses until reaching the provided target number of addresses.
    pub fn extend_recipient_addresses(&mut self, target_len: usize) {
        for (receiver, recipient) in &mut self.recipients {
            if receiver.is_deferred() {
                continue;
            }

            recipient.extend_addresses(target_len);
        }
    }
}

/// A funding stream recipient as specified in [protocol specification §7.10.1][7.10.1]
///
/// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
#[derive(Deserialize, Clone, Debug, Eq, PartialEq)]
pub struct FundingStreamRecipient {
    /// The numerator for each funding stream receiver category
    /// as described in [protocol specification §7.10.1][7.10.1].
    ///
    /// [7.10.1]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    numerator: u64,
    /// Addresses for the funding stream recipient
    addresses: Vec<transparent::Address>,
}

impl FundingStreamRecipient {
    /// Creates a new [`FundingStreamRecipient`].
    pub fn new<I, T>(numerator: u64, addresses: I) -> Self
    where
        T: ToString,
        I: IntoIterator<Item = T>,
    {
        Self {
            numerator,
            addresses: addresses
                .into_iter()
                .map(|addr| {
                    let addr = addr.to_string();
                    addr.parse()
                        .expect("funding stream address must deserialize")
                })
                .collect(),
        }
    }

    /// Returns the numerator for this funding stream.
    pub fn numerator(&self) -> u64 {
        self.numerator
    }

    /// Returns the receiver of this funding stream.
    pub fn addresses(&self) -> &[transparent::Address] {
        &self.addresses
    }

    /// Accepts a target number of addresses that this recipient should have.
    ///
    /// Extends the addresses for this funding stream recipient by repeating
    /// existing addresses until reaching the provided target number of addresses.
    ///
    /// # Panics
    ///
    /// If there are no recipient addresses.
    pub fn extend_addresses(&mut self, target_len: usize) {
        assert!(
            !self.addresses.is_empty(),
            "cannot extend addresses for empty recipient"
        );

        self.addresses = self
            .addresses
            .iter()
            .cycle()
            .take(target_len)
            .cloned()
            .collect();
    }
}

/// Functionality specific to block subsidy-related consensus rules
pub trait ParameterSubsidy {
    /// Returns the minimum height after the first halving
    /// as described in [protocol specification §7.10][7.10]
    ///
    /// [7.10]: <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
    fn height_for_first_halving(&self) -> Height;

    /// Returns the halving interval after Blossom
    fn post_blossom_halving_interval(&self) -> HeightDiff;

    /// Returns the halving interval before Blossom
    fn pre_blossom_halving_interval(&self) -> HeightDiff;

    /// Returns the address change interval for funding streams
    /// as described in [protocol specification §7.10][7.10].
    ///
    /// > FSRecipientChangeInterval := PostBlossomHalvingInterval / 48
    ///
    /// [7.10]: https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams
    fn funding_stream_address_change_interval(&self) -> HeightDiff;
}

/// Network methods related to Block Subsidy and Funding Streams
impl ParameterSubsidy for Network {
    fn height_for_first_halving(&self) -> Height {
        // First halving on Mainnet is at Canopy
        // while in Testnet is at block constant height of `1_116_000`
        // <https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams>
        match self {
            Network::Mainnet => NetworkUpgrade::Canopy
                .activation_height(self)
                .expect("canopy activation height should be available"),
            Network::Testnet(params) => {
                if params.is_regtest() {
                    regtest::FIRST_HALVING
                } else if params.is_default_testnet() {
                    testnet::FIRST_HALVING
                } else {
                    height_for_halving(1, self).expect("first halving height should be available")
                }
            }
        }
    }

    fn post_blossom_halving_interval(&self) -> HeightDiff {
        match self {
            Network::Mainnet => POST_BLOSSOM_HALVING_INTERVAL,
            Network::Testnet(params) => params.post_blossom_halving_interval(),
        }
    }

    fn pre_blossom_halving_interval(&self) -> HeightDiff {
        match self {
            Network::Mainnet => PRE_BLOSSOM_HALVING_INTERVAL,
            Network::Testnet(params) => params.pre_blossom_halving_interval(),
        }
    }

    fn funding_stream_address_change_interval(&self) -> HeightDiff {
        self.post_blossom_halving_interval() / 48
    }
}

/// Returns the address change period
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn funding_stream_address_period<N: ParameterSubsidy>(height: Height, network: &N) -> u32 {
    // Spec equation: `address_period = floor((height - (height_for_halving(1) - post_blossom_halving_interval))/funding_stream_address_change_interval)`,
    // <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
    //
    // Note that the brackets make it so the post blossom halving interval is added to the total.
    //
    // In Rust, "integer division rounds towards zero":
    // <https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators>
    // This is the same as `floor()`, because these numbers are all positive.

    let height_after_first_halving = height - network.height_for_first_halving();

    let address_period = (height_after_first_halving + network.post_blossom_halving_interval())
        / network.funding_stream_address_change_interval();

    address_period
        .try_into()
        .expect("all values are positive and smaller than the input height")
}

/// The first block height of the halving at the provided halving index for a network.
///
/// See `Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn height_for_halving(halving: u32, network: &Network) -> Option<Height> {
    if halving == 0 {
        return Some(Height(0));
    }

    let slow_start_shift = i64::from(network.slow_start_shift().0);
    let blossom_height = i64::from(NetworkUpgrade::Blossom.activation_height(network)?.0);
    let pre_blossom_halving_interval = network.pre_blossom_halving_interval();
    let halving_index = i64::from(halving);

    let unscaled_height = halving_index.checked_mul(pre_blossom_halving_interval)?;

    let pre_blossom_height = unscaled_height
        .min(blossom_height)
        .checked_add(slow_start_shift)?;

    let post_blossom_height = 0
        .max(unscaled_height - blossom_height)
        .checked_mul(i64::from(BLOSSOM_POW_TARGET_SPACING_RATIO))?
        .checked_add(slow_start_shift)?;

    let height = pre_blossom_height.checked_add(post_blossom_height)?;

    let height = u32::try_from(height).ok()?;
    height.try_into().ok()
}

/// Returns the `fs.Value(height)` for each stream receiver
/// as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn funding_stream_values(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<HashMap<FundingStreamReceiver, Amount<NonNegative>>, amount::Error> {
    let mut results = HashMap::new();

    if expected_block_subsidy.is_zero() {
        return Ok(results);
    }

    if NetworkUpgrade::current(network, height) >= NetworkUpgrade::Canopy {
        let funding_streams = network.funding_streams(height);
        if let Some(funding_streams) = funding_streams {
            for (&receiver, recipient) in funding_streams.recipients() {
                // - Spec equation: `fs.value = floor(block_subsidy(height)*(fs.numerator/fs.denominator))`:
                //   https://zips.z.cash/protocol/protocol.pdf#subsidies
                // - In Rust, "integer division rounds towards zero":
                //   https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators
                //   This is the same as `floor()`, because these numbers are all positive.
                let amount_value = ((expected_block_subsidy * recipient.numerator())?
                    / FUNDING_STREAM_RECEIVER_DENOMINATOR)?;

                results.insert(receiver, amount_value);
            }
        }
    }

    Ok(results)
}

/// Block subsidy errors.
#[derive(thiserror::Error, Clone, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum SubsidyError {
    #[error("no coinbase transaction in block")]
    NoCoinbase,

    #[error("funding stream expected output not found")]
    FundingStreamNotFound,

    #[error("founders reward output not found")]
    FoundersRewardNotFound,

    #[error("one-time lockbox disbursement output not found")]
    OneTimeLockboxDisbursementNotFound,

    #[error("miner fees are invalid")]
    InvalidMinerFees,

    #[error("addition of amounts overflowed")]
    Overflow,

    #[error("subtraction of amounts underflowed")]
    Underflow,

    #[error("unsupported height")]
    UnsupportedHeight,

    #[error("invalid amount")]
    InvalidAmount(#[from] amount::Error),

    #[cfg(zcash_unstable = "zip235")]
    #[error("invalid zip233 amount")]
    InvalidZip233Amount,
}

/// The divisor used for halvings.
///
/// `1 << Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
///
/// Returns `None` if the divisor would overflow a `u64`.
pub fn halving_divisor(height: Height, network: &Network) -> Option<u64> {
    // Some far-future shifts can be more than 63 bits
    1u64.checked_shl(halving(height, network))
}

/// The halving index for a block height and network.
///
/// `Halving(height)`, as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn halving(height: Height, network: &Network) -> u32 {
    let slow_start_shift = network.slow_start_shift();
    let blossom_height = NetworkUpgrade::Blossom
        .activation_height(network)
        .expect("blossom activation height should be available");

    let halving_index = if height < slow_start_shift {
        0
    } else if height < blossom_height {
        let pre_blossom_height = height - slow_start_shift;
        pre_blossom_height / network.pre_blossom_halving_interval()
    } else {
        let pre_blossom_height = blossom_height - slow_start_shift;
        let scaled_pre_blossom_height =
            pre_blossom_height * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO);

        let post_blossom_height = height - blossom_height;

        (scaled_pre_blossom_height + post_blossom_height) / network.post_blossom_halving_interval()
    };

    halving_index
        .try_into()
        .expect("already checked for negatives")
}

#[cfg(all(zcash_unstable = "zip234", zcash_unstable = "zip234alt"))]
compile_error!("`zip234` and `zip234alt` are mutually exclusive");

/// Returns the height at which ZIP-234 applies (lowest height after the second halving following NU7).
///
/// Returns `None` when NU7 isn't configured.
#[cfg(any(zcash_unstable = "zip234", zcash_unstable = "zip234alt"))]
pub fn zip234_start_height(net: &Network) -> Option<Height> {
    let nu7 = NetworkUpgrade::Nu7.activation_height(net)?;
    let target_halving = halving(nu7, net).checked_add(2)?;
    height_for_halving(target_halving, net)
}

/// The halving-based block subsidy at `height` (i.e. ZIP-234 smoothing branches not applied).
#[cfg(zcash_unstable = "zip234alt")]
pub(crate) fn halving_subsidy_at(height: Height, net: &Network) -> Amount<NonNegative> {
    let Some(halving_div) = halving_divisor(height, net) else {
        return Amount::zero();
    };
    let slow_start_interval = net.slow_start_interval();
    let amount = if height < slow_start_interval {
        let slow_start_rate = MAX_BLOCK_SUBSIDY / u64::from(slow_start_interval);
        if height < net.slow_start_shift() {
            slow_start_rate * u64::from(height)
        } else {
            slow_start_rate * (u64::from(height) + 1)
        }
    } else {
        let base = if NetworkUpgrade::current(net, height) < NetworkUpgrade::Blossom {
            MAX_BLOCK_SUBSIDY
        } else {
            MAX_BLOCK_SUBSIDY / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO)
        };
        base / halving_div
    };
    Amount::try_from(amount).expect("halving subsidy is non-negative and bounded by MAX_BLOCK_SUBSIDY")
}

/// Cumulative halving-only block subsidies issued for blocks 1..=`height`.
/// Used by `zip234alt` to compute the burned-and-unrecycled deficit as `expected_supply − actual_supply`.
#[cfg(zcash_unstable = "zip234alt")]
pub fn cumulative_halving_subsidies(net: &Network, height: Height) -> Amount<NonNegative> {
    let h = u64::from(height.0);
    let ss_shift = u64::from(net.slow_start_shift().0);
    let ss_interval = u64::from(net.slow_start_interval().0);
    let blossom = u64::from(
        NetworkUpgrade::Blossom
            .activation_height(net)
            .expect("blossom activation height should be available")
            .0,
    );
    let ss_rate = MAX_BLOCK_SUBSIDY / ss_interval;

    let mut total: u128 = 0;

    // Slow-start phase 1
    let s1_end = h.min(ss_shift);
    if s1_end >= 2 {
        let n = s1_end - 1;
        total += u128::from(n) * u128::from(n + 1) / 2 * u128::from(ss_rate);
    }

    // Slow-start phase 2
    if h > ss_shift {
        let e = h.min(ss_interval);
        let sum_to_e = u128::from(e) * u128::from(e + 1) / 2;
        let sum_below = u128::from(ss_shift) * u128::from(ss_shift + 1) / 2;
        total += (sum_to_e - sum_below) * u128::from(ss_rate);
    }

    // Halving eras
    if h > ss_interval {
        let mut era: u32 = 0;
        loop {
            let era_start = u64::from(height_for_halving(era, net).expect("era start").0);
            if era_start >= h {
                break;
            }
            let next_era_start =
                u64::from(height_for_halving(era + 1, net).unwrap_or(Height::MAX).0);
            let era_end = next_era_start.min(h);
            let clipped_start = era_start.max(ss_interval);
            if clipped_start >= era_end {
                era += 1;
                continue;
            }

            // pre-blossom slice
            if clipped_start < blossom {
                let pre_end = era_end.min(blossom);
                let blocks = u128::from(pre_end - clipped_start);
                let subsidy = MAX_BLOCK_SUBSIDY >> era;
                total += blocks * u128::from(subsidy);
            }

            // post-blossom slice
            if era_end > blossom {
                let post_start = clipped_start.max(blossom);
                let blocks = u128::from(era_end - post_start);
                let subsidy = (MAX_BLOCK_SUBSIDY / 2) >> era;
                if subsidy == 0 {
                    break;
                }
                total += blocks * u128::from(subsidy);
            }

            era += 1;
        }
    }

    let total_i64 = i64::try_from(total).expect("cumulative subsidies fit in i64");
    total_i64
        .try_into()
        .expect("cumulative subsidies are ≤ MAX_MONEY")
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
/// and [ZIP-234] for post-activation smoothed issuance.
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
/// [ZIP-234]: https://zips.z.cash/zip-0234
pub fn block_subsidy(
    height: Height,
    net: &Network,
    money_reserve: Option<Amount<NonNegative>>,
) -> Result<Amount<NonNegative>, SubsidyError> {
    #[cfg(zcash_unstable = "zip234")]
    // ZIP-234 smoothed subsidy
    //
    if let Some(start) = zip234_start_height(net) {
        if height >= start {
            const NUMERATOR: u128 = 4_126;
            const DENOMINATOR: u128 = 10_000_000_000;

            let money_reserve = money_reserve
                .expect("money_reserve is required at post-activation heights");
            let reserve = u128::from(
                u64::try_from(i64::from(money_reserve))
                    .expect("money_reserve is Amount<NonNegative> so its i64 is >= 0"),
            );
            let subsidy_u128 = reserve
                .checked_mul(NUMERATOR)
                .expect("reserve <= MAX_MONEY ~ 2.1e15, * 4126 ~ 8.7e18 fits in u128")
                .div_ceil(DENOMINATOR);

            let subsidy = i64::try_from(subsidy_u128)
                .expect("subsidy <= MAX_MONEY * 4126 / 10^10 ~ 8.66e8 zat, fits in i64");
            return Ok(subsidy
                .try_into()
                .expect("subsidy is non-negative and <= MAX_MONEY"));
        }
    }

    // ZIP-234 (alt) halving-based subsidy
    //
    #[cfg(zcash_unstable = "zip234alt")]
    if let Some(start) = zip234_start_height(net) {
        if height >= start {
            const NUMERATOR: u128 = 4_126;
            const DENOMINATOR: u128 = 10_000_000_000;

            let money_reserve = money_reserve
                .expect("money_reserve is required at post-activation heights");

            // Halving subsidy at this height (the base).
            let halving = halving_subsidy_at(height, net);

            // deficit = expected_supply(parent) − actual_supply(parent)
            //         = cumulative_halvings(h-1) − (MAX_MONEY − money_reserve)
            let parent = height.previous().unwrap_or(Height(0));
            let expected_supply: i64 =
                cumulative_halving_subsidies(net, parent).into();
            let actual_supply: i64 =
                (MAX_MONEY as i64) - i64::from(money_reserve);
            let deficit = expected_supply.saturating_sub(actual_supply).max(0);

            let bonus_u128 = u128::from(u64::try_from(deficit).expect("deficit non-negative"))
                .checked_mul(NUMERATOR)
                .expect("deficit ≤ MAX_MONEY, * 4126 fits in u128")
                .div_ceil(DENOMINATOR);
            let bonus = i64::try_from(bonus_u128).expect("bonus fits in i64");
            let bonus: Amount<NonNegative> = bonus
                .try_into()
                .expect("bonus is non-negative and bounded by deficit");

            return Ok((halving + bonus)?);
        }
    }

    // Pre ZIP-234 subsidy calculation
    //
    let Some(halving_div) = halving_divisor(height, net) else {
        return Ok(Amount::zero());
    };

    let slow_start_interval = net.slow_start_interval();

    // The `floor` fn used in the spec is implicit in Rust's division of primitive integer types.

    let amount = if height < slow_start_interval {
        let slow_start_rate = MAX_BLOCK_SUBSIDY / u64::from(slow_start_interval);

        if height < net.slow_start_shift() {
            slow_start_rate * u64::from(height)
        } else {
            slow_start_rate * (u64::from(height) + 1)
        }
    } else {
        let base_subsidy = if NetworkUpgrade::current(net, height) < NetworkUpgrade::Blossom {
            MAX_BLOCK_SUBSIDY
        } else {
            MAX_BLOCK_SUBSIDY / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO)
        };

        base_subsidy / halving_div
    };

    Ok(Amount::try_from(amount)?)
}

/// `MinerSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn miner_subsidy(
    height: Height,
    network: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, amount::Error> {
    let founders_reward = founders_reward(network, height);

    let funding_streams_sum = funding_stream_values(height, network, expected_block_subsidy)?
        .values()
        .sum::<Result<Amount<NonNegative>, _>>()?;

    expected_block_subsidy - founders_reward - funding_streams_sum
}

/// Returns the founders reward address for a given height and network as described in [§7.9].
///
/// [§7.9]: <https://zips.z.cash/protocol/protocol.pdf#foundersreward>
pub fn founders_reward_address(net: &Network, height: Height) -> Option<transparent::Address> {
    let founders_address_list = net.founder_address_list();
    let num_founder_addresses = u32::try_from(founders_address_list.len()).ok()?;
    let slow_start_shift = u32::from(net.slow_start_shift());
    let pre_blossom_halving_interval = u32::try_from(net.pre_blossom_halving_interval()).ok()?;

    let founder_address_change_interval = slow_start_shift
        .checked_add(pre_blossom_halving_interval)?
        .div_ceil(num_founder_addresses);

    let founder_address_adjusted_height =
        if NetworkUpgrade::current(net, height) < NetworkUpgrade::Blossom {
            u32::from(height)
        } else {
            NetworkUpgrade::Blossom
                .activation_height(net)
                .and_then(|h| {
                    let blossom_activation_height = u32::from(h);
                    let height = u32::from(height);

                    blossom_activation_height.checked_add(
                        height.checked_sub(blossom_activation_height)?
                            / BLOSSOM_POW_TARGET_SPACING_RATIO,
                    )
                })?
        };

    let founder_address_index =
        usize::try_from(founder_address_adjusted_height / founder_address_change_interval).ok()?;

    founders_address_list
        .get(founder_address_index)
        .and_then(|a| a.parse().ok())
}

/// `FoundersReward(height)` as described in [§7.8].
///
/// [§7.8]: <https://zips.z.cash/protocol/protocol.pdf#subsidies>
pub fn founders_reward(net: &Network, height: Height) -> Amount<NonNegative> {
    // The founders reward is 20% of the block subsidy before the first halving, and 0 afterwards.
    //
    // On custom testnets, the first halving can occur later than Canopy, which causes an
    // inconsistency in the definition of the founders reward, which should occur only before
    // Canopy, so we check if Canopy is active as well.
    if halving(height, net) < 1 && NetworkUpgrade::current(net, height) < NetworkUpgrade::Canopy {
        block_subsidy(height, net, None)
            .map(|subsidy| subsidy.div_exact(5))
            .expect("block subsidy must be valid for founders rewards")
    } else {
        Amount::zero()
    }
}
