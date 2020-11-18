//! Block difficulty adjustment calculations for contextual validation.

use chrono::{DateTime, Utc};
use primitive_types::U256;

use std::convert::TryInto;

use zebra_chain::{
    block, block::Block, parameters::Network, work::difficulty::CompactDifficulty,
    work::difficulty::ExpandedDifficulty,
};

/// The averaging window for difficulty threshold arithmetic mean calculations.
///
/// `PoWAveragingWindow` in the Zcash specification.
pub const POW_AVERAGING_WINDOW: usize = 17;

/// The median block span for time median calculations.
///
/// `PoWMedianBlockSpan` in the Zcash specification.
pub const POW_MEDIAN_BLOCK_SPAN: usize = 11;

/// Contains the context needed to calculate the adjusted difficulty for a block.
#[allow(dead_code)]
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
    relevant_difficulty_thresholds: [CompactDifficulty; 28],
    /// The `header.time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks, in reverse height
    /// order.
    ///
    /// Only the first and last `PoWMedianBlockSpan` times are used. Times
    /// `11..=16` are ignored.
    relevant_times: [DateTime<Utc>; 28],
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
    /// Panics:
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

    /// Initialise and return a new `AdjustedDifficulty` using a
    /// `candidate_header`, `previous_block_height`, `network`, and a `context`.
    ///
    /// Designed for use when validating block headers, where the full block has not
    /// been downloaded yet.
    ///
    /// See `new_from_block` for detailed information about the `context`.
    ///
    /// Panics:
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

        // unzip would be a lot nicer here, but we can't satisfy its trait bounds
        let context: Vec<_> = context
            .into_iter()
            .take(POW_AVERAGING_WINDOW + POW_MEDIAN_BLOCK_SPAN)
            .collect();
        let relevant_difficulty_thresholds = context
            .iter()
            .map(|pair| pair.0)
            .collect::<Vec<_>>()
            .try_into()
            .expect("not enough context: difficulty adjustment needs at least 28 (PoWAveragingWindow + PoWMedianBlockSpan) headers");
        let relevant_times = context
            .iter()
            .map(|pair| pair.1)
            .collect::<Vec<_>>()
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

    /// Calculate the expected `difficulty_threshold` for a candidate block, based
    /// on the `candidate_time`, `candidate_height`, `network`, and the
    /// `difficulty_threshold`s and `time`s from the previous
    /// `PoWAveragingWindow + PoWMedianBlockSpan` (28) blocks in the relevant chain.
    ///
    /// Implements `ThresholdBits` from the Zcash specification, and the Testnet
    /// minimum difficulty adjustment from ZIPs 205 and 208.
    pub fn expected_difficulty_threshold(&self) -> CompactDifficulty {
        // TODO: Testnet minimum difficulty
        self.threshold_bits()
    }

    /// Calculate the `difficulty_threshold` for a candidate block, based on the
    /// `candidate_height`, `network`, and the relevant `difficulty_threshold`s and
    /// `time`s.
    ///
    /// See `expected_difficulty_threshold` for details.
    ///
    /// Implements `ThresholdBits` from the Zcash specification. (Which excludes the
    /// Testnet minimum difficulty adjustment.)
    fn threshold_bits(&self) -> CompactDifficulty {
        let mean_target = self.mean_target_difficulty();

        // TODO: calculate the actual value
        mean_target.to_compact()
    }

    /// Calculate the arithmetic mean of the averaging window thresholds: the
    /// expanded `difficulty_threshold`s from the previous `PoWAveragingWindow` (17)
    /// blocks in the relevant chain.
    ///
    /// Implements `MeanTarget` from the Zcash specification.
    fn mean_target_difficulty(&self) -> ExpandedDifficulty {
        // In Zebra, contextual validation starts after Sapling activation, so we
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
        let total: U256 = total.into();
        let divisor: U256 = POW_AVERAGING_WINDOW.into();

        (total / divisor).into()
    }
}
