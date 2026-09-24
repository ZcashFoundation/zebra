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
    target_spacings: vec::IntoIter<(block::Height, Duration)>,
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
    /// The current height determines the initial target spacing. Past and future spacing changes
    /// are retained so estimates can cross transitions in either direction.
    pub fn new(
        current_block_time: DateTime<Utc>,
        current_height: block::Height,
        network: &Network,
    ) -> Self {
        NetworkChainTipHeightEstimator {
            current_block_time,
            current_height,
            current_target_spacing: NetworkUpgrade::target_spacing_for_height(
                network,
                current_height,
            ),
            // TODO: Remove the `Vec` allocation once existential `impl Trait`s are available.
            target_spacings: NetworkUpgrade::target_spacings(network)
                .collect::<Vec<_>>()
                .into_iter(),
        }
    }

    /// Estimate the network chain tip height at the provided `target_time`.
    ///
    /// # Implementation details
    ///
    /// The reference time and height move through each spacing era towards `target_time`.
    /// The interval ending at an activation block uses that upgrade's spacing in both directions.
    /// Once the target era is reached, its spacing is used to calculate the final height.
    pub fn estimate_height_at(mut self, target_time: DateTime<Utc>) -> block::Height {
        if target_time < self.current_block_time {
            while let Some((change_height, target_spacing)) = self.target_spacings.next_back() {
                if change_height > self.current_height {
                    continue;
                }

                self.current_target_spacing = target_spacing;
                self.estimate_at((change_height - 1).unwrap_or(block::Height(0)));

                if self.current_block_time <= target_time {
                    break;
                }
            }
        } else {
            while let Some((change_height, next_target_spacing)) = self.target_spacings.next() {
                if change_height <= self.current_height {
                    continue;
                }

                self.estimate_at(
                    (change_height - 1).expect("future spacing changes are after genesis"),
                );

                if self.current_block_time >= target_time {
                    break;
                }

                self.current_target_spacing = next_target_spacing;
            }
        }

        self.estimate_height_at_with_current_target_spacing(target_time)
    }

    /// Move the reference time and height to an era boundary using the current target spacing.
    fn estimate_at(&mut self, height: block::Height) {
        let remaining_blocks = height - self.current_height;
        let target_spacing_seconds = self.current_target_spacing.num_seconds();
        self.current_block_time += Duration::seconds(remaining_blocks * target_spacing_seconds);
        self.current_height = height;
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

        if time_difference.subsec_nanos() < 0 {
            // Chrono truncates whole seconds towards zero. Floor negative fractions before
            // dividing by the spacing, without changing exact negative seconds.
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
