//! Shared test checks for MetaAddr

use super::super::MetaAddr;

use crate::{constants::TIMESTAMP_TRUNCATION_SECONDS, types::PeerServices};

/// Check that `sanitized_addr` has less time and state metadata than
/// `original_addr`.
///
/// Also check that the time hasn't changed too much.
pub(crate) fn sanitize_avoids_leaks(original: &MetaAddr, sanitized: &MetaAddr) {
    if let Some(sanitized_last_seen) = sanitized.last_seen() {
        // We want the sanitized timestamp to:
        // - be a multiple of the truncation interval,
        // - have a zero nanoseconds component, and
        // - be within the truncation interval of the original timestamp.
        assert_eq!(
            sanitized_last_seen.timestamp() % TIMESTAMP_TRUNCATION_SECONDS,
            0
        );

        if let Some(original_last_seen) = original.last_seen() {
            // handle underflow and overflow by skipping the check
            // the other check will ensure correctness
            let lowest_time = original_last_seen
                .timestamp()
                .checked_sub(TIMESTAMP_TRUNCATION_SECONDS);
            let highest_time = original_last_seen
                .timestamp()
                .checked_add(TIMESTAMP_TRUNCATION_SECONDS);
            if let Some(lowest_time) = lowest_time {
                assert!(sanitized_last_seen.timestamp() > lowest_time);
            }
            if let Some(highest_time) = highest_time {
                assert!(sanitized_last_seen.timestamp() < highest_time);
            }
        }
    }

    // Sanitize to the the default state, even though it's not serialized
    assert_eq!(sanitized.last_connection_state, Default::default());
    // Sanitize to known flags
    assert_eq!(sanitized.services, original.services & PeerServices::all());

    // We want the other fields to be unmodified
    assert_eq!(sanitized.addr, original.addr);
}
