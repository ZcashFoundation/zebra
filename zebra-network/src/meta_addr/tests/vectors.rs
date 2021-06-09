//! Test vectors for MetaAddr.

use super::{super::MetaAddr, check};

/// Make sure that the sanitize function handles minimum and maximum times.
#[test]
fn sanitize_extremes() {
    zebra_test::init();

    let min_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        untrusted_last_seen: Some(u32::MIN.into()),
        last_response: Some(u32::MIN.into()),
        last_attempt: None,
        last_failure: None,
        last_connection_state: Default::default(),
    };

    let max_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        untrusted_last_seen: Some(u32::MAX.into()),
        last_response: Some(u32::MAX.into()),
        last_attempt: None,
        last_failure: None,
        last_connection_state: Default::default(),
    };

    if let Some(min_sanitized) = min_time_entry.sanitize() {
        check::sanitize_avoids_leaks(&min_time_entry, &min_sanitized);
    }
    if let Some(max_sanitized) = max_time_entry.sanitize() {
        check::sanitize_avoids_leaks(&max_time_entry, &max_sanitized);
    }
}
