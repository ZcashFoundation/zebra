//! Test vectors for MetaAddr.

use super::{
    super::{MetaAddr, PeerAddrState},
    check,
};

use chrono::{MAX_DATETIME, MIN_DATETIME};

/// Make sure that the sanitize function handles minimum and maximum times.
#[test]
fn sanitize_extremes() {
    use PeerAddrState::*;

    zebra_test::init();

    let min_time_untrusted = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        last_connection_state: NeverAttemptedGossiped {
            untrusted_last_seen: MIN_DATETIME,
            untrusted_services: Default::default(),
        },
    };

    let min_time_local = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        last_connection_state: Responded {
            last_attempt: MIN_DATETIME,
            last_success: MIN_DATETIME,
            last_failed: Some(MIN_DATETIME),
            untrusted_last_seen: Some(MIN_DATETIME),
            services: Default::default(),
        },
    };

    let max_time_untrusted = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        last_connection_state: NeverAttemptedGossiped {
            untrusted_last_seen: MAX_DATETIME,
            untrusted_services: Default::default(),
        },
    };

    let max_time_local = MetaAddr {
        addr: "127.0.0.1:8233".parse().unwrap(),
        last_connection_state: Responded {
            last_attempt: MAX_DATETIME,
            last_success: MAX_DATETIME,
            last_failed: Some(MAX_DATETIME),
            untrusted_last_seen: Some(MAX_DATETIME),
            services: Default::default(),
        },
    };

    check::sanitize_avoids_leaks(&min_time_untrusted);
    check::sanitize_avoids_leaks(&min_time_local);
    check::sanitize_avoids_leaks(&max_time_untrusted);
    check::sanitize_avoids_leaks(&max_time_local);
}
