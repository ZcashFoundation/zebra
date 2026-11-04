//! Ordinary and RPC rejection must suppress mining work without an early-relay subscriber.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use futures::FutureExt;

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_and_mined_rejections_suppress_mining_replay() {
    let _init_guard = zebra_test::init();
    for initially_mined in [false, true] {
        for contextual_failure in [false, true] {
            tokio::time::timeout(Duration::from_secs(20), async {
                let (network, state, read) = state().await;
                let block = candidate(&network, &read, true).await;
                let hash = block.hash();
                // Mining is enabled, but deliberately never request EarlyRelayCache.
                let zs::ReadResponse::MiningCandidateChanges(mut changes) = read
                    .clone()
                    .oneshot(zs::ReadRequest::MiningCandidateChanges)
                    .await
                    .unwrap()
                else {
                    panic!("state must return a mining subscription")
                };
                let admissions = Arc::new(AtomicUsize::new(0));
                let rejections = Arc::new(AtomicUsize::new(0));
                let observed_state = state.map_response({
                    let admissions = admissions.clone();
                    let rejections = rejections.clone();
                    move |response: zs::Response| {
                        match &response {
                            zs::Response::EarlyRelay(guard) => assert!(
                                guard.is_none(),
                                "without an early-relay subscriber ordinary validation has no guard"
                            ),
                            zs::Response::MiningCandidate(Some(_)) => assert_eq!(
                                admissions.fetch_add(1, Ordering::SeqCst),
                                0,
                                "a rejected replay must not reserve another mining candidate"
                            ),
                            zs::Response::Rejected(remembered) => {
                                assert!(*remembered, "the genuine body must authenticate");
                                rejections.fetch_add(1, Ordering::SeqCst);
                            }
                            _ => {}
                        }
                        response
                    }
                });
                let mut transactions: MockService<tx::BlockRequest, tx::BlockResponse, _, BoxError> =
                    MockService::build().for_unit_tests();

                // Each initial source is replayed through both the ordinary and RPC-equivalent path.
                for (delivery, mined) in [initially_mined, false, true].into_iter().enumerate() {
                    let (committed, mut commits) = tokio::sync::mpsc::channel(1);
                    let (validated, validation) = tokio::sync::watch::channel(None);
                    let request = if mined {
                        Request::CommitMined(
                            block.clone(),
                            zs::MinedBlockNotifications::new(committed, validated),
                        )
                    } else {
                        Request::Commit(block.clone())
                    };
                    let verifier = SemanticBlockVerifier::new(
                        &network,
                        observed_state.clone(),
                        transactions.clone(),
                    );
                    let verification = tokio::spawn(verifier.oneshot(request));
                    let mut pending = Vec::new();
                    for _ in &block.transactions {
                        pending.push(transactions.expect_request_that(|_| true).await);
                    }
                    if delivery == 0 {
                        changes.changed().await;
                        assert!(changes.is_current(hash));
                        assert!(matches!(
                            read.clone().oneshot(zs::ReadRequest::MiningCandidate).await.unwrap(),
                            zs::ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == hash
                        ));
                    } else {
                        assert!(!changes.is_current(hash));
                        assert!(matches!(
                            read.clone().oneshot(zs::ReadRequest::MiningCandidate).await.unwrap(),
                            zs::ReadResponse::MiningCandidate(None)
                        ));
                        assert!(
                            changes.changed().now_or_never().is_none(),
                            "replay must not signal new template work while verification is held"
                        );
                    }
                    assert_eq!(admissions.load(Ordering::SeqCst), 1);
                    assert!(!verification.is_finished(), "transaction verification is held");
                    for request in pending {
                        if contextual_failure {
                            // Let the real state reject the fixture's missing transparent spend.
                            request.respond_with(|request| tx::BlockResponse {
                                tx_id: request.transaction.unmined_id(),
                                miner_fee: None,
                                sigops: 0,
                            });
                        } else {
                            request.respond_error(TransactionError::BadBalance.into());
                        }
                    }
                    let error = verification.await.unwrap().unwrap_err();
                    if contextual_failure {
                        assert!(matches!(
                            &error,
                            VerifyBlockError::Commit(zs::CommitBlockError::ValidateContextError(source))
                                if matches!(**source, zs::ValidateContextError::MissingTransparentOutput { .. })
                        ), "{error:?}");
                    } else {
                        assert!(matches!(
                            &error,
                            VerifyBlockError::Transaction(TransactionError::BadBalance)
                        ), "{error:?}");
                        assert_eq!(error.misbehavior_score(), 100);
                    }
                    assert_eq!(rejections.load(Ordering::SeqCst), delivery + 1);
                    assert!(validation.borrow().is_none());
                    assert!(commits.try_recv().is_err());
                    if delivery == 0 {
                        // Drain the first candidate's withdrawal before observing replay signals.
                        changes.changed().await;
                        changes.mark_seen();
                    }
                    assert!(!changes.is_current(hash));
                    assert!(matches!(
                        read.clone().oneshot(zs::ReadRequest::MiningCandidate).await.unwrap(),
                        zs::ReadResponse::MiningCandidate(None)
                    ));
                    assert!(
                        changes.changed().now_or_never().is_none(),
                        "rejection and replay must not publish another mining candidate"
                    );
                }
            })
            .await
            .expect("ordinary rejection and both replay paths must finish before candidate expiry");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_and_mined_forged_bodies_do_not_reject_genuine_mining_work() {
    let _init_guard = zebra_test::init();
    for mined in [false, true] {
        tokio::time::timeout(Duration::from_secs(20), async {
            let (network, state, read) = state().await;
            let block = candidate(&network, &read, true).await;
            let forged = forge_authorizing_data(&block);
            assert_eq!(forged.hash(), block.hash());
            let hashes: Vec<_> = forged.transactions.iter().map(|tx| tx.hash()).collect();
            check::merkle_root_validity(&network, &forged, &hashes).unwrap();
            let rejections = Arc::new(AtomicUsize::new(0));
            let observed_state = state.clone().map_response({
                let rejections = rejections.clone();
                move |response: zs::Response| {
                    match &response {
                        zs::Response::EarlyRelay(guard) => assert!(guard.is_none()),
                        zs::Response::Rejected(remembered) => {
                            assert!(
                                !remembered,
                                "an unauthenticated body must not poison its hash"
                            );
                            rejections.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                    response
                }
            });
            let transactions = tower::service_fn(|_| async {
                Err::<tx::BlockResponse, BoxError>(TransactionError::BadBalance.into())
            });
            let (committed, _commits) = tokio::sync::mpsc::channel(1);
            let (validated, _validation) = tokio::sync::watch::channel(None);
            let request = if mined {
                Request::CommitMined(
                    forged,
                    zs::MinedBlockNotifications::new(committed, validated),
                )
            } else {
                Request::Commit(forged)
            };
            let error = SemanticBlockVerifier::new(&network, observed_state.clone(), transactions)
                .oneshot(request)
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                VerifyBlockError::Transaction(TransactionError::BadBalance)
            ));
            assert_eq!(rejections.load(Ordering::SeqCst), 1);
            let zs::Response::EarlyRelay(Some(guard)) = state
                .oneshot(zs::Request::EarlyRelay {
                    block: block.clone(),
                    require_authentication: true,
                })
                .await
                .unwrap()
            else {
                panic!("the genuine body must authenticate after forged ordinary or RPC delivery")
            };
            assert!(!guard.is_rejected());
            drop(guard);

            // Enable mining only now: forged-body computation cannot temporarily occupy its slot.
            // Still never subscribe to early relay, so genuine verification has no relay guard.
            let zs::ReadResponse::MiningCandidateChanges(mut changes) = read
                .clone()
                .oneshot(zs::ReadRequest::MiningCandidateChanges)
                .await
                .unwrap()
            else {
                panic!("state must return a mining subscription")
            };
            let mut transactions: MockService<tx::BlockRequest, tx::BlockResponse, _, BoxError> =
                MockService::build().for_unit_tests();
            let verifier =
                SemanticBlockVerifier::new(&network, observed_state, transactions.clone());
            let (committed, _commits) = tokio::sync::mpsc::channel(1);
            let (validated, _validation) = tokio::sync::watch::channel(None);
            let request = if mined {
                Request::CommitMined(
                    block.clone(),
                    zs::MinedBlockNotifications::new(committed, validated),
                )
            } else {
                Request::Commit(block.clone())
            };
            let verification = tokio::spawn(verifier.oneshot(request));
            let pending = transactions.expect_request_that(|_| true).await;
            changes.changed().await;
            assert!(changes.is_current(block.hash()));
            assert!(matches!(
                read.oneshot(zs::ReadRequest::MiningCandidate).await.unwrap(),
                zs::ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == block.hash()
            ));
            assert!(!verification.is_finished());
            verification.abort();
            assert!(verification.await.unwrap_err().is_cancelled());
            drop(pending);
            changes.changed().await;
            assert!(!changes.is_current(block.hash()));
            assert_eq!(rejections.load(Ordering::SeqCst), 1);
        })
        .await
        .expect("forged rejection must leave genuine mining work admissible");
    }
}
