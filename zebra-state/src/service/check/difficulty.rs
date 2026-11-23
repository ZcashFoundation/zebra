//! Block difficulty adjustment calculations for contextual validation.
//!
//! This module supports the following consensus rule calculations:
//!  * `ThresholdBits` from the Zcash Specification,
//!  * the Testnet minimum difficulty adjustment from ZIPs 205 and 208, and
//!  * `median-time-past`.

use std::cmp::{max, min};

use chrono::{DateTime, Duration, Utc};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade, MAX_POW_AVERAGING_WINDOW},
    work::difficulty::{CompactDifficulty, ExpandedDifficulty, ParameterDifficulty as _, U256},
    BoundedVec,
};

#[cfg(test)]
mod tests;

/// The median block span for time median calculations.
///
/// `PoWMedianBlockSpan` in the Zcash specification.
pub const POW_MEDIAN_BLOCK_SPAN: usize = 11;

/// The largest overall block span used for adjusting Zcash block difficulty.
///
/// `PoWAveragingWindow(height) + PoWMedianBlockSpan` in the Zcash specification, for the height
/// with the largest averaging window, based on
/// > ActualTimespan(height : N) := MedianTime(height) − MedianTime(height − PoWAveragingWindow)
///
/// Used to size the buffers that hold difficulty adjustment context. Use
/// [`pow_adjustment_block_span`] for the span that actually applies to a block, because [ZIP 218]
/// makes the averaging window height-dependent.
///
/// [ZIP 218]: https://zips.z.cash/zip-0218
pub const MAX_POW_ADJUSTMENT_BLOCK_SPAN: usize = MAX_POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN;

/// The overall block span used for adjusting the difficulty of the block at `height` on `network`.
///
/// `PoWAveragingWindow(height) + PoWMedianBlockSpan` in the Zcash specification.
pub fn pow_adjustment_block_span(network: &Network, height: block::Height) -> usize {
    NetworkUpgrade::averaging_window_for_height(network, height) + POW_MEDIAN_BLOCK_SPAN
}

/// The damping factor for median timespan variance.
///
/// `PoWDampingFactor` in the Zcash specification.
pub const POW_DAMPING_FACTOR: i32 = 4;

/// The maximum upward adjustment percentage for median timespan variance.
///
/// `PoWMaxAdjustUp * 100` in the Zcash specification.
pub const POW_MAX_ADJUST_UP_PERCENT: i32 = 16;

/// The maximum downward adjustment percentage for median timespan variance.
///
/// `PoWMaxAdjustDown * 100` in the Zcash specification.
pub const POW_MAX_ADJUST_DOWN_PERCENT: i32 = 32;

/// The maximum number of seconds between the `median-time-past` of a block,
/// and the block's `time` field.
///
/// Part of the block header consensus rules in the Zcash specification.
pub const BLOCK_MAX_TIME_SINCE_MEDIAN: u32 = 90 * 60;

/// Contains the context needed to calculate the adjusted difficulty for a block.
pub(crate) struct AdjustedDifficulty {
    /// The `header.time` field from the candidate block
    candidate_time: DateTime<Utc>,
    /// The coinbase height from the candidate block
    ///
    /// If we only have the header, this field is calculated from the previous
    /// block height.
    candidate_height: block::Height,
    /// The configured network
    network: Network,
    /// The `header.difficulty_threshold`s from the previous
    /// `PoWAveragingWindow(height) + PoWMedianBlockSpan` blocks, in reverse height
    /// order.
    relevant_difficulty_thresholds: BoundedVec<CompactDifficulty, 1, MAX_POW_ADJUSTMENT_BLOCK_SPAN>,
    /// The `header.time`s from the previous
    /// `PoWAveragingWindow(height) + PoWMedianBlockSpan` blocks, in reverse height
    /// order.
    ///
    /// Only the first and last `PoWMedianBlockSpan` times are used. The times in
    /// between the two medians are ignored.
    relevant_times: BoundedVec<DateTime<Utc>, 1, MAX_POW_ADJUSTMENT_BLOCK_SPAN>,
}

impl AdjustedDifficulty {
    /// Initialise and return a new `AdjustedDifficulty` using a `candidate_block`,
    /// `network`, and a `context`.
    ///
    /// The `context` contains the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) `difficulty_threshold`s and
    /// `time`s from the relevant chain for `candidate_block`, in reverse height
    /// order, starting with the previous block.
    ///
    /// Note that the `time`s might not be in reverse chronological order, because
    /// block times are supplied by miners.
    ///
    /// # Panics
    ///
    /// This function may panic in the following cases:
    /// - The `candidate_block` has no coinbase height (should never happen for valid blocks).
    /// - The `candidate_block` is the genesis block, so `previous_block_height` cannot be computed.
    /// - `AdjustedDifficulty::new_from_header_time` panics.
    pub fn new_from_block<C>(
        candidate_block: &Block,
        network: &Network,
        context: C,
    ) -> AdjustedDifficulty
    where
        C: IntoIterator<Item = (CompactDifficulty, DateTime<Utc>)>,
    {
        let candidate_block_height = candidate_block
            .coinbase_height()
            .expect("semantically valid blocks have a coinbase height");
        let previous_block_height = (candidate_block_height - 1)
            .expect("contextual validation is never run on the genesis block");

        AdjustedDifficulty::new_from_header_time(
            candidate_block.header.time,
            previous_block_height,
            network,
            context,
        )
    }

    /// Initialise and return a new [`AdjustedDifficulty`] using a
    /// `candidate_header_time`, `previous_block_height`, `network`, and a `context`.
    ///
    /// Designed for use when validating block headers, where the full block has not
    /// been downloaded yet.
    ///
    /// See [`Self::new_from_block`] for detailed information about the `context`.
    ///
    /// # Panics
    ///
    /// This function may panic in the following cases:
    /// - The next block height is invalid.
    /// - The `context` iterator is empty, because at least one difficulty threshold
    ///   and block time are required to construct the `Bounded` vectors.
    /// - The context iterator is empty, because at least one difficulty threshold and block time are required.
    pub fn new_from_header_time<C>(
        candidate_header_time: DateTime<Utc>,
        previous_block_height: block::Height,
        network: &Network,
        context: C,
    ) -> AdjustedDifficulty
    where
        C: IntoIterator<Item = (CompactDifficulty, DateTime<Utc>)>,
    {
        let candidate_height = (previous_block_height + 1).expect("next block height is valid");

        let (thresholds, times) = context
            .into_iter()
            .take(pow_adjustment_block_span(network, candidate_height))
            .unzip::<_, _, Vec<_>, Vec<_>>();

        let relevant_difficulty_thresholds: BoundedVec<
            CompactDifficulty,
            1,
            MAX_POW_ADJUSTMENT_BLOCK_SPAN,
        > = thresholds
            .try_into()
            .expect("context must provide a bounded number of difficulty thresholds");
        let relevant_times: BoundedVec<DateTime<Utc>, 1, MAX_POW_ADJUSTMENT_BLOCK_SPAN> = times
            .try_into()
            .expect("context must provide a bounded number of block times");

        AdjustedDifficulty {
            candidate_time: candidate_header_time,
            candidate_height,
            network: network.clone(),
            relevant_difficulty_thresholds,
            relevant_times,
        }
    }

    /// Returns the candidate block's height.
    pub fn candidate_height(&self) -> block::Height {
        self.candidate_height
    }

    /// Returns the candidate block's time field.
    pub fn candidate_time(&self) -> DateTime<Utc> {
        self.candidate_time
    }

    /// Returns the difficulty averaging window that applies to the candidate block.
    ///
    /// `PoWAveragingWindow(height)` in the Zcash specification, as redefined by [ZIP 218].
    ///
    /// [ZIP 218]: https://zips.z.cash/zip-0218
    fn averaging_window(&self) -> usize {
        NetworkUpgrade::averaging_window_for_height(&self.network, self.candidate_height)
    }

    /// Returns the configured network.
    pub fn network(&self) -> Network {
        self.network.clone()
    }

    /// Calculate the expected `difficulty_threshold` for a candidate block, based
    /// on the `candidate_time`, `candidate_height`, `network`, and the
    /// `difficulty_threshold`s and `time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
    ///
    /// Implements `ThresholdBits` from the Zcash specification, and the Testnet
    /// minimum difficulty adjustment from ZIPs 205 and 208.
    pub fn expected_difficulty_threshold(&self) -> CompactDifficulty {
        // Regtest uses fixed minimum difficulty with no retargeting, matching
        // zcashd's and Bitcoin's `fPowNoRetargeting`. This keeps every regtest
        // block at the powLimit, so a zcashd sidecar following a Zebra regtest
        // chain accepts its headers instead of rejecting them as "bad-diffbits".
        if self.network.is_regtest() {
            return self.network.target_difficulty_limit().to_compact();
        }

        if NetworkUpgrade::is_testnet_min_difficulty_block(
            &self.network,
            self.candidate_height,
            self.candidate_time,
            *self.relevant_times.first(),
        ) {
            assert!(
                self.network.is_a_test_network(),
                "invalid network: the minimum difficulty rule only applies on test networks"
            );
            self.network.target_difficulty_limit().to_compact()
        } else {
            self.threshold_bits()
        }
    }

    /// Calculate the `difficulty_threshold` for a candidate block, based on the
    /// `candidate_height`, `network`, and the relevant `difficulty_threshold`s and
    /// `time`s.
    ///
    /// See [`Self::expected_difficulty_threshold`] for details.
    ///
    /// Implements `ThresholdBits` from the Zcash specification. (Which excludes the
    /// Testnet minimum difficulty adjustment.)
    fn threshold_bits(&self) -> CompactDifficulty {
        let averaging_window_timespan = NetworkUpgrade::averaging_window_timespan_for_height(
            &self.network,
            self.candidate_height,
        );

        let threshold = (self.mean_target_difficulty() / averaging_window_timespan.num_seconds())
            * self.median_timespan_bounded().num_seconds();
        let threshold = min(self.network.target_difficulty_limit(), threshold);

        threshold.to_compact()
    }

    /// Calculate the arithmetic mean of the expanded `difficulty_threshold`s from
    /// the previous `PoWAveragingWindow(height)` blocks in the relevant chain.
    ///
    /// Implements `MeanTarget` from the Zcash specification.
    fn mean_target_difficulty(&self) -> ExpandedDifficulty {
        let averaging_window = self.averaging_window();
        let averaging_window_height = u32::try_from(averaging_window)
            .expect("the averaging window is at most MAX_POW_AVERAGING_WINDOW");
        if self.candidate_height.0 <= averaging_window_height
            || self.relevant_difficulty_thresholds.len() < averaging_window
        {
            return self.network.target_difficulty_limit();
        }

        let averaging_window_thresholds =
            &self.relevant_difficulty_thresholds.as_slice()[..averaging_window];

        // A sum of 102 Testnet targets can overflow U256. Sum the quotients and
        // remainders separately to preserve floor(sum(targets) / window) exactly:
        // the quotient sum is at most U256::MAX, and the remainder sum is < window^2.
        let divisor: U256 = averaging_window.into();
        let (quotients, remainders) = averaging_window_thresholds.iter().fold(
            (U256::zero(), U256::zero()),
            |(quotients, remainders), compact| {
                let target = compact
                    .to_expanded()
                    .expect("difficulty thresholds in previously verified blocks are valid");
                let (quotient, remainder) = U256::from(target).div_mod(divisor);
                (quotients + quotient, remainders + remainder)
            },
        );

        (quotients + remainders / divisor).into()
    }

    /// Calculate the bounded median timespan. The median timespan is the
    /// difference of medians of the timespan times, which are the `time`s from
    /// the previous `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the
    /// relevant chain.
    ///
    /// Uses the candidate block's `height' and `network` to calculate the
    /// `AveragingWindowTimespan` for that block.
    ///
    /// The median timespan is damped by the `PoWDampingFactor`, and bounded by
    /// `PoWMaxAdjustDown` and `PoWMaxAdjustUp`.
    ///
    /// Implements `ActualTimespanBounded` from the Zcash specification.
    ///
    /// Note: This calculation only uses `PoWMedianBlockSpan` (11) times at the
    /// start and end of the timespan times. timespan times `[11..=16]` are ignored.
    fn median_timespan_bounded(&self) -> Duration {
        let averaging_window_timespan = NetworkUpgrade::averaging_window_timespan_for_height(
            &self.network,
            self.candidate_height,
        );
        // This value is exact, but we need to truncate its nanoseconds component
        let damped_variance =
            (self.median_timespan() - averaging_window_timespan) / POW_DAMPING_FACTOR;
        // num_seconds truncates negative values towards zero, matching the Zcash specification
        let damped_variance = Duration::seconds(damped_variance.num_seconds());

        // `ActualTimespanDamped` in the Zcash specification
        let median_timespan_damped = averaging_window_timespan + damped_variance;

        // `MinActualTimespan` and `MaxActualTimespan` in the Zcash spec
        let min_median_timespan =
            averaging_window_timespan * (100 - POW_MAX_ADJUST_UP_PERCENT) / 100;
        let max_median_timespan =
            averaging_window_timespan * (100 + POW_MAX_ADJUST_DOWN_PERCENT) / 100;

        // `ActualTimespanBounded` in the Zcash specification
        max(
            min_median_timespan,
            min(max_median_timespan, median_timespan_damped),
        )
    }

    /// Calculate the median timespan. The median timespan is the difference of
    /// medians of the timespan times, which are the `time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
    ///
    /// Implements `ActualTimespan` from the Zcash specification.
    ///
    /// See [`Self::median_timespan_bounded`] for details.
    fn median_timespan(&self) -> Duration {
        let newer_median = self.median_time_past();

        // MedianTime(height : N) := median([ nTime(𝑖) for 𝑖 from max(0, height − PoWMedianBlockSpan) up to max(0, height − 1) ])
        let averaging_window = self.averaging_window();

        let older_median = if self.relevant_times.len() > averaging_window {
            let older_times: Vec<_> = self
                .relevant_times
                .iter()
                .skip(averaging_window)
                .cloned()
                .take(POW_MEDIAN_BLOCK_SPAN)
                .collect();

            AdjustedDifficulty::median_time(older_times)
        } else {
            *self.relevant_times.last()
        };

        // `ActualTimespan` in the Zcash specification
        newer_median - older_median
    }

    /// Calculate the median of the `time`s from the previous
    /// `PoWMedianBlockSpan` (11) blocks in the relevant chain.
    ///
    /// Implements `median-time-past` and `MedianTime(candidate_height)` from the
    /// Zcash specification. (These functions are identical, but they are
    /// specified in slightly different ways.)
    pub fn median_time_past(&self) -> DateTime<Utc> {
        let median_times: Vec<DateTime<Utc>> = self
            .relevant_times
            .iter()
            .take(POW_MEDIAN_BLOCK_SPAN)
            .cloned()
            .collect();

        AdjustedDifficulty::median_time(median_times)
    }

    /// Calculate the median of the `median_block_span_times`: the `time`s from a
    /// Vec of `PoWMedianBlockSpan` (11) or fewer blocks in the relevant chain.
    ///
    /// Implements `MedianTime` from the Zcash specification.
    ///
    /// # Panics
    ///
    /// If provided an empty Vec
    pub(crate) fn median_time(mut median_block_span_times: Vec<DateTime<Utc>>) -> DateTime<Utc> {
        median_block_span_times.sort_unstable();

        // > median(𝑆) := sorted(𝑆)_{ceiling((length(𝑆)+1)/2)}
        // <https://zips.z.cash/protocol/protocol.pdf>, section 7.7.3, Difficulty Adjustment (p. 132)
        let median_idx = median_block_span_times.len() / 2;
        median_block_span_times[median_idx]
    }
}
