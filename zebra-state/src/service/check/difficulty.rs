//! Block difficulty adjustment calculations for contextual validation.
//!
//! This module supports the following consensus rule calculations:
//!  * `ThresholdBits` from the Zcash Specification,
//!  * the Testnet minimum difficulty adjustment from ZIPs 205 and 208, and
//!  * `median-time-past`.

use chrono::{DateTime, Duration, Utc};

use std::{cmp::max, cmp::min, convert::TryInto};

use zebra_chain::{
    block,
    block::Block,
    parameters::Network,
    parameters::NetworkUpgrade,
    parameters::POW_AVERAGING_WINDOW,
    work::difficulty::ExpandedDifficulty,
    work::difficulty::{CompactDifficulty, U256},
};

/// The median block span for time median calculations.
///
/// `PoWMedianBlockSpan` in the Zcash specification.
pub const POW_MEDIAN_BLOCK_SPAN: usize = 11;

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
pub const BLOCK_MAX_TIME_SINCE_MEDIAN: i64 = 90 * 60;

/// Contains the context needed to calculate the adjusted difficulty for a block.
pub(super) struct AdjustedDifficulty {
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
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks, in reverse height
    /// order.
    relevant_difficulty_thresholds:
        [CompactDifficulty; POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN],
    /// The `header.time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks, in reverse height
    /// order.
    ///
    /// Only the first and last `PoWMedianBlockSpan` times are used. Times
    /// `11..=16` are ignored.
    relevant_times: [DateTime<Utc>; POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN],
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
    /// If the `context` contains fewer than 28 items.
    pub fn new_from_block<C>(
        candidate_block: &Block,
        network: Network,
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

        AdjustedDifficulty::new_from_header(
            &candidate_block.header,
            previous_block_height,
            network,
            context,
        )
    }

    /// Initialise and return a new [`AdjustedDifficulty`] using a
    /// `candidate_header`, `previous_block_height`, `network`, and a `context`.
    ///
    /// Designed for use when validating block headers, where the full block has not
    /// been downloaded yet.
    ///
    /// See [`Self::new_from_block`] for detailed information about the `context`.
    ///
    /// # Panics
    ///
    /// If the context contains fewer than 28 items.
    pub fn new_from_header<C>(
        candidate_header: &block::Header,
        previous_block_height: block::Height,
        network: Network,
        context: C,
    ) -> AdjustedDifficulty
    where
        C: IntoIterator<Item = (CompactDifficulty, DateTime<Utc>)>,
    {
        let candidate_height = (previous_block_height + 1).expect("next block height is valid");

        let (relevant_difficulty_thresholds, relevant_times) = context
            .into_iter()
            .take(POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN)
            .unzip::<_, _, Vec<_>, Vec<_>>();

        let relevant_difficulty_thresholds = relevant_difficulty_thresholds
            .try_into()
            .expect("not enough context: difficulty adjustment needs at least 28 (PoWAveragingWindow + PoWMedianBlockSpan) headers");
        let relevant_times = relevant_times
            .try_into()
            .expect("not enough context: difficulty adjustment needs at least 28 (PoWAveragingWindow + PoWMedianBlockSpan) headers");

        AdjustedDifficulty {
            candidate_time: candidate_header.time,
            candidate_height,
            network,
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

    /// Returns the configured network.
    pub fn network(&self) -> Network {
        self.network
    }

    /// Calculate the expected `difficulty_threshold` for a candidate block, based
    /// on the `candidate_time`, `candidate_height`, `network`, and the
    /// `difficulty_threshold`s and `time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
    ///
    /// Implements `ThresholdBits` from the Zcash specification, and the Testnet
    /// minimum difficulty adjustment from ZIPs 205 and 208.
    pub fn expected_difficulty_threshold(&self) -> CompactDifficulty {
        if NetworkUpgrade::is_testnet_min_difficulty_block(
            self.network,
            self.candidate_height,
            self.candidate_time,
            self.relevant_times[0],
        ) {
            assert_eq!(
                self.network,
                Network::Testnet,
                "invalid network: the minimum difficulty rule only applies on testnet"
            );
            ExpandedDifficulty::target_difficulty_limit(self.network).to_compact()
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
            self.network,
            self.candidate_height,
        );

        let threshold = (self.mean_target_difficulty() / averaging_window_timespan.num_seconds())
            * self.median_timespan_bounded().num_seconds();
        let threshold = min(
            ExpandedDifficulty::target_difficulty_limit(self.network),
            threshold,
        );

        threshold.to_compact()
    }

    /// Calculate the arithmetic mean of the averaging window thresholds: the
    /// expanded `difficulty_threshold`s from the previous `PoWAveragingWindow` (17)
    /// blocks in the relevant chain.
    ///
    /// Implements `MeanTarget` from the Zcash specification.
    fn mean_target_difficulty(&self) -> ExpandedDifficulty {
        // In Zebra, contextual validation starts after Canopy activation, so we
        // can assume that the relevant chain contains at least 17 blocks.
        // Therefore, the `PoWLimit` case of `MeanTarget()` from the Zcash
        // specification is unreachable.

        let averaging_window_thresholds =
            &self.relevant_difficulty_thresholds[0..POW_AVERAGING_WINDOW];

        // Since the PoWLimits are `2^251 − 1` for Testnet, and `2^243 − 1` for
        // Mainnet, the sum of 17 `ExpandedDifficulty` will be less than or equal
        // to: `(2^251 − 1) * 17 = 2^255 + 2^251 - 17`. Therefore, the sum can
        // not overflow a u256 value.
        let total: ExpandedDifficulty = averaging_window_thresholds
            .iter()
            .map(|compact| {
                compact
                    .to_expanded()
                    .expect("difficulty thresholds in previously verified blocks are valid")
            })
            .sum();

        let divisor: U256 = POW_AVERAGING_WINDOW.into();
        total / divisor
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
            self.network,
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

        let older_times: [DateTime<Utc>; POW_MEDIAN_BLOCK_SPAN] = self.relevant_times
            [POW_AVERAGING_WINDOW..]
            .try_into()
            .expect("relevant times is the correct length");
        let older_median = AdjustedDifficulty::median_time(older_times);

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
        let median_times: [DateTime<Utc>; POW_MEDIAN_BLOCK_SPAN] = self.relevant_times
            [0..POW_MEDIAN_BLOCK_SPAN]
            .try_into()
            .expect("relevant times is the correct length");

        AdjustedDifficulty::median_time(median_times)
    }

    /// Calculate the median of the `median_block_span_times`: the `time`s from a
    /// slice of `PoWMedianBlockSpan` (11) blocks in the relevant chain.
    ///
    /// Implements `MedianTime` from the Zcash specification.
    fn median_time(
        mut median_block_span_times: [DateTime<Utc>; POW_MEDIAN_BLOCK_SPAN],
    ) -> DateTime<Utc> {
        median_block_span_times.sort_unstable();
        median_block_span_times[POW_MEDIAN_BLOCK_SPAN / 2]
    }
}
