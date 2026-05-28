//! Timing tests for the mempool crawler.

use zebra_chain::parameters::POST_NU7_POW_TARGET_SPACING;
use zebra_network::constants::DEFAULT_CRAWL_NEW_PEER_INTERVAL;

use crate::components::mempool::crawler::RATE_LIMIT_DELAY;

#[test]
fn ensure_timing_consistent() {
    assert!(
        RATE_LIMIT_DELAY.as_secs() < POST_NU7_POW_TARGET_SPACING.into(),
        "a mempool crawl should complete before most new blocks"
    );

    assert!(
        RATE_LIMIT_DELAY < DEFAULT_CRAWL_NEW_PEER_INTERVAL,
        "the mempool crawler intentionally runs more frequently than the peer address crawler"
    );
}
