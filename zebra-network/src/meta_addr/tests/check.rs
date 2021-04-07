//! Shared test checks for MetaAddr

use super::super::MetaAddr;

use crate::constants::TIMESTAMP_TRUNCATION_SECONDS;

/// Make sure that the sanitize function reduces time and state metadata
/// leaks.
pub(crate) fn sanitize_avoids_leaks(entry: &MetaAddr) {
    let sanitized = entry.sanitize();

    // We want the sanitized timestamp to:
    // - be a multiple of the truncation interval,
    // - have a zero nanoseconds component, and
    // - be within the truncation interval of the original timestamp.
    assert_eq!(
        sanitized.get_last_seen().timestamp() % TIMESTAMP_TRUNCATION_SECONDS,
        0
    );
    assert_eq!(sanitized.get_last_seen().timestamp_subsec_nanos(), 0);
    // handle underflow and overflow by skipping the check
    // the other check will ensure correctness
    let lowest_time = entry
        .get_last_seen()
        .timestamp()
        .checked_sub(TIMESTAMP_TRUNCATION_SECONDS);
    let highest_time = entry
        .get_last_seen()
        .timestamp()
        .checked_add(TIMESTAMP_TRUNCATION_SECONDS);
    if let Some(lowest_time) = lowest_time {
        assert!(sanitized.get_last_seen().timestamp() > lowest_time);
    }
    if let Some(highest_time) = highest_time {
        assert!(sanitized.get_last_seen().timestamp() < highest_time);
    }

    // Sanitize to the the default state, even though it's not serialized
    assert_eq!(sanitized.last_connection_state, Default::default());
    // We want the other fields to be unmodified
    assert_eq!(sanitized.addr, entry.addr);
    // Services are sanitized during parsing, so we don't need to make
    // any changes in sanitize()
    assert_eq!(sanitized.services, entry.services);
}
