//! Shared test checks for MetaAddr

use super::super::MetaAddr;

use crate::{constants::TIMESTAMP_TRUNCATION_SECONDS, types::PeerServices};

/// Check that:
/// - the only times that are included in serialized `MetaAddr`s are the last
///   responded time, and the untrusted last seen time, and
/// - `sanitized_addr` has less time and state metadata than `original_addr`.
///
/// Also checks that the time hasn't changed too much.
pub(crate) fn sanitize_avoids_leaks(original: &MetaAddr, sanitized: &MetaAddr) {
    // check that irrelevant times are cleared by sanitization
    assert_eq!(sanitized.last_attempt, None);
    assert_eq!(sanitized.last_failure, None);

    // check that the last seen and responded times end up in the untrusted field
    // (but ignore their values for now - those checks are next)
    assert_eq!(sanitized.last_response, None);
    assert_eq!(
        sanitized.untrusted_last_seen.is_some(),
        original
            .last_response
            .or(original.untrusted_last_seen)
            .is_some()
    );
    // Check the responded field overrides the untrusted field
    assert_eq!(
        original.last_seen(),
        original.last_response.or(original.untrusted_last_seen)
    );

    // check for time truncation
    if let Some(sanitized_last_seen) = sanitized.last_seen() {
        // We want the sanitized timestamp to:
        // - be a multiple of the truncation interval,
        // - have a zero nanoseconds component, and
        // - be within the truncation interval of the original timestamp.
        assert_eq!(
            sanitized_last_seen.timestamp() % TIMESTAMP_TRUNCATION_SECONDS,
            0
        );

        // check that the times haven't changed too much
        let original_last_seen = original
            .last_seen()
            .expect("unexpected missing original last seen when sanitized last seen is Some");

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

    // check the other fields

    // Sanitize to the the default state, even though it's not serialized
    assert_eq!(sanitized.last_connection_state, Default::default());
    // Sanitize to known flags
    assert_eq!(sanitized.services, original.services & PeerServices::all());

    // We want the other fields to be unmodified
    assert_eq!(sanitized.addr, original.addr);
}
