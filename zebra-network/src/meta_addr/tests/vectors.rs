//! Test vectors for MetaAddr.

use super::{super::MetaAddr, check};

use chrono::{MAX_DATETIME, MIN_DATETIME};

/// Make sure that the sanitize function handles minimum and maximum times.
#[test]
fn sanitize_extremes() {
    zebra_test::init();

    let min_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        last_seen: MIN_DATETIME,
        last_connection_state: Default::default(),
    };

    let max_time_entry = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        services: Default::default(),
        last_seen: MAX_DATETIME,
        last_connection_state: Default::default(),
    };

    check::sanitize_avoids_leaks(&min_time_entry);
    check::sanitize_avoids_leaks(&max_time_entry);
}
