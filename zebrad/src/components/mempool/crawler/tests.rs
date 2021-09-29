use std::time::Duration;

use proptest::{collection::vec, prelude::*};
use tokio::time;

use zebra_chain::transaction::UnminedTxId;
use zebra_network as zn;
use zebra_test::mock_service::{MockService, PropTestAssertion};

use super::{
    super::{
        super::{mempool, sync::RecentSyncLengths},
        downloads::Gossip,
        error::MempoolError,
    },
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

/// A [`MockService`] representing the network service.
type MockPeerSet = MockService<zn::Request, zn::Response, PropTestAssertion>;

/// A [`MockService`] representing the mempool service.
type MockMempool = MockService<mempool::Request, mempool::Response, PropTestAssertion>;

proptest! {
    /// Test if crawler periodically crawls for transaction IDs.
    ///
    /// The crawler should periodically perform a fanned-out series of requests to obtain
    /// transaction IDs from other peers. These requests should only be sent if the mempool is
    /// enabled, i.e., if the block synchronizer is likely close to the chain tip.
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
            let (mut peer_set, _mempool, sync_status, mut recent_sync_lengths) = setup_crawler();

            time::pause();

            for sync_length in sync_lengths {
                let mempool_is_enabled = sync_status.is_close_to_tip();

                for _ in 0..CRAWL_ITERATIONS {
                    for _ in 0..FANOUT {
                        if mempool_is_enabled {
                            respond_with_transaction_ids(&mut peer_set, vec![]).await?;
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

        let transaction_id_count = transaction_ids.len();

        runtime.block_on(async move {
            let (mut peer_set, mut mempool, _sync_status, mut recent_sync_lengths) =
                setup_crawler();

            time::pause();

            // Mock end of chain sync to enable the mempool crawler.
            SyncStatus::sync_close_to_tip(&mut recent_sync_lengths);

            crawler_iteration(&mut peer_set, vec![transaction_ids.clone()]).await?;

            respond_to_queue_request(
                &mut mempool,
                transaction_ids,
                vec![Ok(()); transaction_id_count],
            ).await?;

            mempool.expect_no_requests().await?;

            Ok::<(), TestCaseError>(())
        })?;
    }
}

/// Spawn a crawler instance using mock services.
fn setup_crawler() -> (MockPeerSet, MockMempool, SyncStatus, RecentSyncLengths) {
    let peer_set = MockService::build().for_prop_tests();
    let mempool = MockService::build().for_prop_tests();
    let (sync_status, recent_sync_lengths) = SyncStatus::new();

    Crawler::spawn(peer_set.clone(), mempool.clone(), sync_status.clone());

    (peer_set, mempool, sync_status, recent_sync_lengths)
}

/// Intercept a request for mempool transaction IDs and respond with the `transaction_ids` list.
async fn respond_with_transaction_ids(
    peer_set: &mut MockPeerSet,
    transaction_ids: Vec<UnminedTxId>,
) -> Result<(), TestCaseError> {
    peer_set
        .expect_request(zn::Request::MempoolTransactionIds)
        .await?
        .respond(zn::Response::TransactionIds(transaction_ids));

    Ok(())
}

/// Intercept fanned-out requests for mempool transaction IDs and answer with the `responses`.
///
/// Each item in `responses` is a list of transaction IDs to send back to a single request.
/// Therefore, each item represents the response sent by a peer in the network.
///
/// If there are less items in `responses` the [`FANOUT`] number, then the remaining requests are
/// answered with an empty list of transaction IDs.
///
/// # Panics
///
/// If `responses` contains more items than the [`FANOUT`] number.
async fn crawler_iteration(
    peer_set: &mut MockPeerSet,
    responses: Vec<Vec<UnminedTxId>>,
) -> Result<(), TestCaseError> {
    let empty_responses = FANOUT
        .checked_sub(responses.len())
        .expect("Too many responses to be sent in a single crawl iteration");

    for response in responses {
        respond_with_transaction_ids(peer_set, response).await?;
    }

    for _ in 0..empty_responses {
        respond_with_transaction_ids(peer_set, vec![]).await?;
    }

    peer_set.expect_no_requests().await?;

    Ok(())
}

/// Intercept request for mempool to download and verify transactions.
///
/// The intercepted request will be verified to check if it has the `expected_transaction_ids`, and
/// it will be answered with a list of results, one for each transaction requested to be
/// downloaded.
///
/// # Panics
///
/// If `response` and `expected_transaction_ids` have different sizes.
async fn respond_to_queue_request(
    mempool: &mut MockMempool,
    expected_transaction_ids: Vec<UnminedTxId>,
    response: Vec<Result<(), MempoolError>>,
) -> Result<(), TestCaseError> {
    let request_parameter = expected_transaction_ids
        .into_iter()
        .map(Gossip::Id)
        .collect();

    mempool
        .expect_request(mempool::Request::Queue(request_parameter))
        .await?
        .respond(mempool::Response::Queued(response));

    Ok(())
}
