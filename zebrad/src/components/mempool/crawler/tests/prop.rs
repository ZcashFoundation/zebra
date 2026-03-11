//! Randomised property tests for the mempool crawler.

use std::{collections::HashSet, time::Duration};

use proptest::{
    collection::{hash_set, vec},
    prelude::*,
};
use tokio::{sync::oneshot, time};

use zebra_chain::{
    chain_sync_status::ChainSyncStatus, parameters::Network, transaction::UnminedTxId,
};
use zebra_network as zn;
use zebra_node_services::mempool::Gossip;
use zebra_state::ChainTipSender;
use zebra_test::mock_service::{MockService, PropTestAssertion};

use crate::{
    components::{
        mempool::{
            self,
            crawler::{Crawler, SyncStatus, FANOUT, RATE_LIMIT_DELAY},
            error::MempoolError,
            storage::{
                NonStandardTransactionError, SameEffectsChainRejectionError,
                SameEffectsTipRejectionError,
            },
            Config,
        },
        sync::RecentSyncLengths,
    },
    BoxError,
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

fn mempool_error_strategy() -> BoxedStrategy<MempoolError> {
    prop_oneof![
        Just(MempoolError::InMempool),
        Just(MempoolError::AlreadyQueued),
        Just(MempoolError::FullQueue),
        Just(MempoolError::Disabled),
        Just(MempoolError::StorageEffectsTip(
            SameEffectsTipRejectionError::SpendConflict
        )),
        Just(MempoolError::StorageEffectsTip(
            SameEffectsTipRejectionError::MissingOutput
        )),
        Just(MempoolError::StorageEffectsChain(
            SameEffectsChainRejectionError::RandomlyEvicted
        )),
        Just(MempoolError::NonStandardTransaction(
            NonStandardTransactionError::IsDust
        )),
    ]
    .boxed()
}

fn mempool_error_result_strategy() -> BoxedStrategy<Result<(), MempoolError>> {
    prop_oneof![Just(Ok(())), mempool_error_strategy().prop_map(Err)].boxed()
}

proptest! {
    /// Test if crawler periodically crawls for transaction IDs.
    ///
    /// The crawler should periodically perform a fanned-out series of requests to obtain
    /// transaction IDs from other peers. These requests should only be sent if the mempool is
    /// enabled, i.e., if the block synchronizer is likely close to the chain tip.
    #[test]
    fn crawler_requests_for_transaction_ids(mut sync_lengths in any::<Vec<usize>>()) {
        let (runtime, _init_guard) = zebra_test::init_async();

        // Add a dummy last element, so that all of the original values are used.
        sync_lengths.push(0);

        runtime.block_on(async move {
            let (mut peer_set, _mempool, sync_status, mut recent_sync_lengths, _chain_tip_sender,
 ) = setup_crawler();

            time::pause();

            for sync_length in sync_lengths {
                let mempool_is_enabled = sync_status.is_close_to_tip();

                for _ in 0..CRAWL_ITERATIONS {
                    for _ in 0..FANOUT {
                        if mempool_is_enabled {
                            respond_with_transaction_ids(&mut peer_set, HashSet::new()).await?;
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
        transaction_ids in hash_set(any::<UnminedTxId>(), 1..MAX_CRAWLED_TX),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        let transaction_id_count = transaction_ids.len();

        runtime.block_on(async move {
            let (mut peer_set, mut mempool, _sync_status, mut recent_sync_lengths, _chain_tip_sender) =
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

    /// Test if errors while forwarding transaction IDs do not stop the crawler.
    ///
    /// The crawler should continue operating normally if some transactions fail to download or
    /// even if the mempool service fails to enqueue the transactions to be downloaded.
    #[test]
    fn transaction_id_forwarding_errors_dont_stop_the_crawler(
        service_call_error in mempool_error_strategy(),
        transaction_ids_for_call_failure in hash_set(any::<UnminedTxId>(), 1..MAX_CRAWLED_TX),
        transaction_ids_and_responses in
            vec((any::<UnminedTxId>(), mempool_error_result_strategy()), 1..MAX_CRAWLED_TX),
        transaction_ids_for_return_to_normal in hash_set(any::<UnminedTxId>(), 1..MAX_CRAWLED_TX),
    ) {
        let (runtime, _init_guard) = zebra_test::init_async();

        // Make transaction_ids_and_responses unique
        let unique_transaction_ids_and_responses: HashSet<UnminedTxId> = transaction_ids_and_responses.iter().map(|(id, _result)| id).copied().collect();
        let transaction_ids_and_responses: Vec<(UnminedTxId, Result<(), MempoolError>)> = unique_transaction_ids_and_responses.iter().map(|unique_id| transaction_ids_and_responses.iter().find(|(id, _result)| id == unique_id).unwrap()).cloned().collect();

        runtime.block_on(async move {
            let (mut peer_set, mut mempool, _sync_status, mut recent_sync_lengths, _chain_tip_sender) =
                setup_crawler();

            time::pause();

            // Mock end of chain sync to enable the mempool crawler.
            SyncStatus::sync_close_to_tip(&mut recent_sync_lengths);

            // Prepare to simulate download errors.
            let download_result_count = transaction_ids_and_responses.len();
            let mut transaction_ids_for_download_errors = HashSet::with_capacity(download_result_count);
            let mut download_result_list = Vec::with_capacity(download_result_count);

            for (transaction_id, result) in transaction_ids_and_responses {
                transaction_ids_for_download_errors.insert(transaction_id);
                download_result_list.push(result);
            }

            // First crawl iteration:
            // 1. Fails with a mempool call error
            // 2. Some downloads fail
            // Rest: no crawled transactions
            crawler_iteration(
                &mut peer_set,
                vec![
                    transaction_ids_for_call_failure.clone(),
                    transaction_ids_for_download_errors.clone(),
                ],
            )
            .await?;

            // First test with an error returned from the Mempool service.
            respond_to_queue_request_with_error(
                &mut mempool,
                transaction_ids_for_call_failure,
                service_call_error,
            ).await?;

            // Then test a failure to download transactions.
            respond_to_queue_request(
                &mut mempool,
                transaction_ids_for_download_errors,
                download_result_list,
            ).await?;

            mempool.expect_no_requests().await?;

            // Wait until next crawl iteration.
            time::sleep(RATE_LIMIT_DELAY).await;

            // Second crawl iteration:
            // The mempool should continue crawling normally.
            crawler_iteration(
                &mut peer_set,
                vec![transaction_ids_for_return_to_normal.clone()],
            )
            .await?;

            let response_list = vec![Ok(()); transaction_ids_for_return_to_normal.len()];

            respond_to_queue_request(
                &mut mempool,
                transaction_ids_for_return_to_normal,
                response_list,
            ).await?;

            mempool.expect_no_requests().await?;

            Ok::<(), TestCaseError>(())
        })?;
    }
}

/// Spawn a crawler instance using mock services.
fn setup_crawler() -> (
    MockPeerSet,
    MockMempool,
    SyncStatus,
    RecentSyncLengths,
    ChainTipSender,
) {
    let peer_set = MockService::build().for_prop_tests();
    let mempool = MockService::build().for_prop_tests();
    let (sync_status, recent_sync_lengths) = SyncStatus::new();

    // the network should be irrelevant here
    let (chain_tip_sender, _latest_chain_tip, chain_tip_change) =
        ChainTipSender::new(None, &Network::Mainnet);

    Crawler::spawn(
        &Config::default(),
        peer_set.clone(),
        mempool.clone(),
        sync_status.clone(),
        chain_tip_change,
    );

    (
        peer_set,
        mempool,
        sync_status,
        recent_sync_lengths,
        chain_tip_sender,
    )
}

/// Intercept a request for mempool transaction IDs and respond with the `transaction_ids` list.
async fn respond_with_transaction_ids(
    peer_set: &mut MockPeerSet,
    transaction_ids: HashSet<UnminedTxId>,
) -> Result<(), TestCaseError> {
    peer_set
        .expect_request(zn::Request::MempoolTransactionIds)
        .await?
        .respond(zn::Response::TransactionIds(
            transaction_ids.into_iter().collect(),
        ));

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
    responses: Vec<HashSet<UnminedTxId>>,
) -> Result<(), TestCaseError> {
    let empty_responses = FANOUT
        .checked_sub(responses.len())
        .expect("Too many responses to be sent in a single crawl iteration");

    for response in responses {
        respond_with_transaction_ids(peer_set, response).await?;
    }

    for _ in 0..empty_responses {
        respond_with_transaction_ids(peer_set, HashSet::new()).await?;
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
    expected_transaction_ids: HashSet<UnminedTxId>,
    response: impl IntoIterator<Item = Result<(), MempoolError>>,
) -> Result<(), TestCaseError> {
    let response: Vec<Result<oneshot::Receiver<Result<(), BoxError>>, BoxError>> = response
        .into_iter()
        .map(|result| {
            result
                .map(|_| {
                    let (rsp_tx, rsp_rx) = oneshot::channel();
                    let _ = rsp_tx.send(Ok(()));
                    rsp_rx
                })
                .map_err(BoxError::from)
        })
        .collect();

    mempool
        .expect_request_that(|req| {
            if let mempool::Request::Queue(req) = req {
                let ids: HashSet<UnminedTxId> = req
                    .iter()
                    .filter_map(|gossip| {
                        if let Gossip::Id(id) = gossip {
                            Some(*id)
                        } else {
                            None
                        }
                    })
                    .collect();
                ids == expected_transaction_ids
            } else {
                false
            }
        })
        .await?
        .respond(mempool::Response::Queued(response));

    Ok(())
}

/// Intercept request for mempool to download and verify transactions, and answer with an error.
///
/// The intercepted request will be verified to check if it has the `expected_transaction_ids`, and
/// it will be answered with `error`, as if the service had an internal failure that prevented it
/// from queuing the transactions for downloading.
async fn respond_to_queue_request_with_error(
    mempool: &mut MockMempool,
    expected_transaction_ids: HashSet<UnminedTxId>,
    error: MempoolError,
) -> Result<(), TestCaseError> {
    mempool
        .expect_request_that(|req| {
            if let mempool::Request::Queue(req) = req {
                let ids: HashSet<UnminedTxId> = req
                    .iter()
                    .filter_map(|gossip| {
                        if let Gossip::Id(id) = gossip {
                            Some(*id)
                        } else {
                            None
                        }
                    })
                    .collect();
                ids == expected_transaction_ids
            } else {
                false
            }
        })
        .await?
        .respond(Err(error));

    Ok(())
}
