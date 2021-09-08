use std::time::Duration;

use proptest::prelude::*;
use tokio::time;

use zebra_network as zn;
use zebra_test::mock_service::MockService;

use super::{Crawler, SyncStatus, FANOUT, RATE_LIMIT_DELAY};

/// The number of iterations to crawl while testing.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// There are [`CRAWL_ITERATIONS`] requests that are expected to not be sent, so the test runs for
/// at least `CRAWL_ITERATIONS` times the timeout for receiving a request (see more information in
/// [`MockServiceBuilder::with_max_request_delay`]).
const CRAWL_ITERATIONS: usize = 4;

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
            let mut peer_set = MockService::build().for_prop_tests();
            let (sync_status, mut recent_sync_lengths) = SyncStatus::new();

            time::pause();

            Crawler::spawn(peer_set.clone(), sync_status.clone());

            for sync_length in sync_lengths {
                let mempool_is_enabled = sync_status.is_close_to_tip();

                for _ in 0..CRAWL_ITERATIONS {
                    for _ in 0..FANOUT {
                        if mempool_is_enabled {
                            peer_set
                                .expect_request_that(|request| {
                                    matches!(request, zn::Request::MempoolTransactionIds)
                                })
                                .await?
                                .respond(zn::Response::TransactionIds(vec![]));
                        } else {
                            peer_set.expect_no_requests().await?;
                        }
                    }

                    peer_set.expect_no_requests().await?;

                    time::sleep(RATE_LIMIT_DELAY + ERROR_MARGIN).await;
                }

                // Applying the update event at the end of the test iteration means that the first
                // iteration runs with an empty recent sync. lengths vector. A dummy element is
                // appended to the events so that all of the original values are applied.
                recent_sync_lengths.push_extend_tips_length(sync_length);
            }

            Ok::<(), TestCaseError>(())
        })?;
    }
}
