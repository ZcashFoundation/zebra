//! Block validity checks

use super::Error;

use chrono::{DateTime, Duration, Utc};

/// Check if `block_header_time` is less than or equal to
/// 2 hours in the future, according to the node's local clock (`now`).
///
/// This is a non-deterministic rule, as clocks vary over time, and
/// between different nodes.
///
/// "In addition, a full validator MUST NOT accept blocks with nTime
/// more than two hours in the future according to its clock. This
/// is not strictly a consensus rule because it is nondeterministic,
/// and clock time varies between nodes. Also note that a block that
/// is rejected by this rule at a given point in time may later be
/// accepted."[S 7.5][7.5]
///
/// [7.5]: https://zips.z.cash/protocol/protocol.pdf#blockheader
pub(super) fn node_time_check(
    block_header_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), Error> {
    let two_hours_in_the_future = now
        .checked_add_signed(Duration::hours(2))
        .ok_or("overflow when calculating 2 hours in the future")?;

    if block_header_time <= two_hours_in_the_future {
        Ok(())
    } else {
        Err("block header time is more than 2 hours in the future".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::offset::{LocalResult, TimeZone};
    use std::sync::Arc;

    use zebra_chain::block::Block;
    use zebra_chain::serialization::ZcashDeserialize;

    #[test]
    fn time_check_past_block() {
        // This block is also verified as part of the BlockVerifier service
        // tests.
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
                .expect("block should deserialize");
        let now = Utc::now();

        // This check is non-deterministic, but BLOCK_MAINNET_415000 is
        // a long time in the past. So it's unlikely that the test machine
        // will have a clock that's far enough in the past for the test to
        // fail.
        node_time_check(block.header.time, now)
            .expect("the header time from a mainnet block should be valid");
    }

    #[test]
    fn time_check_now() {
        // These checks are deteministic, because all the times are offset
        // from the current time.
        let now = Utc::now();
        let three_hours_in_the_past = now - Duration::hours(3);
        let two_hours_in_the_future = now + Duration::hours(2);
        let two_hours_and_one_second_in_the_future =
            now + Duration::hours(2) + Duration::seconds(1);

        node_time_check(now, now).expect("the current time should be valid as a block header time");
        node_time_check(three_hours_in_the_past, now)
            .expect("a past time should be valid as a block header time");
        node_time_check(two_hours_in_the_future, now)
            .expect("2 hours in the future should be valid as a block header time");
        node_time_check(two_hours_and_one_second_in_the_future, now).expect_err(
            "2 hours and 1 second in the future should be invalid as a block header time",
        );

        // Now invert the tests
        // 3 hours in the future should fail
        node_time_check(now, three_hours_in_the_past)
            .expect_err("3 hours in the future should be invalid as a block header time");
        // The past should succeed
        node_time_check(now, two_hours_in_the_future)
            .expect("2 hours in the past should be valid as a block header time");
        node_time_check(now, two_hours_and_one_second_in_the_future)
            .expect("2 hours and 1 second in the past should be valid as a block header time");
    }

    /// Valid unix epoch timestamps for blocks, in seconds
    static BLOCK_HEADER_VALID_TIMESTAMPS: &[i64] = &[
        // These times are currently invalid DateTimes, but they could
        // become valid in future chrono versions
        i64::MIN,
        i64::MIN + 1,
        // These times are valid DateTimes
        (u32::MIN as i64) - 1,
        (u32::MIN as i64),
        (u32::MIN as i64) + 1,
        (i32::MIN as i64) - 1,
        (i32::MIN as i64),
        (i32::MIN as i64) + 1,
        -1,
        0,
        1,
        // maximum nExpiryHeight or lock_time, in blocks
        499_999_999,
        // minimum lock_time, in seconds
        500_000_000,
        500_000_001,
    ];

    /// Invalid unix epoch timestamps for blocks, in seconds
    static BLOCK_HEADER_INVALID_TIMESTAMPS: &[i64] = &[
        (i32::MAX as i64) - 1,
        (i32::MAX as i64),
        (i32::MAX as i64) + 1,
        (u32::MAX as i64) - 1,
        (u32::MAX as i64),
        (u32::MAX as i64) + 1,
        // These times are currently invalid DateTimes, but they could
        // become valid in future chrono versions
        i64::MAX - 1,
        i64::MAX,
    ];

    #[test]
    fn time_check_fixed() {
        // These checks are non-deterministic, but the times are all in the
        // distant past or far future. So it's unlikely that the test
        // machine will have a clock that makes these tests fail.
        let now = Utc::now();

        for valid_timestamp in BLOCK_HEADER_VALID_TIMESTAMPS {
            let block_header_time = match Utc.timestamp_opt(*valid_timestamp, 0) {
                LocalResult::Single(time) => time,
                LocalResult::None => {
                    // Skip the test if the timestamp is invalid
                    continue;
                }
                LocalResult::Ambiguous(_, _) => {
                    // Utc doesn't have ambiguous times
                    unreachable!();
                }
            };
            node_time_check(block_header_time, now)
                .expect("the time should be valid as a block header time");
            // Invert the check, leading to an invalid time
            node_time_check(now, block_header_time)
                .expect_err("the inverse comparison should be invalid");
        }

        for invalid_timestamp in BLOCK_HEADER_INVALID_TIMESTAMPS {
            let block_header_time = match Utc.timestamp_opt(*invalid_timestamp, 0) {
                LocalResult::Single(time) => time,
                LocalResult::None => {
                    // Skip the test if the timestamp is invalid
                    continue;
                }
                LocalResult::Ambiguous(_, _) => {
                    // Utc doesn't have ambiguous times
                    unreachable!();
                }
            };
            node_time_check(block_header_time, now)
                .expect_err("the time should be invalid as a block header time");
            // Invert the check, leading to a valid time
            node_time_check(now, block_header_time)
                .expect("the inverse comparison should be valid");
        }
    }
}
