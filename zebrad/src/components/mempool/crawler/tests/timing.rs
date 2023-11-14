//! Timing tests for the mempool crawler.

use zebra_chain::parameters::POST_BLOSSOM_POW_TARGET_SPACING;
use zebra_network::constants::{DEFAULT_CRAWL_NEW_PEER_INTERVAL, HANDSHAKE_TIMEOUT};

use crate::components::mempool::crawler::RATE_LIMIT_DELAY;

#[test]
fn ensure_timing_consistent() {
    assert!(
        RATE_LIMIT_DELAY.as_secs() < POST_BLOSSOM_POW_TARGET_SPACING.into(),
        "a mempool crawl should complete before most new blocks"
    );

    // The default peer crawler interval should be at least
    // `HANDSHAKE_TIMEOUT` lower than all other crawler intervals.
    //
    // See `DEFAULT_CRAWL_NEW_PEER_INTERVAL` for details.
    assert!(
        DEFAULT_CRAWL_NEW_PEER_INTERVAL.as_secs() + HANDSHAKE_TIMEOUT.as_secs()
            < RATE_LIMIT_DELAY.as_secs(),
        "an address crawl and peer connections should complete before most syncer restarts"
    );
}
