use std::time::Duration;

use proptest::prelude::*;
use tokio::time::{self, timeout};

use zebra_network::Request;

use crate::components::tests::mock_peer_set;

use super::{Crawler, SyncStatus, FANOUT, RATE_LIMIT_DELAY};

/// The number of iterations to crawl while testing.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// See more information in [`MAX_REQUEST_DELAY`].
const CRAWL_ITERATIONS: usize = 4;

/// The maximum time to wait for a request to arrive before considering it won't arrive.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// There are [`CRAWL_ITERATIONS`] requests that are expected to not be sent, so the test runs for
/// at least `MAX_REQUEST_DELAY * CRAWL_ITERATIONS`.
const MAX_REQUEST_DELAY: Duration = Duration::from_millis(25);

/// The amount of time to advance beyond the expected instant that the crawler wakes up.
const ERROR_MARGIN: Duration = Duration::from_millis(100);

proptest! {
    #[test]
    fn crawler_requests_for_transaction_ids(mut sync_lengths in any::<Vec<usize>>()) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");
        let _guard = runtime.enter();

        // Add a dummy last element, so that all of the original values are used.
        sync_lengths.push(0);

        runtime.block_on(async move {
            let (peer_set, mut requests) = mock_peer_set();
            let (sync_status, mut recent_sync_lengths) = SyncStatus::new();

            time::pause();

            Crawler::spawn(peer_set, sync_status.clone());

            for sync_length in sync_lengths {
                let mempool_is_enabled = sync_status.is_close_to_tip();

                for _ in 0..CRAWL_ITERATIONS {
                    for _ in 0..FANOUT {
                        let request = timeout(MAX_REQUEST_DELAY, requests.recv()).await;

                        if mempool_is_enabled {
                            prop_assert!(matches!(request, Ok(Some(Request::MempoolTransactionIds))));
                        } else {
                            prop_assert!(request.is_err());
                        }
                    }

                    let extra_request = timeout(MAX_REQUEST_DELAY, requests.recv()).await;

                    prop_assert!(extra_request.is_err());

                    time::sleep(RATE_LIMIT_DELAY + ERROR_MARGIN).await;
                }

                // Applying the update event at the end of the test iteration means that the first
                // iteration runs with an empty recent sync. lengths vector. A dummy element is
                // appended to the events so that all of the original values are applied.
                recent_sync_lengths.push_extend_tips_length(sync_length);
            }

            Ok(())
        })?;
    }
}
