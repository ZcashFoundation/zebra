//! Fixed test vectors for the mempool transaction downloader.

use std::{sync::Arc, time::Duration};

use tokio::sync::Notify;

use futures::StreamExt as _;
use tower::{service_fn, util::BoxCloneService};

use zebra_chain::parameters::Network;
use zebra_test::mock_service::{MockService, PanicAssertion};

use super::*;

#[test]
fn download_verify_error_metric_labels_ignore_details() {
    use zebra_consensus::error::TransactionError;

    use TransactionDownloadVerifyError::*;

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
        };
        assert_eq!(error.metric_label(), "invalid");
    }
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
    let state: MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion> =
        MockService::build().for_unit_tests();

    let mut downloads = Downloads::new(peer_set, verifier, state, Arc::new(Notify::new()));

    // The first `MAX_INBOUND_CONCURRENCY_PER_PEER` pushes from this peer are admitted.
    for gossip in pushed.iter().take(MAX_INBOUND_CONCURRENCY_PER_PEER) {
        downloads
            .download_if_needed_and_verify(gossip.clone(), Some(source), None)
            .expect("pushes below the per-peer cap are admitted");
    }

    // The next push from the same peer exceeds the per-peer cap and is
    // rejected, even though the global queue still has spare capacity.
    let result = downloads.download_if_needed_and_verify(
        pushed[MAX_INBOUND_CONCURRENCY_PER_PEER].clone(),
        Some(source),
        None,
    );
    assert!(
        matches!(result, Err(MempoolError::FullQueue)),
        "a push exceeding the per-peer cap must be rejected with `FullQueue`, got {result:?}",
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
                zs::ReadRequest::Transaction(_) => Ok(zs::ReadResponse::Transaction(None)),
                zs::ReadRequest::Tip => Ok(zs::ReadResponse::Tip(None)),
                request => Err(format!("unexpected state request: {request:?}").into()),
            }
        })),
        Arc::new(Notify::new()),
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

/// Same-effects variants from one host share a slot, released on every terminal path.
#[tokio::test(start_paused = true)]
async fn same_effects_variants_share_pending_slot_per_peer() {
    let _init_guard = zebra_test::init();
    let source: SocketAddr = "192.0.2.1:8233".parse().expect("valid socket address");
    let transaction = Network::Mainnet
        .unmined_transactions_in_blocks(..)
        .rev()
        .find(|tx| matches!(tx.transaction.id, UnminedTxId::Witnessed(_)))
        .expect("block test vectors contain a witnessed transaction");
    let first_id = transaction.transaction.id;
    let mut variant_id = first_id;
    variant_id
        .auth_digest_mut()
        .expect("witnessed transactions have an authorization digest")
        .0[0] ^= 1;

    let mut peer_set: MockService<zn::Request, zn::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let mut verifier: MockService<tx::MempoolRequest, tx::MempoolResponse, PanicAssertion> =
        MockService::build().for_unit_tests();
    let state = service_fn(|request| async move {
        match request {
            zs::ReadRequest::Transaction(_) => Ok(zs::ReadResponse::Transaction(None)),
            zs::ReadRequest::Tip => Ok(zs::ReadResponse::Tip(None)),
            request => Err(BoxError::from(format!(
                "unexpected state request: {request:?}"
            ))),
        }
    });
    let mut downloads = Downloads::new(
        peer_set.clone(),
        verifier.clone(),
        state,
        Arc::new(Notify::new()),
    );

    downloads
        .download_if_needed_and_verify(Gossip::Id(first_id), Some(source), None)
        .expect("first variant is admitted");
    let download = peer_set
        .expect_request(zn::Request::TransactionsById([first_id].into()))
        .await;

    assert!(matches!(
        downloads.download_if_needed_and_verify(Gossip::Id(variant_id), Some(source), None),
        Err(MempoolError::AlreadyQueued)
    ));
    assert_eq!(downloads.in_flight(), 1);

    download.respond(zn::Response::Transactions(vec![
        zn::InventoryResponse::Available((transaction.transaction.clone(), None)),
    ]));
    let verification = verifier
        .expect_request_that(|request| request.transaction.id == first_id)
        .await;
    assert!(matches!(
        downloads.download_if_needed_and_verify(Gossip::Id(variant_id), Some(source), None),
        Err(MempoolError::AlreadyQueued)
    ));
    verification.respond(tx::MempoolResponse::from(transaction));

    let completed = tokio::time::timeout(Duration::from_secs(1), downloads.next())
        .await
        .expect("first variant finishes")
        .expect("a task was queued")
        .expect("verification does not time out")
        .expect("first variant verifies successfully");
    assert_eq!(completed.0.transaction.id, first_id);
    assert_eq!(downloads.in_flight(), 0);

    for outcome in ["failure", "cancellation", "timeout", "cancel_all"] {
        downloads
            .download_if_needed_and_verify(Gossip::Id(variant_id), Some(source), None)
            .expect("completion releases the mined-ID reservation");
        let download = peer_set
            .expect_request(zn::Request::TransactionsById([variant_id].into()))
            .await;
        assert!(matches!(
            downloads.download_if_needed_and_verify(Gossip::Id(first_id), Some(source), None),
            Err(MempoolError::AlreadyQueued)
        ));

        match outcome {
            "failure" => download.respond_error("download failed".into()),
            "cancellation" => {
                downloads.cancel(&[variant_id.mined_id()].into());
                assert!(matches!(
                    downloads.download_if_needed_and_verify(
                        Gossip::Id(first_id),
                        Some(source),
                        None
                    ),
                    Err(MempoolError::AlreadyQueued)
                ));
            }
            "timeout" => {
                tokio::time::advance(RATE_LIMIT_DELAY).await;
            }
            "cancel_all" => {
                downloads.cancel_all();
            }
            _ => unreachable!("all test outcomes are handled"),
        }

        let completed = tokio::time::timeout(Duration::from_secs(1), downloads.next())
            .await
            .expect("terminal task is ready");
        match outcome {
            // Cancelled tasks still queue a result, which releases their admission slot.
            "failure" | "cancellation" | "cancel_all" => {
                let error = completed
                    .expect("a task was queued")
                    .expect("task does not time out")
                    .expect_err("task does not verify");
                assert_eq!(error.0, variant_id);
                assert!(matches!(
                    (outcome, error.1),
                    ("failure", TransactionDownloadVerifyError::DownloadFailed(_))
                        | (
                            "cancellation" | "cancel_all",
                            TransactionDownloadVerifyError::Cancelled
                        )
                ));
            }
            "timeout" => assert_eq!(
                completed
                    .expect("a task was queued")
                    .expect_err("task times out")
                    .0,
                variant_id
            ),
            _ => unreachable!("all test outcomes are handled"),
        }
        assert_eq!(downloads.in_flight(), 0);
    }

    downloads
        .download_if_needed_and_verify(Gossip::Id(first_id), Some(source), None)
        .expect("reset releases the mined-ID reservation");
    downloads.cancel_all();
}

/// A fabricated authorization digest must not suppress other peers' or local candidates.
#[tokio::test(start_paused = true)]
async fn unverified_witnessed_id_does_not_reserve_other_sources() {
    let _init_guard = zebra_test::init();
    let attacker: SocketAddr = "192.0.2.1:8233".parse().expect("valid socket address");
    let honest_peer: SocketAddr = "192.0.2.2:8233".parse().expect("valid socket address");
    let transaction = Network::Mainnet
        .unmined_transactions_in_blocks(..)
        .rev()
        .find(|tx| matches!(tx.transaction.id, UnminedTxId::Witnessed(_)))
        .expect("block test vectors contain a witnessed transaction");
    let txid = transaction.transaction.id;
    let mut fabricated_id = txid;
    fabricated_id
        .auth_digest_mut()
        .expect("witnessed transactions have an authorization digest")
        .0[0] ^= 1;

    let mut peer_set: MockService<zn::Request, zn::Response, PanicAssertion> =
        MockService::build().for_unit_tests();
    let mut verifier: MockService<tx::MempoolRequest, tx::MempoolResponse, PanicAssertion> =
        MockService::build().for_unit_tests();
    let state = service_fn(|request| async move {
        match request {
            zs::ReadRequest::Transaction(_) => Ok(zs::ReadResponse::Transaction(None)),
            zs::ReadRequest::Tip => Ok(zs::ReadResponse::Tip(None)),
            request => Err(BoxError::from(format!(
                "unexpected state request: {request:?}"
            ))),
        }
    });
    let mut downloads = Downloads::new(
        peer_set.clone(),
        verifier.clone(),
        state,
        Arc::new(Notify::new()),
    );

    for source in [Some(honest_peer), None] {
        for gossip in [
            Gossip::Id(txid),
            Gossip::Tx(transaction.transaction.clone()),
        ] {
            downloads
                .download_if_needed_and_verify(Gossip::Id(fabricated_id), Some(attacker), None)
                .expect("fabricated ID starts a download");
            let attack = peer_set
                .expect_request(zn::Request::TransactionsById([fabricated_id].into()))
                .await;

            let needs_download = matches!(gossip, Gossip::Id(_));
            downloads
                .download_if_needed_and_verify(gossip, source, None)
                .expect("another source's candidate is not suppressed by a fabricated ID");
            assert_eq!(downloads.in_flight(), 2);
            if needs_download {
                peer_set
                    .expect_request(zn::Request::TransactionsById([txid].into()))
                    .await
                    .respond(zn::Response::Transactions(vec![
                        zn::InventoryResponse::Available((transaction.transaction.clone(), None)),
                    ]));
            }
            verifier
                .expect_request_that(|request| request.transaction.id == txid)
                .await
                .respond(tx::MempoolResponse::from(transaction.clone()));
            let verified = tokio::time::timeout(Duration::from_secs(1), downloads.next())
                .await
                .expect("honest candidate completes while the fabricated ID remains pending")
                .expect("honest task was queued")
                .expect("verification does not time out")
                .expect("honest candidate verifies");
            assert_eq!(verified.0.transaction.id, txid);

            attack.respond_error("notfound".into());
            let failed = tokio::time::timeout(Duration::from_secs(1), downloads.next())
                .await
                .expect("failed download completes")
                .expect("fabricated ID was queued")
                .expect("download does not time out")
                .expect_err("fabricated ID has no transaction");
            assert_eq!(failed.0, fabricated_id);
            assert!(matches!(
                failed.1,
                TransactionDownloadVerifyError::DownloadFailed(_)
            ));
            assert_eq!(downloads.in_flight(), 0);
        }
    }
}

/// A coalesced wake must drain a batch larger than Tokio's cooperative budget.
#[tokio::test(start_paused = true)]
async fn completed_batch_keeps_queue_checker_awake() {
    let notified = Arc::new(Notify::new());
    let mut downloads = Downloads::new(
        service_fn(|_| async { Ok(zn::Response::Transactions(Vec::new())) }),
        service_fn(|_| std::future::pending::<Result<tx::MempoolResponse, BoxError>>()),
        service_fn(|request| async move {
            Ok(match request {
                zs::ReadRequest::Transaction(_) => zs::ReadResponse::Transaction(None),
                zs::ReadRequest::Tip => zs::ReadResponse::Tip(None),
                _ => unreachable!("downloads only query transactions and the tip"),
            })
        }),
        notified.clone(),
    );
    for index in 0..MAX_INBOUND_CONCURRENCY {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
        downloads
            .download_if_needed_and_verify(
                Gossip::Id(UnminedTxId::Legacy(transaction::Hash(hash))),
                None,
                None,
            )
            .unwrap();
    }
    // Paused time advances only after every runnable download has finished.
    tokio::time::sleep(Duration::from_millis(1)).await;
    assert!(downloads.pending.iter().all(JoinHandle::is_finished));

    let before_backstop = super::super::queue_checker::RATE_LIMIT_DELAY / 2;
    tokio::time::timeout(before_backstop, async {
        let mut completed = 0;
        while completed < MAX_INBOUND_CONCURRENCY {
            notified.notified().await;
            // Like the mempool, drain ready results but return readiness on Pending.
            futures::future::poll_fn(|cx| {
                while let Poll::Ready(Some(_)) = Pin::new(&mut downloads).poll_next(cx) {
                    completed += 1;
                }
                Poll::Ready(())
            })
            .await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued results must not wait for the five-second backstop");
}

/// Consuming a result frees admission even if its task has not exited.
#[tokio::test(start_paused = true)]
async fn full_queue_can_retry_before_completed_task_exits() {
    let mut downloads = Downloads::new(
        service_fn(|_| std::future::pending::<Result<zn::Response, BoxError>>()),
        service_fn(|_| std::future::pending::<Result<tx::MempoolResponse, BoxError>>()),
        service_fn(|_| std::future::pending::<Result<zs::ReadResponse, BoxError>>()),
        Arc::new(Notify::new()),
    );
    let tx = Network::Mainnet
        .unmined_transactions_in_blocks(1..=1)
        .next()
        .expect("mainnet vectors contain a transaction");
    downloads
        .download_if_needed_and_verify(tx.transaction.clone().into(), None, None)
        .unwrap();
    for index in 1..MAX_INBOUND_CONCURRENCY {
        let mut hash = [0; 32];
        hash[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
        downloads
            .download_if_needed_and_verify(
                Gossip::Id(UnminedTxId::Legacy(transaction::Hash(hash))),
                None,
                None,
            )
            .unwrap();
    }

    // Reproduce the interval after sending a result but before its JoinHandle becomes ready.
    let (response, mut receiver) = oneshot::channel();
    downloads
        .results_sender
        .send(Ok(Ok((tx, Vec::new(), Some(Height(0)), Some(response)))))
        .unwrap();
    let (tx, _, _, response) = tokio::time::timeout(Duration::from_secs(1), downloads.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(downloads.pending.len(), MAX_INBOUND_CONCURRENCY);
    assert!(downloads.pending.iter().all(|task| !task.is_finished()));

    // The mempool retries this transaction when its verified tip height is stale.
    downloads
        .download_if_needed_and_verify(tx.transaction.into(), None, response)
        .expect("consuming a result must free its slot before a stale-tip retry");
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}
