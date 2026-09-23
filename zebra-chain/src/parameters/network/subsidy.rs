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

#[cfg(test)]
mod tests;

use mset::MultiSet;
use std::collections::HashMap;

use crate::{
    amount::{self, Amount, DeferredPoolBalanceChange, NegativeAllowed, NonNegative},
    block::{Block, Height, HeightDiff},
    parameters::{Network, NetworkUpgrade},
    transaction::Transaction,
    transparent::{self, Address, Output},
};

use constants::{
    BLOSSOM_POW_TARGET_SPACING_RATIO, FUNDING_STREAM_RECEIVER_DENOMINATOR,
    FUNDING_STREAM_SPECIFICATION, LOCKBOX_SPECIFICATION, MAX_BLOCK_SUBSIDY, NSM_FEE_DENOMINATOR,
    NSM_FEE_NUMERATOR, NSM_LN2_SCALED, NSM_SUBSIDY_DENOMINATOR, NU7_POW_TARGET_SPACING_RATIO,
    POST_BLOSSOM_HALVING_INTERVAL, POST_NU7_HALVING_INTERVAL, PRE_BLOSSOM_HALVING_INTERVAL,
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

    /// Returns the halving interval after NU7.
    ///
    /// `PostNU7HalvingInterval` in [ZIP 218].
    ///
    /// [ZIP 218]: https://zips.z.cash/zip-0218
    fn post_nu7_halving_interval(&self) -> HeightDiff;

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
        // Derived rather than hard-coded, so that ZIP 218's stretched halving schedule applies
        // here too and this can not disagree with `halving()`.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#zip214fundingstreams>
        height_for_halving(1, self).expect("the first halving height is representable")
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

    fn post_nu7_halving_interval(&self) -> HeightDiff {
        match self {
            Network::Mainnet => POST_NU7_HALVING_INTERVAL,
            Network::Testnet(params) => {
                params.post_blossom_halving_interval()
                    * HeightDiff::from(NU7_POW_TARGET_SPACING_RATIO)
            }
        }
    }

    fn funding_stream_address_change_interval(&self) -> HeightDiff {
        self.post_blossom_halving_interval() / 48
    }
}

/// Returns the address change period
/// as described in [protocol specification §7.10][7.10]
///
/// The result is signed, and callers only ever use the *difference* between two periods. It can
/// be negative below the first funding stream: on a configured Testnet whose upgrades are packed
/// into a few hundred blocks, ZIP 218 pushes the first halving above every height the network
/// reaches once NU7 activates before it. Clamping a negative period to zero would silently
/// collapse those differences, so the sign is preserved here and the conversion is left to the
/// callers, which know the range they are counting over.
///
/// [7.10]: https://zips.z.cash/protocol/protocol.pdf#fundingstreams
pub fn funding_stream_address_period<N: ParameterSubsidy>(
    height: Height,
    network: &N,
) -> HeightDiff {
    // Spec equation: `address_period = floor((height - (height_for_halving(1) - post_blossom_halving_interval))/funding_stream_address_change_interval)`,
    // <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
    //
    // Note that the brackets make it so the post blossom halving interval is added to the total.
    //
    // In Rust, "integer division rounds towards zero":
    // <https://doc.rust-lang.org/stable/reference/expressions/operator-expr.html#arithmetic-and-logical-binary-operators>
    // which is not `floor()` for a negative numerator, so `div_euclid` is used instead. The
    // divisor is positive, so `div_euclid` is exactly the spec's `floor()`. Truncating would
    // merge the two spec periods either side of zero, and both callers subtract one period from
    // another, so that would shift every later period down by one.

    let height_after_first_halving = height - network.height_for_first_halving();

    (height_after_first_halving + network.post_blossom_halving_interval())
        .div_euclid(network.funding_stream_address_change_interval())
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

    // The two periods are only meaningful relative to each other, so the subtraction is done in
    // signed arithmetic: see `funding_stream_address_period()`.
    let index = usize::try_from(
        1 + funding_stream_address_period(height, network)
            - funding_stream_address_period(funding_streams.height_range().start, network),
    )
    .ok()?;

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

    // ZIP 218: once NU7 is active, the blocks after its activation height are three times as
    // frequent, so the remaining blocks of this halving are stretched by the same ratio.
    let height = match NetworkUpgrade::Nu7.activation_height(network) {
        Some(nu7_height) if height > i64::from(nu7_height.0) => {
            let nu7_height = i64::from(nu7_height.0);

            (height - nu7_height)
                .checked_mul(i64::from(NU7_POW_TARGET_SPACING_RATIO))?
                .checked_add(nu7_height)?
        }
        _ => height,
    };

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

    #[error("{0}")]
    Other(String),

    #[error("invalid amount")]
    InvalidAmount(#[from] amount::Error),
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

    // `NetworkUpgrade::activation_height()` falls back to the next upgrade's height, so this is
    // `None` exactly when no upgrade from NU7 onward is scheduled on `network`.
    let nu7_height = NetworkUpgrade::Nu7.activation_height(network);

    let halving_index = if height < slow_start_shift {
        0
    } else if height < blossom_height {
        let pre_blossom_height = height - slow_start_shift;
        pre_blossom_height / network.pre_blossom_halving_interval()
    } else if nu7_height.is_none_or(|nu7_height| height < nu7_height) {
        let pre_blossom_height = blossom_height - slow_start_shift;
        let scaled_pre_blossom_height =
            pre_blossom_height * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO);

        let post_blossom_height = height - blossom_height;

        (scaled_pre_blossom_height + post_blossom_height) / network.post_blossom_halving_interval()
    } else {
        // ZIP 218 adds a third era to `Halving()`. Each era is scaled into post-NU7 blocks: a
        // pre-Blossom block is worth `BlossomPoWTargetSpacingRatio * NU7PoWTargetSpacingRatio`
        // of them, and a post-Blossom pre-NU7 block is worth `NU7PoWTargetSpacingRatio`.
        let nu7_height = nu7_height.expect("checked by the branch condition above");

        let scaled_pre_blossom_height = (blossom_height - slow_start_shift)
            * HeightDiff::from(BLOSSOM_POW_TARGET_SPACING_RATIO)
            * HeightDiff::from(NU7_POW_TARGET_SPACING_RATIO);

        let scaled_post_blossom_height =
            (nu7_height - blossom_height) * HeightDiff::from(NU7_POW_TARGET_SPACING_RATIO);

        let post_nu7_height = height - nu7_height;

        (scaled_pre_blossom_height + scaled_post_blossom_height + post_nu7_height)
            / network.post_nu7_halving_interval()
    };

    halving_index
        .try_into()
        .expect("already checked for negatives")
}

/// Total scheduled and reserve-funded block subsidy, using the exact parent's NSM reserve.
pub fn block_subsidy(
    height: Height,
    net: &Network,
    previous_nsm_reserve: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, SubsidyError> {
    Ok((scheduled_block_subsidy(height, net)? + nsm_subsidy(height, net, previous_nsm_reserve)?)?)
}

/// Scheduled issuance, excluding NSM reissuance.
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
        // Nested integer divisions by `base_subsidy` then `halving_div` give the same result as
        // the spec's single `floor()` over their product, because both divisors are positive.
        let nu = NetworkUpgrade::current(net, height);
        let base_subsidy = if nu < NetworkUpgrade::Blossom {
            MAX_BLOCK_SUBSIDY
        } else if nu < NetworkUpgrade::Nu7 {
            MAX_BLOCK_SUBSIDY / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO)
        } else {
            // ZIP 218 divides the subsidy by a further `NU7PoWTargetSpacingRatio`, so that the
            // issuance per unit of wall clock time is unchanged by the faster block spacing.
            MAX_BLOCK_SUBSIDY
                / u64::from(BLOSSOM_POW_TARGET_SPACING_RATIO)
                / u64::from(NU7_POW_TARGET_SPACING_RATIO)
        };

        base_subsidy / halving_div
    };

    Ok(Amount::try_from(amount)?)
}

/// Cumulative scheduled issuance through `height`, including the scheduled genesis subsidy.
///
/// Slow start is summed arithmetically; later constant-subsidy ranges are bounded by spacing
/// changes and halvings. The inverse halving-height calculation is deliberately not required.
pub fn cumulative_scheduled_issuance(
    height: Height,
    network: &Network,
) -> Result<Amount<NonNegative>, SubsidyError> {
    let end = u64::from(height) + 1;
    let mut slow_end = u64::from(network.slow_start_interval()).min(end);
    // The schedule stops after 64 halvings, even during slow start on custom networks.
    if slow_end > 0
        && halving_divisor(
            Height(u32::try_from(slow_end - 1).map_err(|_| SubsidyError::Overflow)?),
            network,
        )
        .is_none()
    {
        let mut low = 0;
        while low < slow_end {
            let mid = low + (slow_end - low) / 2;
            if halving_divisor(
                Height(u32::try_from(mid).map_err(|_| SubsidyError::Overflow)?),
                network,
            )
            .is_some()
            {
                low = mid + 1;
            } else {
                slow_end = mid;
            }
        }
    }
    let mut total = 0u64;
    if slow_end > 0 {
        let last = slow_end - 1;
        let shift = u64::from(network.slow_start_shift());
        total = (MAX_BLOCK_SUBSIDY / u64::from(network.slow_start_interval()))
            * (last * (last + 1) / 2 + slow_end.saturating_sub(shift));
    }

    let mut start = slow_end;
    while start < end {
        let height = Height(u32::try_from(start).map_err(|_| SubsidyError::Overflow)?);
        let subsidy = u64::from(scheduled_block_subsidy(height, network)?);
        if subsidy == 0 {
            break;
        }
        let era_end = [NetworkUpgrade::Blossom, NetworkUpgrade::Nu7]
            .into_iter()
            .filter_map(|upgrade| upgrade.activation_height(network))
            .map(u64::from)
            .filter(|&boundary| boundary > start)
            .min()
            .unwrap_or(end)
            .min(end);
        let index = halving(height, network);
        let (mut low, mut high) = (start + 1, era_end);
        while low < high {
            let mid = low + (high - low) / 2;
            if halving(
                Height(u32::try_from(mid).map_err(|_| SubsidyError::Overflow)?),
                network,
            ) == index
            {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        total = total
            .checked_add(subsidy * (low - start))
            .ok_or(SubsidyError::Overflow)?;
        start = low;
    }
    Ok(Amount::try_from(total)?)
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
        scheduled_block_subsidy(height, net)
            .map(|subsidy| subsidy.div_exact(5))
            .expect("block subsidy must be valid for founders rewards")
    } else {
        Amount::zero()
    }
}

/// The contribution that a block's transaction fees make to the NSM reserve.
///
/// # Consensus
///
/// > $\mathsf{NSMFeeContribution}(\mathsf{height}) :=
/// > \mathsf{floor}(6 \cdot \mathsf{TransactionFees}(\mathsf{height}) / 10)$
///
/// This calculation is performed on the aggregate fees for the block, so rounding favours the
/// miner. It is zero before NU7 activates.
///
/// The rule is specified in the NU7 deployment ZIP (`zcash/zips#1363`), which takes precedence
/// over [ZIP 235] for NU7: NU7 does not deploy the [ZIP 233] voluntary-removal bundle, so the fee
/// contribution is a block-level calculation and needs no new coinbase field.
///
/// [ZIP 233]: https://zips.z.cash/zip-0233
/// [ZIP 235]: https://zips.z.cash/zip-0235
pub fn nsm_fee_contribution(
    height: Height,
    network: &Network,
    transaction_fees: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, amount::Error> {
    if NetworkUpgrade::current(network, height) < NetworkUpgrade::Nu7 {
        return Ok(Amount::zero());
    }

    // A `NonNegative` amount is never negative, and `transaction_fees` is at most `MAX_MONEY`,
    // which is under 2^53, so multiplying it by 6 can not overflow a `u64`. The `floor()` in the
    // spec is implicit in Rust's integer division.
    let fees =
        u64::try_from(transaction_fees.zatoshis()).map_err(|source| amount::Error::Convert {
            value: i128::from(transaction_fees.zatoshis()),
            source,
        })?;

    Amount::try_from(fees * NSM_FEE_NUMERATOR / NSM_FEE_DENOMINATOR)
}

/// The part of a block's transaction fees that its miner may claim.
///
/// # Consensus
///
/// > $\mathsf{MinerFees}(\mathsf{height}) := \mathsf{TransactionFees}(\mathsf{height}) -
/// > \mathsf{NSMFeeContribution}(\mathsf{height})$
///
/// This is the whole of the transaction fees before NU7 activates.
///
/// See [`nsm_fee_contribution`] for the specification this comes from.
pub fn miner_fees(
    height: Height,
    network: &Network,
    transaction_fees: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, amount::Error> {
    transaction_fees - nsm_fee_contribution(height, network, transaction_fees)?
}

/// The part of the NSM reserve that is reissued in the block at `height`.
///
/// # Consensus
///
/// > $\mathsf{NSMSubsidy}(\mathsf{height}) := 0$, if
/// > $\mathsf{height} < \mathsf{NSMReissuanceHeight}$, otherwise
/// > $\mathsf{ceiling}(\mathsf{NSM\_SUBSIDY\_FRACTION} \cdot
/// > \mathsf{NSMReserveAfter}(\mathsf{height} - 1))$
///
/// `reserve_before` is the NSM reserve balance after the previous block was applied.
///
/// Rounding upward ensures that any positive reserve balance is eventually reissued.
///
/// The reissuance height and halving-preserving release rate are network parameters.
pub fn nsm_subsidy(
    height: Height,
    network: &Network,
    reserve_before: Amount<NonNegative>,
) -> Result<Amount<NonNegative>, amount::Error> {
    let Some(reissuance_height) = network.nsm_reissuance_height() else {
        return Ok(Amount::zero());
    };

    if height < reissuance_height {
        return Ok(Amount::zero());
    }

    // Widen before multiplying: custom networks can have much shorter halving intervals.
    let reserve = u128::from(u64::from(reserve_before));
    // Network parameters validate a positive i64 interval, which fits u128.
    let interval = network.post_nu7_halving_interval() as u128;
    let numerator = u128::from(NSM_LN2_SCALED) / interval;
    let subsidy = (reserve * numerator).div_ceil(u128::from(NSM_SUBSIDY_DENOMINATOR));
    // MAX_MONEY times NSM_LN2_SCALED fits i128 even before division.
    Amount::try_from(subsidy as i128)
}
/// Returns `Ok()` with the deferred pool balance change of the coinbase transaction if the block
/// subsidy in `block` is valid for `network`
///
/// [3.9]: https://zips.z.cash/protocol/protocol.pdf#subsidyconcepts
pub fn subsidy_is_valid(
    block: &Block,
    net: &Network,
    expected_block_subsidy: Amount<NonNegative>,
) -> Result<DeferredPoolBalanceChange, SubsidyError> {
    if expected_block_subsidy.is_zero() {
        return Ok(DeferredPoolBalanceChange::zero());
    }

    let height = block.coinbase_height().ok_or(SubsidyError::NoCoinbase)?;

    let mut coinbase_outputs: MultiSet<Output> = block
        .transactions
        .first()
        .ok_or(SubsidyError::NoCoinbase)?
        .outputs()
        .iter()
        .cloned()
        .collect();

    let mut has_amount = |addr: &Address, amount| {
        assert!(addr.is_script_hash(), "address must be P2SH");

        coinbase_outputs.remove(&Output::new(amount, addr.script()))
    };

    // # Note
    //
    // Canopy activation is at the first halving on Mainnet, but not on Testnet. [ZIP-1014] only
    // applies to Mainnet; [ZIP-214] contains the specific rules for Testnet funding stream amount
    // values.
    //
    // [ZIP-1014]: <https://zips.z.cash/zip-1014>
    // [ZIP-214]: <https://zips.z.cash/zip-0214
    if NetworkUpgrade::current(net, height) < NetworkUpgrade::Canopy {
        // # Consensus
        //
        // > [Pre-Canopy] A coinbase transaction at `height ∈ {1 .. FoundersRewardLastBlockHeight}`
        // > MUST include at least one output that pays exactly `FoundersReward(height)` zatoshi
        // > with a standard P2SH script of the form `OP_HASH160 FounderRedeemScriptHash(height)
        // > OP_EQUAL` as its `scriptPubKey`.
        //
        // ## Notes
        //
        // - `FoundersRewardLastBlockHeight := max({height : N | Halving(height) < 1})`
        //
        // <https://zips.z.cash/protocol/protocol.pdf#foundersreward>

        if Height::MIN < height && height < net.height_for_first_halving() {
            let addr = founders_reward_address(net, height).ok_or(SubsidyError::Other(format!(
                "founders reward address must be defined for height: {height:?}"
            )))?;

            if !has_amount(&addr, founders_reward(net, height)) {
                Err(SubsidyError::FoundersRewardNotFound)?;
            }
        }

        Ok(DeferredPoolBalanceChange::zero())
    } else {
        // # Consensus
        //
        // > [Canopy onward] In each block with coinbase transaction `cb` at block height `height`,
        // > `cb` MUST contain at least the given number of distinct outputs for each of the
        // > following:
        //
        // > • for each funding stream `fs` active at that block height with a recipient identifier
        // > other than `DEFERRED_POOL` given by `fs.Recipient(height)`, one output that pays
        // > `fs.Value(height)` zatoshi in the prescribed way to the address represented by that
        // > recipient identifier;
        //
        // > • [NU6.1 onward] if the block height is `ZIP271ActivationHeight`,
        // > `ZIP271DisbursementChunks` equal outputs paying a total of `ZIP271DisbursementAmount`
        // > zatoshi in the prescribed way to the Key-Holder Organizations’ P2SH multisig address
        // > represented by `ZIP271DisbursementAddress`, as specified by [ZIP-271].
        //
        // > The term “prescribed way” is defined as follows:
        //
        // > The prescribed way to pay a transparent P2SH address is to use a standard P2SH script
        // > of the form `OP_HASH160 fs.RedeemScriptHash(height) OP_EQUAL` as the `scriptPubKey`.
        // > Here `fs.RedeemScriptHash(height)` is the standard redeem script hash for the recipient
        // > address for `fs.Recipient(height)` in _Base58Check_ form. Standard redeem script hashes
        // > are defined in [ZIP-48] for P2SH multisig addresses, or [Bitcoin-P2SH] for other P2SH
        // > addresses.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#fundingstreams>
        //
        // [ZIP-271]: <https://zips.z.cash/zip-0271>
        // [ZIP-48]: <https://zips.z.cash/zip-0048>
        // [Bitcoin-P2SH]: <https://developer.bitcoin.org/devguide/transactions.html#pay-to-script-hash-p2sh>

        let mut funding_streams = funding_stream_values(height, net, expected_block_subsidy)?;

        // The deferred pool contribution is checked in `miner_fees_are_valid()` according to
        // [ZIP-1015](https://zips.z.cash/zip-1015).
        let mut deferred_pool_balance_change = funding_streams
            .remove(&FundingStreamReceiver::Deferred)
            .unwrap_or_default()
            .constrain::<NegativeAllowed>()?;

        // Check the one-time lockbox disbursements in the NU6.1 activation block's coinbase tx
        // according to [ZIP-271] and [ZIP-1016].
        //
        // [ZIP-271]: <https://zips.z.cash/zip-0271>
        // [ZIP-1016]: <https://zips.z.cash/zip-101>
        if Some(height) == NetworkUpgrade::Nu6_1.activation_height(net) {
            let lockbox_disbursements = net.lockbox_disbursements(height);

            // The Mainnet and default Testnet disbursement lists are hardcoded and must be
            // non-empty. Custom testnets and Regtest may configure no disbursements, in which
            // case the NU6.1 activation block is not required to contain any disbursement
            // outputs.
            let must_have_disbursements =
                matches!(net, Network::Mainnet) || net.is_default_testnet();
            if lockbox_disbursements.is_empty() && must_have_disbursements {
                Err(SubsidyError::Other(
                    "missing lockbox disbursements for NU6.1 activation block".to_string(),
                ))?;
            }

            deferred_pool_balance_change = lockbox_disbursements.into_iter().try_fold(
                deferred_pool_balance_change,
                |balance, (addr, expected_amount)| {
                    if !has_amount(&addr, expected_amount) {
                        Err(SubsidyError::OneTimeLockboxDisbursementNotFound)?;
                    }

                    balance
                        .checked_sub(expected_amount)
                        .ok_or(SubsidyError::Underflow)
                },
            )?;
        };

        // Check each funding stream output.
        funding_streams.into_iter().try_for_each(
            |(receiver, expected_amount)| -> Result<(), SubsidyError> {
                let addr =
                    funding_stream_address(height, net, receiver).ok_or(SubsidyError::Other(
                        "A funding stream other than the deferred pool must have an address"
                            .to_string(),
                    ))?;

                if !has_amount(addr, expected_amount) {
                    Err(SubsidyError::FundingStreamNotFound)?;
                }

                Ok(())
            },
        )?;

        Ok(DeferredPoolBalanceChange::new(deferred_pool_balance_change))
    }
}

/// Returns `Ok(())` if the miner fees consensus rule is valid.
///
/// From NU7, only part of the block's transaction fees may be claimed by the miner: the rest is
/// removed from circulation into the NSM reserve. See [`nsm_fee_contribution`].
///
/// [7.1.2]: https://zips.z.cash/protocol/protocol.pdf#txnconsensus
pub fn miner_fees_are_valid(
    coinbase_tx: &Transaction,
    height: Height,
    block_miner_fees: Amount<NonNegative>,
    expected_block_subsidy: Amount<NonNegative>,
    expected_deferred_pool_balance_change: DeferredPoolBalanceChange,
    network: &Network,
) -> Result<(), SubsidyError> {
    let transparent_value_balance = coinbase_tx
        .outputs()
        .iter()
        .map(|output| output.value())
        .sum::<Result<Amount<NonNegative>, amount::Error>>()
        .map_err(|_| SubsidyError::Overflow)?
        .constrain()
        .map_err(|e| SubsidyError::Other(format!("invalid transparent value balance: {e}")))?;
    let sapling_value_balance = coinbase_tx.sapling_value_balance().sapling_amount();
    let orchard_value_balance = coinbase_tx.orchard_value_balance().orchard_amount();
    // [NU6.3 onward] The Ironwood pool is shielded too, so its value balance affects the coinbase
    // output value exactly like Sapling and Orchard. This is zero for pre-v6 coinbase transactions
    // (no Ironwood bundle), so it is a no-op before NU6.3.
    let ironwood_value_balance = coinbase_tx.ironwood_value_balance().ironwood_amount();

    // # Consensus
    //
    // > - define the total output value of its coinbase transaction to be the total value in zatoshi of its transparent
    // >   outputs, minus vbalanceSapling, minus vbalanceOrchard, minus vbalanceIronwood, plus totalDeferredOutput(height);
    // > – define the total input value of its coinbase transaction to be the value in zatoshi of the block subsidy,
    // >   plus the transaction fees paid by transactions in the block.
    //
    // https://zips.z.cash/protocol/protocol.pdf#txnconsensus
    //
    // The expected lockbox funding stream output of the coinbase transaction is also subtracted
    // from the block subsidy value plus the transaction fees paid by transactions in this block.
    let total_output_value = (transparent_value_balance
        - sapling_value_balance
        - orchard_value_balance
        - ironwood_value_balance
        + expected_deferred_pool_balance_change.value())
    .map_err(|_| SubsidyError::Overflow)?;

    // # Consensus
    //
    // > For every block from NU7 activation onward, the coinbase transaction MUST be balanced
    // > using MinerFees in place of TransactionFees, and the NSM reserve balance MUST increase by
    // > NSMFeeContribution.
    //
    // > In particular, the total input value of the coinbase transaction defined in
    // > § 7.1.2 'Transaction Consensus Rules' MUST be calculated as:
    // > BlockSubsidy(height) + MinerFees(height) + totalDeferredInput(height).
    //
    // This is the NU7 deployment ZIP (`zcash/zips#1363`), which takes precedence over ZIP 235 for
    // NU7. `miner_fees()` returns the whole of the fees before NU7, so this is a no-op until then.
    let block_miner_fees =
        miner_fees(height, network, block_miner_fees).map_err(|_| SubsidyError::Overflow)?;

    let total_input_value =
        (expected_block_subsidy + block_miner_fees).map_err(|_| SubsidyError::Overflow)?;

    // # Consensus
    //
    // > [Pre-NU6] The total output of a coinbase transaction MUST NOT be greater than its total
    // input.
    //
    // > [NU6 onward] The total output of a coinbase transaction MUST be equal to its total input.
    if if NetworkUpgrade::current(network, height) < NetworkUpgrade::Nu6 {
        total_output_value > total_input_value
    } else {
        total_output_value != total_input_value
    } {
        Err(SubsidyError::InvalidMinerFees)?
    };

    Ok(())
}
