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

mod check;

use std::collections::HashMap;

use crate::{
    amount::{self, Amount, NonNegative},
    block::{Height, HeightDiff},
    parameters::{Network, NetworkUpgrade},
    transparent,
    value_balance::ValueBalance,
};

#[cfg(zcash_unstable = "zip234")]
use crate::amount::NegativeAllowed;

pub use check::{miner_fees_are_valid, subsidy_is_valid, CoinbaseTransactionError};

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

/// Returns the position in the address slice for each funding stream
/// as described in [protocol specification §7.10][7.10]
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
fn funding_stream_address_index(
    height: Height,
    network: &Network,
    receiver: FundingStreamReceiver,
) -> Option<usize> {
    if receiver == FundingStreamReceiver::Deferred {
        return None;
    }

    let funding_streams = network.funding_streams(height)?;
    let num_addresses = funding_streams.recipient(receiver)?.addresses().len();

    let index = 1u32
        .checked_add(funding_stream_address_period(height, network))?
        .checked_sub(funding_stream_address_period(
            funding_streams.height_range().start,
            network,
        ))? as usize;

    assert!(index > 0 && index <= num_addresses);
    // spec formula will output an index starting at 1 but
    // Zebra indices for addresses start at zero, return converted.
    Some(index - 1)
}

/// Return the address corresponding to given height, network and funding stream receiver.
///
/// This function only returns transparent addresses, because the current Zcash funding streams
/// only use transparent addresses,
pub fn funding_stream_address(
    height: Height,
    network: &Network,
    receiver: FundingStreamReceiver,
) -> Option<&transparent::Address> {
    let index = funding_stream_address_index(height, network, receiver)?;
    let funding_streams = network.funding_streams(height)?;
    funding_streams.recipient(receiver)?.addresses().get(index)
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

    #[cfg(zcash_unstable = "zip234")]
    #[error(
        "the block subsidy at height {0:?} depends on the chain value pools after its parent block"
    )]
    ParentChainValuePoolsRequired(Height),
}

/// `INITIAL_NSM_VALUE_BALANCE` on Mainnet: the block subsidy and fees left unclaimed by coinbase
/// transactions before NU6, which seeds the NSM value balance at NU7 activation under the
/// [halving-preserving issuance ZIP][zip].
///
/// Since [ZIP 236] made claiming the full coinbase value a consensus rule, this is the fixed amount
/// `sum(ScheduledBlockSubsidy(0..=H)) - IssuedSupply(H)` for any `H` from the last pre-NU6 height
/// (2,726,399) up to NU7 activation.
///
/// TODO: provisional; measure on a synced node (see the ZIP's Parameters).
///
/// [zip]: https://github.com/zcash/zips/pull/1354
/// [ZIP 236]: https://zips.z.cash/zip-0236
#[cfg(zcash_unstable = "zip234")]
pub const MAINNET_INITIAL_NSM_VALUE_BALANCE: u64 = 35_080_000_000;

impl Network {
    /// `INITIAL_NSM_VALUE_BALANCE`: the NSM value balance seeded at NU7 activation under the
    /// [halving-preserving issuance ZIP][zip].
    ///
    /// Mainnet uses [`MAINNET_INITIAL_NSM_VALUE_BALANCE`]. Configured Testnets and Regtest can set
    /// their own value, and default to zero.
    ///
    /// TODO: the default Testnet's value is the same unclaimed pre-NU6 amount, measured at its
    /// last pre-NU6 height (2,975,999), and is not known yet, so it is zero for now.
    ///
    /// [zip]: https://github.com/zcash/zips/pull/1354
    #[cfg(zcash_unstable = "zip234")]
    pub fn initial_nsm_value_balance(&self) -> Amount<NonNegative> {
        match self {
            Network::Mainnet => Amount::try_from(MAINNET_INITIAL_NSM_VALUE_BALANCE)
                .expect("hard-coded initial NSM value balance should be valid"),
            Network::Testnet(params) => params.initial_nsm_value_balance(),
        }
    }

    /// `DEPLOYMENT_BLOCK_HEIGHT`: the height from which the [halving-preserving issuance ZIP][zip]
    /// reissues the NSM value balance, if this network has one.
    ///
    /// The NU7 deployment ZIP assigns it for Mainnet and the default Testnet, and hasn't yet, so
    /// they have none. Configured Testnets and Regtest use their configured
    /// `zip234_deployment_height`, or their NU7 activation height if none is configured.
    ///
    /// [zip]: https://github.com/zcash/zips/pull/1354
    #[cfg(zcash_unstable = "zip234")]
    pub fn zip234_deployment_height(&self) -> Option<Height> {
        match self {
            Network::Mainnet => None,
            Network::Testnet(_) if self.is_default_testnet() => None,
            Network::Testnet(params) => params.zip234_deployment_height(),
        }
    }
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

/// `10^10 * ln 2`, rounded so that Mainnet's `PostBlossomHalvingInterval` of 1,680,000 blocks
/// gives a `BLOCK_SUBSIDY_FRACTION` numerator of exactly 4126.
///
/// The [halving-preserving issuance ZIP][zip] states `BLOCK_SUBSIDY_FRACTION` as a function of the
/// halving schedule, so that the NSM value balance's half-life is one halving period whatever the
/// target spacing is. With ZIP 218's `PostNU7HalvingInterval` of 5,040,000 blocks the numerator is
/// 1375, with no separate constant.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub const LN2_SCALED: u64 = 6_931_680_000;

/// The denominator of `BLOCK_SUBSIDY_FRACTION` in the [halving-preserving issuance ZIP][zip].
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub const BLOCK_SUBSIDY_FRACTION_DENOMINATOR: u64 = 10_000_000_000;

/// `HalvingInterval(height)`: the number of blocks in the halving period containing `height`.
///
/// The [halving-preserving issuance ZIP][zip] defines this as `PostNU7HalvingInterval` where
/// ZIP 218's 25-second blocks are active, and `PostBlossomHalvingInterval` otherwise.
///
/// TODO: Zebra doesn't implement ZIP 218 yet, so this is always `PostBlossomHalvingInterval`.
/// When it does, return its post-NU7 halving interval from NU7 activation, otherwise the
/// reissuance fraction would be three times too large under 25-second blocks.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub fn halving_interval(_height: Height, network: &Network) -> HeightDiff {
    network.post_blossom_halving_interval()
}

/// The numerator of `BLOCK_SUBSIDY_FRACTION(height)` on `network`:
/// `LN2_SCALED / HalvingInterval(height)`.
///
/// Over one halving period this reissues about half the NSM value balance, because
/// `(1 - ln 2 / n)^n` approaches `1/2` for large `n`.
///
/// The result is non-zero for any halving interval up to `Height::MAX`.
#[cfg(zcash_unstable = "zip234")]
pub fn block_subsidy_fraction_numerator(height: Height, network: &Network) -> u64 {
    let halving_interval =
        u64::try_from(halving_interval(height, network)).expect("halving intervals are positive");

    LN2_SCALED / halving_interval
}

#[cfg(all(zcash_unstable = "zip235", not(zcash_unstable = "zip234")))]
compile_error!(
    "`--cfg zcash_unstable=\"zip235\"` requires `--cfg zcash_unstable=\"zip234\"`: the fees ZIP 235 \
     removes from circulation are credited to the NSM value balance"
);

/// `NSMFeeContribution(height)`: the transaction fees the block at `height` removes from
/// circulation, `floor(block_miner_fees * 6 / 10)` from NU7 activation, and zero before it. This
/// is the fraction from [ZIP 235], applied the way the [NU7 deployment ZIP][nu7] specifies.
///
/// The fraction applies to the total fees in the block, so rounding favors the miner. The
/// coinbase transaction can only claim the remaining fees, and the contribution needs no
/// transaction field.
///
/// Always zero without `--cfg zcash_unstable="zip235"`, which requires the `zip234` cfg, because
/// the contribution is credited to the NSM value balance.
///
/// [ZIP 235]: https://zips.z.cash/zip-0235
/// [nu7]: https://github.com/zcash/zips/pull/1363
pub fn nsm_fee_contribution(
    height: Height,
    network: &Network,
    block_miner_fees: Amount<NonNegative>,
) -> Amount<NonNegative> {
    if !cfg!(zcash_unstable = "zip235") || !nsm_value_balance_is_tracked(height, network) {
        return Amount::zero();
    }

    let fees = u64::from(block_miner_fees);

    // Splitting at the divisor keeps the exact floor without overflowing `fees * 6`.
    (fees / 10 * 6 + fees % 10 * 6 / 10)
        .try_into()
        .expect("the contribution is at most the fees, which are a valid amount")
}

/// Returns `true` if the NSM value balance is tracked at `height`: from NU7 activation, per the
/// [halving-preserving issuance ZIP][zip].
///
/// [zip]: https://github.com/zcash/zips/pull/1354
pub fn nsm_value_balance_is_tracked(height: Height, network: &Network) -> bool {
    NetworkUpgrade::Nu7
        .activation_height(network)
        .is_some_and(|activation_height| height >= activation_height)
}

/// Returns `true` if the [halving-preserving issuance ZIP][zip] reissues the NSM value balance at
/// `height`: from `DEPLOYMENT_BLOCK_HEIGHT`, which is at or after NU7 activation.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub fn zip234_reissuance_is_active(height: Height, network: &Network) -> bool {
    network
        .zip234_deployment_height()
        .is_some_and(|deployment_height| height >= deployment_height)
}

/// The value the block at `height` seeds into the NSM value balance: `INITIAL_NSM_VALUE_BALANCE`
/// at the NU7 activation block, and zero otherwise.
#[cfg(zcash_unstable = "zip234")]
fn nsm_seed(height: Height, network: &Network) -> Amount<NonNegative> {
    if Some(height) == NetworkUpgrade::Nu7.activation_height(network) {
        network.initial_nsm_value_balance()
    } else {
        Amount::zero()
    }
}

/// `NSMValueBalance(height - 1)`: the Network Sustainability Mechanism value balance the block at
/// `height` reissues from, where `parent_nsm_value_balance` is the balance after its parent block.
///
/// The [halving-preserving issuance ZIP][zip] defines
/// `NSMValueBalance(NU7ActivationHeight - 1)` as `INITIAL_NSM_VALUE_BALANCE`, the subsidy and fees
/// left unclaimed before NU6, so the NU7 activation block reissues from that seed.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub fn nsm_value_balance_before(
    height: Height,
    network: &Network,
    parent_nsm_value_balance: Amount<NonNegative>,
) -> Amount<NonNegative> {
    (parent_nsm_value_balance + nsm_seed(height, network))
        .expect("the balance is zero before NU7 activation, and the seed is zero after it")
}

/// The change the block at `height` makes to the Network Sustainability Mechanism value balance,
/// where `parent_chain_value_pools` are the chain value pools after its parent block.
///
/// The balance is credited at the NU7 activation block with `INITIAL_NSM_VALUE_BALANCE`, and
/// debited by the additional block subsidy, which is zero before `DEPLOYMENT_BLOCK_HEIGHT`. Before
/// NU7 activation the change is zero.
///
/// It is also credited with [`nsm_fee_contribution`], the part of `block_miner_fees` that ZIP 235
/// removes from circulation.
#[cfg(zcash_unstable = "zip234")]
pub fn nsm_value_balance_change(
    height: Height,
    network: &Network,
    parent_chain_value_pools: ValueBalance<NonNegative>,
    block_miner_fees: Amount<NonNegative>,
) -> Result<Amount<NegativeAllowed>, amount::Error> {
    if !nsm_value_balance_is_tracked(height, network) {
        return Ok(Amount::zero());
    }

    let seed = nsm_seed(height, network).constrain::<NegativeAllowed>()?;
    let contributed =
        nsm_fee_contribution(height, network, block_miner_fees).constrain::<NegativeAllowed>()?;
    let reissued = additional_block_subsidy(
        height,
        network,
        nsm_value_balance_before(height, network, parent_chain_value_pools.nsm_amount()),
    )
    .constrain::<NegativeAllowed>()?;

    // The seed and the contribution can each be up to `MAX_MONEY`, so subtract first.
    (seed - reissued)? + contributed
}

/// `AdditionalBlockSubsidy(height)`: the block subsidy added to the scheduled block subsidy by the
/// [halving-preserving issuance ZIP][zip], `ceiling(BLOCK_SUBSIDY_FRACTION * nsm_value_balance)`
/// from `DEPLOYMENT_BLOCK_HEIGHT`, and zero before it.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
pub fn additional_block_subsidy(
    height: Height,
    network: &Network,
    nsm_value_balance: Amount<NonNegative>,
) -> Amount<NonNegative> {
    if !zip234_reissuance_is_active(height, network) {
        return Amount::zero();
    }

    let additional_block_subsidy = (u128::from(u64::from(nsm_value_balance))
        * u128::from(block_subsidy_fraction_numerator(height, network)))
    .div_ceil(u128::from(BLOCK_SUBSIDY_FRACTION_DENOMINATOR));

    u64::try_from(additional_block_subsidy)
        .ok()
        .and_then(|additional_block_subsidy| Amount::try_from(additional_block_subsidy).ok())
        .expect("BLOCK_SUBSIDY_FRACTION is less than 1, so the result is at most the balance")
}

/// `BlockSubsidy(height)` for a block whose parent block leaves `parent_chain_value_pools` in the
/// chain value pools.
///
/// From `DEPLOYMENT_BLOCK_HEIGHT`, this is
/// `ScheduledBlockSubsidy(height) + ceiling(BLOCK_SUBSIDY_FRACTION * NSMValueBalance(height - 1))`.
/// Before that, `parent_chain_value_pools` is unused and this is [`scheduled_block_subsidy`].
///
/// [zip]: https://github.com/zcash/zips/pull/1354
pub fn block_subsidy_with_parent_pools(
    height: Height,
    net: &Network,
    parent_chain_value_pools: ValueBalance<NonNegative>,
) -> Result<Amount<NonNegative>, SubsidyError> {
    #[cfg(zcash_unstable = "zip234")]
    return block_subsidy_with_parent_nsm_value_balance(
        height,
        net,
        parent_chain_value_pools.nsm_amount(),
    );

    #[cfg(not(zcash_unstable = "zip234"))]
    {
        let _ = parent_chain_value_pools;
        scheduled_block_subsidy(height, net)
    }
}

/// `BlockSubsidy(height)` for a block whose parent block leaves `parent_nsm_value_balance` in the
/// NSM value balance. See [`block_subsidy_with_parent_pools`].
#[cfg(zcash_unstable = "zip234")]
pub fn block_subsidy_with_parent_nsm_value_balance(
    height: Height,
    net: &Network,
    parent_nsm_value_balance: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, SubsidyError> {
    let scheduled_block_subsidy = scheduled_block_subsidy(height, net)?;

    if !zip234_reissuance_is_active(height, net) {
        return Ok(scheduled_block_subsidy);
    }

    let nsm_value_balance = nsm_value_balance_before(height, net, parent_nsm_value_balance);

    Ok((scheduled_block_subsidy + additional_block_subsidy(height, net, nsm_value_balance))?)
}

/// `BlockSubsidy(height)` as described in [protocol specification §7.8][7.8]
///
/// With `zcash_unstable = "zip234"`, this returns [`SubsidyError::ParentChainValuePoolsRequired`]
/// once the [halving-preserving issuance ZIP][zip] reissues, because the subsidy then depends on
/// the parent block's NSM value balance. Use [`block_subsidy_with_parent_pools`] there.
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
/// [zip]: https://github.com/zcash/zips/pull/1354
pub fn block_subsidy(height: Height, net: &Network) -> Result<Amount<NonNegative>, SubsidyError> {
    #[cfg(zcash_unstable = "zip234")]
    if zip234_reissuance_is_active(height, net) {
        return Err(SubsidyError::ParentChainValuePoolsRequired(height));
    }

    scheduled_block_subsidy(height, net)
}

/// `ScheduledBlockSubsidy(height)`: the block subsidy under the halving schedule, as described in
/// [protocol specification §7.8][7.8] for `BlockSubsidy(height)`.
///
/// [7.8]: https://zips.z.cash/protocol/protocol.pdf#subsidies
pub fn scheduled_block_subsidy(
    height: Height,
    net: &Network,
) -> Result<Amount<NonNegative>, SubsidyError> {
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
        block_subsidy(height, net)
            .map(|subsidy| subsidy.div_exact(5))
            .expect("block subsidy must be valid for founders rewards")
    } else {
        Amount::zero()
    }
}
