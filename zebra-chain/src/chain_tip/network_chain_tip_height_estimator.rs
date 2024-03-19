//! A module with helper code to estimate the network chain tip's height.

use std::vec;

use chrono::{DateTime, Duration, Utc};

use crate::{
    block::{self, HeightDiff},
    parameters::{Network, NetworkUpgrade},
};

/// A type used to estimate the chain tip height at a given time.
///
/// The estimation is based on a known block time and height for a block. The estimator will then
/// handle any target spacing changes to extrapolate the provided information into a target time
/// and obtain an estimation for the height at that time.
///
/// # Usage
///
/// 1. Use [`NetworkChainTipHeightEstimator::new`] to create and initialize a new instance with the
///    information about a known block.
/// 2. Use [`NetworkChainTipHeightEstimator::estimate_height_at`] to obtain a height estimation for
///    a given instant.
#[derive(Debug)]
pub struct NetworkChainTipHeightEstimator {
    current_block_time: DateTime<Utc>,
    current_height: block::Height,
    current_target_spacing: Duration,
    next_target_spacings: vec::IntoIter<(block::Height, Duration)>,
}

impl NetworkChainTipHeightEstimator {
    /// Create a [`NetworkChainTipHeightEstimator`] and initialize it with the information to use
    /// for calculating a chain height estimate.
    ///
    /// The provided information (`current_block_time`, `current_height` and `network`) **must**
    /// refer to the same block.
    ///
    /// # Implementation details
    ///
    /// The `network` is used to obtain a list of target spacings used in different sections of the
    /// block chain. The first section is used as a starting point.
    pub fn new(
        current_block_time: DateTime<Utc>,
        current_height: block::Height,
        network: &Network,
    ) -> Self {
        let mut target_spacings = NetworkUpgrade::target_spacings(network);
        let (_genesis_height, initial_target_spacing) =
            target_spacings.next().expect("No target spacings were set");

        NetworkChainTipHeightEstimator {
            current_block_time,
            current_height,
            current_target_spacing: initial_target_spacing,
            // TODO: Remove the `Vec` allocation once existential `impl Trait`s are available.
            next_target_spacings: target_spacings.collect::<Vec<_>>().into_iter(),
        }
    }

    /// Estimate the network chain tip height at the provided `target_time`.
    ///
    /// # Implementation details
    ///
    /// The `current_block_time` and the `current_height` is advanced to the end of each section
    /// that has a different target spacing time. Once the `current_block_time` passes the
    /// `target_time`, the last active target spacing time is used to calculate the final height
    /// estimation.
    pub fn estimate_height_at(mut self, target_time: DateTime<Utc>) -> block::Height {
        while let Some((change_height, next_target_spacing)) = self.next_target_spacings.next() {
            self.estimate_up_to(change_height);

            if self.current_block_time >= target_time {
                break;
            }

            self.current_target_spacing = next_target_spacing;
        }

        self.estimate_height_at_with_current_target_spacing(target_time)
    }

    /// Advance the `current_block_time` and `current_height` to the next change in target spacing
    /// time.
    ///
    /// The `current_height` is advanced to `max_height` (if it's not already past that height).
    /// The amount of blocks advanced is then used to extrapolate the amount to advance the
    /// `current_block_time`.
    fn estimate_up_to(&mut self, max_height: block::Height) {
        let remaining_blocks = max_height - self.current_height;

        if remaining_blocks > 0 {
            let target_spacing_seconds = self.current_target_spacing.num_seconds();
            let time_to_activation = Duration::seconds(remaining_blocks * target_spacing_seconds);
            self.current_block_time += time_to_activation;
            self.current_height = max_height;
        }
    }

    /// Calculate an estimate for the chain height using the `current_target_spacing`.
    ///
    /// Using the difference between the `target_time` and the `current_block_time` and the
    /// `current_target_spacing`, the number of blocks to reach the `target_time` from the
    /// `current_block_time` is calculated. The value is added to the `current_height` to calculate
    /// the final estimate.
    fn estimate_height_at_with_current_target_spacing(
        self,
        target_time: DateTime<Utc>,
    ) -> block::Height {
        let time_difference = target_time - self.current_block_time;
        let mut time_difference_seconds = time_difference.num_seconds();

        if time_difference_seconds < 0 {
            // Undo the rounding towards negative infinity done by `chrono::Duration`, which yields
            // an incorrect value for the dividend of the division.
            //
            // (See https://docs.rs/time/0.1.44/src/time/duration.rs.html#166-173)
            time_difference_seconds -= 1;
        }

        // Euclidean division is used so that the number is rounded towards negative infinity,
        // so that fractionary values always round down to the previous height when going back
        // in time (i.e., when the dividend is negative). This works because the divisor (the
        // target spacing) is always positive.
        let block_difference: HeightDiff =
            time_difference_seconds.div_euclid(self.current_target_spacing.num_seconds());

        let current_height_as_diff = HeightDiff::from(self.current_height.0);

        if let Some(height_estimate) = self.current_height + block_difference {
            height_estimate
        } else if current_height_as_diff + block_difference < 0 {
            // Gracefully handle attempting to estimate a block before genesis. This can happen if
            // the local time is set incorrectly to a time too far in the past.
            block::Height(0)
        } else {
            // Gracefully handle attempting to estimate a block at a very large height. This can
            // happen if the local time is set incorrectly to a time too far in the future.
            block::Height::MAX
        }
    }
}
