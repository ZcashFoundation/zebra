use std::time::Duration;

use proptest::{collection::vec, prelude::*};
use tokio::time;

use zebra_chain::transaction::UnminedTxId;
use zebra_network as zn;
use zebra_test::mock_service::MockService;

use super::{
    super::{super::mempool, downloads::Gossip},
    Crawler, SyncStatus, FANOUT, RATE_LIMIT_DELAY,
};

/// The number of iterations to crawl while testing.
///
/// Note that this affects the total run time of the [`crawler_requests_for_transaction_ids`] test.
/// There are [`CRAWL_ITERATIONS`] requests that are expected to not be sent, so the test runs for
/// at least `CRAWL_ITERATIONS` times the timeout for receiving a request (see more information in
/// [`MockServiceBuilder::with_max_request_delay`]).
const CRAWL_ITERATIONS: usize = 4;

/// The maximum number of transactions crawled from a mocked peer.
const MAX_CRAWLED_TX: usize = 10;

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
            let mempool = MockService::build().for_prop_tests();
            let (sync_status, mut recent_sync_lengths) = SyncStatus::new();

            time::pause();

            Crawler::spawn(peer_set.clone(), mempool, sync_status.clone());

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

    /// Test if crawled transactions are forwarded to the [`Mempool`][mempool::Mempool] service.
    ///
    /// The transaction IDs sent by other peers to the crawler should be forwarded to the
    /// [`Mempool`][mempool::Mempool] service so that they can be downloaded, verified and added to
    /// the mempool.
    #[test]
    fn crawled_transactions_are_forwarded_to_downloader(
        transaction_ids in vec(any::<UnminedTxId>(), 1..MAX_CRAWLED_TX),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create Tokio runtime");
        let _guard = runtime.enter();

        runtime.block_on(async move {
            let mut peer_set = MockService::build().for_prop_tests();
            let mut mempool = MockService::build().for_prop_tests();
            let (sync_status, mut recent_sync_lengths) = SyncStatus::new();

            time::pause();

            // Mock end of chain sync to enable the mempool crawler.
            SyncStatus::sync_close_to_tip(&mut recent_sync_lengths);

            Crawler::spawn(peer_set.clone(), mempool.clone(), sync_status);

            peer_set
                .expect_request_that(|request| {
                    matches!(request, zn::Request::MempoolTransactionIds)
                })
                .await?
                .respond(zn::Response::TransactionIds(transaction_ids.clone()));

            for _ in 1..FANOUT {
                peer_set
                    .expect_request_that(|request| {
                        matches!(request, zn::Request::MempoolTransactionIds)
                    })
                    .await?
                    .respond(zn::Response::TransactionIds(vec![]));
            }

            peer_set.expect_no_requests().await?;

            let response_list = vec![Ok(()); transaction_ids.len()];
            let request_param = transaction_ids.into_iter().map(Gossip::Id).collect();

            mempool
                .expect_request(mempool::Request::Queue(request_param))
                .await?
                .respond(mempool::Response::Queued(response_list));

            mempool.expect_no_requests().await?;

            Ok::<(), TestCaseError>(())
        })?;
    }
}
