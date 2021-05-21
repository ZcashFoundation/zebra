//! Shared test checks for MetaAddr

use super::super::{MetaAddr, PeerAddrState::NeverAttemptedGossiped};

use crate::constants::TIMESTAMP_TRUNCATION_SECONDS;

/// Make sure that the sanitize function reduces time and state metadata
/// leaks.
pub(crate) fn sanitize_avoids_leaks(entry: &MetaAddr) {
    let sanitized = match entry.sanitize() {
        Some(sanitized) => sanitized,
        // Skip addresses that will never be sent to peers
        None => {
            return;
        }
    };

    // We want the sanitized timestamp to:
    // - be a multiple of the truncation interval,
    // - have a zero nanoseconds component, and
    // - be within the truncation interval of the original timestamp.
    if let Some(sanitized_last) = sanitized.get_last_success_or_untrusted() {
        assert_eq!(sanitized_last.timestamp() % TIMESTAMP_TRUNCATION_SECONDS, 0);
        assert_eq!(sanitized_last.timestamp_subsec_nanos(), 0);
        // handle underflow and overflow by skipping the check
        // the other check will ensure correctness
        let lowest_time = entry
            .get_last_success_or_untrusted()
            .map(|t| t.timestamp().checked_sub(TIMESTAMP_TRUNCATION_SECONDS))
            .flatten();
        let highest_time = entry
            .get_last_success_or_untrusted()
            .map(|t| t.timestamp().checked_add(TIMESTAMP_TRUNCATION_SECONDS))
            .flatten();
        if let Some(lowest_time) = lowest_time {
            assert!(sanitized_last.timestamp() > lowest_time);
        }
        if let Some(highest_time) = highest_time {
            assert!(sanitized_last.timestamp() < highest_time);
        }
    }

    // Sanitize to the the gossiped state, even though it's not serialized
    assert!(matches!(
        sanitized.last_connection_state,
        NeverAttemptedGossiped { .. }
    ));

    // We want the other fields to be unmodified
    assert_eq!(sanitized.addr, entry.addr);
    // Services are sanitized during parsing, so we don't need to make
    // any changes in sanitize()
    assert_eq!(
        sanitized.get_untrusted_services(),
        entry.get_untrusted_services()
    );
}
