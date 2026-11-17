//! Fixed test vectors for the mempool transaction downloader.

use std::time::Duration;

use futures::StreamExt as _;
use tower::{service_fn, util::BoxCloneService};

use zebra_chain::parameters::Network;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::*;

#[test]
fn download_verify_error_metric_labels_ignore_details() {
    use zebra_consensus::error::TransactionError;

    use TransactionDownloadVerifyError::*;

    let transaction = Network::Mainnet
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .expect("a transaction fixture exists")
        .transaction;

    for detail in ["first error detail", "second error detail"] {
        assert_eq!(
            StateError(BoxError::from(detail).into()).metric_label(),
            "state_error",
        );
        assert_eq!(
            DownloadFailed(BoxError::from(detail).into()).metric_label(),
            "download_failed",
        );
    }

    for error in [
        TransactionError::WrongVersion,
        TransactionError::WrongConsensusBranchId,
    ] {
        let error = Invalid {
            error,
            advertiser_addr: None,
            transaction_upgrade: None,
            verification_height: Height(0),
            transaction: transaction.clone(),
            queue_source: None,
        };
        assert_eq!(error.metric_label(), "invalid");
    }

    assert_eq!(
        NonStandard(NonStandardTransactionError::IsDust).metric_label(),
        "non_standard",
    );
}

/// A directly pushed full transaction must consume the sending peer's
/// per-peer admission budget, exactly like an advertised transaction ID, so
/// a single peer cannot bypass [`MAX_INBOUND_CONCURRENCY_PER_PEER`] by
/// sending `tx` messages instead of `inv` advertisements.
///
/// Regression test for `GHSA-m9xx-8rcj-vmgp`.
#[tokio::test]
async fn per_peer_cap_applies_to_pushed_transactions() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let source: SocketAddr = "192.168.0.1:8233".parse().expect("valid socket address");

    // Distinct full transactions, as if pushed directly by a single peer.
    let pushed: Vec<Gossip> = network
        .unmined_transactions_in_blocks(1..=10)
        .take(MAX_INBOUND_CONCURRENCY_PER_PEER + 1)
        .map(|tx| Gossip::Tx(tx.transaction))
        .collect();
    assert_eq!(
        pushed.len(),
        MAX_INBOUND_CONCURRENCY_PER_PEER + 1,
        "test needs enough distinct transactions to exceed the per-peer cap",
    );

    // The cap is enforced before any download/verify work, so the services
    // are never actually driven in this test.
    let peer_set: MockService<zn::Request, zn::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let verifier: MockService<tx::MempoolRequest, tx::MempoolResponse, PanicAssertion> =
        MockService::build().for_unit_tests();
    let state: MockService<zs::Request, zs::Response, PanicAssertion> =
        MockService::build().for_unit_tests();

    let mut downloads = Downloads::new(peer_set, verifier, state, network.fee_reduction_height());

    // The first `MAX_INBOUND_CONCURRENCY_PER_PEER` pushes from this peer are admitted.
    for gossip in pushed.iter().take(MAX_INBOUND_CONCURRENCY_PER_PEER) {
        downloads
            .download_if_needed_and_verify(gossip.clone(), Some(source), None)
            .expect("pushes below the per-peer cap are admitted");
    }

    let (duplicate_tx, duplicate_rx) = oneshot::channel();
    assert!(matches!(
        downloads.download_if_needed_and_verify(
            pushed[0].clone(),
            Some(source),
            Some(duplicate_tx),
        ),
        Err(MempoolError::AlreadyQueued)
    ));
    assert_eq!(
        duplicate_rx.await.unwrap().unwrap_err().to_string(),
        MempoolError::AlreadyQueued.to_string(),
    );

    // The next push from the same peer exceeds the per-peer cap and is
    // rejected, even though the global queue still has spare capacity.
    let (rsp_tx, rsp_rx) = oneshot::channel();
    let result = downloads.download_if_needed_and_verify(
        pushed[MAX_INBOUND_CONCURRENCY_PER_PEER].clone(),
        Some(source),
        Some(rsp_tx),
    );
    assert!(
        matches!(result, Err(MempoolError::FullQueue)),
        "a push exceeding the per-peer cap must be rejected with `FullQueue`, got {result:?}",
    );
    assert_eq!(
        rsp_rx.await.unwrap().unwrap_err().to_string(),
        MempoolError::FullQueue.to_string(),
    );

    // Don't leave spawned download tasks behind when the runtime shuts down.
    downloads.cancel_all();
}

/// A directly pushed transaction from a peer must keep that peer's address on
/// the `Invalid` verification error, so the mempool can score the peer's
/// misbehavior. Regression test for `GHSA-g7c4-2w6c-cr3r`.
#[tokio::test]
async fn pushed_transaction_attributes_invalid_error_to_peer() {
    use zebra_consensus::error::TransactionError;

    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let peer_addr: SocketAddr = "203.0.113.7:8233".parse().unwrap();
    let transaction = network
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .expect("at least one test transaction")
        .transaction;
    let transaction_upgrade = transaction.transaction.network_upgrade();

    type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

    let mut downloads = Downloads::new(
        BoxCloneService::new(service_fn(|_request| async move {
            panic!("pushed transactions must not be downloaded");
        })),
        BoxCloneService::new(service_fn(|_request| async move {
            Err(Box::new(TransactionError::WrongVersion) as BoxError)
        })),
        BoxCloneService::new(service_fn(|request| async move {
            match request {
                zs::Request::KnownTransaction(_) => Ok(zs::Response::KnownTransaction(false)),
                zs::Request::Tip => Ok(zs::Response::Tip(None)),
                request => Err(format!("unexpected state request: {request:?}").into()),
            }
        })),
        network.fee_reduction_height(),
    );

    downloads
        .download_if_needed_and_verify(Gossip::Tx(transaction), Some(peer_addr), None)
        .expect("download is queued");

    let result = tokio::time::timeout(Duration::from_secs(1), downloads.next())
        .await
        .expect("pushed transaction should complete")
        .expect("download stream should yield an item")
        .expect("pushed transaction should not time out");

    let error = result
        .expect_err("invalid pushed transaction should fail verification")
        .1;
    assert!(
        matches!(
            error,
            TransactionDownloadVerifyError::Invalid {
                advertiser_addr: Some(addr),
                transaction_upgrade: actual_upgrade,
                verification_height: Height(0),
                ..
            } if addr == PeerSocketAddr::from(peer_addr)
                && actual_upgrade == transaction_upgrade
        ),
        "expected the pushed transaction failure to carry its peer, branch, and height, got {error:?}"
    );
}

/// A known transaction is rejected without downloading its unavailable body.
#[tokio::test]
async fn mined_transaction_needs_no_body() {
    let _init_guard = zebra_test::init();
    let transaction = Network::Mainnet
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .expect("mainnet vectors contain a transaction")
        .transaction;
    let hash = transaction.id.mined_id();
    let mut downloads = Downloads::new(
        service_fn(|_| async { panic!("a mined transaction must not be downloaded") }),
        service_fn(|_| async { panic!("a mined transaction must not be verified") }),
        service_fn(move |request| async move {
            assert_eq!(request, zs::Request::KnownTransaction(hash));
            Ok::<_, BoxError>(zs::Response::KnownTransaction(true))
        }),
        Network::Mainnet.fee_reduction_height(),
    );
    downloads
        .download_if_needed_and_verify(Gossip::Id(transaction.id), None, None)
        .expect("the request can be queued");
    let result = tokio::time::timeout(Duration::from_secs(1), downloads.next())
        .await
        .expect("membership lookup must finish")
        .expect("the download returns its result")
        .expect("the task did not time out");
    let error = result.expect_err("a mined transaction is reported as already in the state");
    assert!(matches!(error.1, TransactionDownloadVerifyError::InState));
}

/// Definite failures must reach RPC callers without a mempool poll, and survive a pool reset.
#[tokio::test(start_paused = true)]
async fn verification_errors_reach_rpc_before_mempool_poll() {
    use std::sync::Arc;
    use tower::{buffer::Buffer, ServiceExt as _};
    use zebra_chain::transaction::{LockTime, Transaction, UnminedTx};
    use zebra_consensus::error::TransactionError;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let activation = network.fee_reduction_height();

    for fee_failure in [false, true] {
        let transaction = UnminedTx::from(Arc::new(Transaction::test_v5(
            NetworkUpgrade::current(&network, activation),
            vec![],
            vec![],
            LockTime::unlocked(),
            Height::MIN,
        )));
        let state = BoxCloneService::new(service_fn(move |request| async move {
            match request {
                zs::Request::KnownTransaction(_) => Ok(zs::Response::KnownTransaction(false)),
                zs::Request::Tip => Ok(zs::Response::Tip(Some((
                    activation.previous().unwrap(),
                    zebra_chain::block::Hash([1; 32]),
                )))),
                request => {
                    panic!("invalid transactions need no further state queries: {request:?}")
                }
            }
        }));
        let verifier = if fee_failure {
            let network = network.clone();
            BoxCloneService::new(service_fn(move |request: tx::MempoolRequest| {
                let network = network.clone();
                async move {
                    VerifiedUnminedTx::new(
                        request.transaction,
                        0i64.try_into().unwrap(),
                        0,
                        0,
                        Arc::new(vec![]),
                        &network,
                        request.height,
                    )
                    .map(|transaction| tx::MempoolResponse {
                        transaction,
                        spent_mempool_outpoints: vec![],
                    })
                    .map_err(|error| BoxError::from(TransactionError::Zip317(error)))
                }
            }))
        } else {
            let mempool_setup_rx = oneshot::channel::<
                BoxCloneService<
                    zebra_node_services::mempool::Request,
                    zebra_node_services::mempool::Response,
                    BoxError,
                >,
            >()
            .1;
            BoxCloneService::new(Buffer::new(
                tx::MempoolTxVerifier::new(&network, state.clone(), mempool_setup_rx)
                    .map_err(BoxError::from),
                1,
            ))
        };
        let mut downloads = Downloads::new(
            BoxCloneService::new(service_fn(|_| async {
                panic!("pushed transactions are not downloaded");
            })),
            verifier,
            state,
            activation,
        );
        let (rsp_tx, rsp_rx) = oneshot::channel();
        downloads
            .download_if_needed_and_verify(transaction.into(), None, Some(rsp_tx))
            .expect("the invalid transaction is admitted for verification");

        // Wait for the independent task, not Downloads::poll_next or Mempool::poll_ready.
        let completed = tokio::time::timeout(Duration::from_secs(1), downloads.pending.next())
            .await
            .expect("definite verification failures do not wait for a queue-checker tick")
            .expect("the download task exists")
            .expect("the download task does not panic")
            .expect("the download task does not time out");
        assert!(
            completed.is_err(),
            "the real validation rule rejects the fixture"
        );

        // A mempool reset drops completed results before polling them.
        drop(completed);
        downloads.cancel_all();
        let error = rsp_rx
            .await
            .expect("reset must not replace the original rejection with a closed channel")
            .expect_err("the submission must fail");
        let expected = if fee_failure {
            TransactionError::Zip317(transaction::zip317::Error::UnpaidActions)
        } else {
            TransactionError::NoInputs
        };
        assert!(
            error.to_string().contains(&expected.to_string()),
            "the caller must receive the original validation failure: {error}"
        );
    }
}
