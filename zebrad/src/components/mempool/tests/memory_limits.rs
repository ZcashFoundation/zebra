//! Tests for the mempool limits a memory budget derives.

use crate::components::mempool::{Config, MempoolMemoryLimits};

/// A cost limit a 2 GiB memory budget derives.
const DERIVED: u64 = 54_000_000;

/// The default limits keep every config, default or not, unchanged.
#[test]
fn default_memory_limits_keep_the_mempool_config() {
    let _init_guard = zebra_test::init();

    for config in [
        Config::default(),
        Config {
            tx_cost_limit: 30_000_000,
            debug_enable_at_height: Some(1),
            ..Config::default()
        },
    ] {
        assert_eq!(MempoolMemoryLimits::default().apply_to(&config), config);
    }
}

/// The derived cost limit also bounds pending bytes; the other public settings are unchanged.
#[test]
fn cost_limit_replaces_the_configured_limit() {
    let _init_guard = zebra_test::init();
    let limits = MempoolMemoryLimits {
        tx_cost_limit: Some(DERIVED),
    };

    for configured in [Config::default().tx_cost_limit, 30_000_000, 100_000_000] {
        let config = Config {
            tx_cost_limit: configured,
            debug_enable_at_height: Some(1),
            ..Config::default()
        };
        assert_eq!(
            limits.apply_to(&config),
            Config {
                tx_cost_limit: DERIVED,
                pending_bytes: Some(DERIVED),
                ..config
            },
            "the limits replace a configured cost limit of {configured}"
        );
    }
}
