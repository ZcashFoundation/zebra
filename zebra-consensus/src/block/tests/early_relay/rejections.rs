//! Authenticated rejection suppression must avoid both repeated proof work and cache poisoning.

use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use zebra_chain::transaction::arbitrary::{
    insert_fake_orchard_shielded_data, with_garbage_orchard_authorization,
};

use super::*;

#[test]
fn early_relay_rejected_proof_skips_redispatch_but_still_authenticates() {
    let _init_guard = zebra_test::init();
    // Own the state's background tasks so RocksDB closes before this test returns.
    let state_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (network, state, read) = state_runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), state())
            .await
            .expect("state initialization must finish")
    });
    // Global batch workers must outlive this test.
    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        tokio::time::timeout(Duration::from_secs(60), async {
        let mut block = (*candidate(&network, &read, false).await).clone();
        let shielded = insert_fake_orchard_shielded_data(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            Vec::new(),
            Vec::new(),
            LockTime::unlocked(),
            block.coinbase_height().unwrap(),
        ));
        block.transactions.push(Arc::new(shielded));
        Arc::make_mut(&mut block.header).merkle_root =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        let zs::ReadResponse::ChainInfo(info) = read
            .clone()
            .oneshot(zs::ReadRequest::ChainInfo)
            .await
            .unwrap()
        else {
            panic!("chain info response")
        };
        let block = seal_candidate(&network, block, info.chain_history_root);
        let cache = cache(&read).await;
        let dispatches = Arc::new(AtomicUsize::new(0));
        let count = dispatches.clone();
        let transactions = Buffer::new(tx::BlockTxVerifier::new(&network, state.clone()), 10);
        let transactions = tower::service_fn(move |request| {
            count.fetch_add(1, Ordering::SeqCst);
            transactions.clone().oneshot(request)
        });

        let error = SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
            .oneshot(Request::CommitEarlyRelay(block.clone()))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                VerifyBlockError::EarlyRelay(source)
                    if matches!(**source,
                        VerifyBlockError::Transaction(TransactionError::Halo2VerificationFailed))
            ),
            "expected real shielded proof rejection, got {error:?}"
        );
        assert_eq!(error.misbehavior_score(), 0);
        assert_eq!(dispatches.load(Ordering::SeqCst), block.transactions.len());
        assert!(cache.get(&block.hash()).is_none());
        assert!(matches!(
            state
                .clone()
                .oneshot(zs::Request::KnownBlock(block.hash()))
                .await
                .unwrap(),
            zs::Response::KnownBlock(None)
        ));

        let zs::Response::EarlyRelay(Some(guard)) = state
            .clone()
            .oneshot(zs::Request::EarlyRelay {
                block: block.clone(),
                require_authentication: true,
            })
            .await
            .unwrap()
        else {
            panic!("the real state must authenticate the rejected body")
        };
        assert!(guard.is_rejected());
        drop(guard);
        let error = SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
            .oneshot(Request::CommitEarlyRelay(block.clone()))
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyBlockError::EarlyRelayRejected));
        let error = crate::router::RouterError::from(error);
        assert_eq!(error.misbehavior_score(), 0);
        assert!(
            !error.is_resource_pressure(),
            "rejection must not trigger a retry loop"
        );
        assert_eq!(dispatches.load(Ordering::SeqCst), block.transactions.len());

        let mut forged = (*block).clone();
        forged.transactions[1] = Arc::new(with_garbage_orchard_authorization(
            (*forged.transactions[1]).clone(),
        ));
        assert_eq!(forged.hash(), block.hash());
        assert_eq!(forged.transactions[1].hash(), block.transactions[1].hash());
        assert_ne!(forged.auth_data_root(), block.auth_data_root());
        let error = SemanticBlockVerifier::new(&network, state, transactions)
            .oneshot(Request::CommitEarlyRelay(Arc::new(forged)))
            .await
            .unwrap_err();
        assert!(error.is_auth_commitment_mismatch());
        assert_eq!(error.misbehavior_score(), 100);
        assert_eq!(dispatches.load(Ordering::SeqCst), block.transactions.len());
        assert!(cache.get(&block.hash()).is_none());
    })
    .await
    .expect("rejection suppression and forgery authentication must finish");
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn block_transient_failures_retry_without_recording_rejection() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let block = candidate(&network, &read, true).await;
        let state = tower::service_fn(move |request| {
            assert!(
                !matches!(&request, zs::Request::RejectBlock(_)),
                "transient failures must not notify rejection"
            );
            state.clone().oneshot(request)
        });
        for failure in 0..3 {
            let dispatches = Arc::new(AtomicUsize::new(0));
            let count = dispatches.clone();
            let transactions = tower::service_fn(move |_: tx::BlockRequest| {
                count.fetch_add(1, Ordering::SeqCst);
                async move {
                    match failure {
                        0 => {
                            tower::timeout::Timeout::new(
                                tower::service_fn(|()| {
                                    std::future::pending::<
                                        Result<tx::BlockResponse, std::convert::Infallible>,
                                    >()
                                }),
                                Duration::from_millis(1),
                            )
                            .oneshot(())
                            .await
                        }
                        1 => Err(std::io::Error::other("transaction worker unavailable").into()),
                        _ => Err(TransactionError::TransparentInputNotFound.into()),
                    }
                }
            });
            for delivery in 1..=6 {
                let (committed, _commits) = tokio::sync::mpsc::channel(1);
                let (validated, _validation) = tokio::sync::watch::channel(None);
                let request = match (delivery - 1) % 3 {
                    0 => Request::CommitEarlyRelay(block.clone()),
                    1 => Request::Commit(block.clone()),
                    _ => Request::CommitMined(
                        block.clone(),
                        zs::MinedBlockNotifications::new(committed, validated),
                    ),
                };
                let error =
                    SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
                        .oneshot(request)
                        .await
                        .unwrap_err();
                assert!(matches!(
                    error,
                    VerifyBlockError::Transaction(TransactionError::InternalDowncastError(_))
                        | VerifyBlockError::Transaction(TransactionError::TransparentInputNotFound)
                ));
                assert_eq!(
                    dispatches.load(Ordering::SeqCst),
                    delivery * block.transactions.len()
                );
                let zs::Response::EarlyRelay(Some(guard)) = state
                    .clone()
                    .oneshot(zs::Request::EarlyRelay {
                        block: block.clone(),
                        require_authentication: true,
                    })
                    .await
                    .unwrap()
                else {
                    panic!("transient failures must not remove the authentication proof")
                };
                assert!(!guard.is_rejected());
            }
        }
    })
    .await
    .expect("transient failures must permit fresh transaction verification");
}

/// Content-derived totals stay invalid, while local state pressure must remain retryable.
#[test]
fn early_relay_rejection_classifies_block_totals_not_state_pressure() {
    let height = Height(1);
    let hash = block::Hash([0; 32]);
    for error in [
        BlockError::TooManyTransparentSignatureOperations {
            height,
            hash,
            sigops: MAX_BLOCK_SIGOPS + 1,
        },
        BlockError::SummingMinerFees {
            height,
            hash,
            source: Amount::<NonNegative>::try_from(i64::MAX).unwrap_err(),
        },
        BlockError::TooManyShieldedActions {
            height,
            hash,
            pool: "Orchard actions",
            count: 2,
            limit: 1,
        },
        BlockError::from(zebra_chain::parameters::subsidy::SubsidyError::FundingStreamNotFound),
        BlockError::from(zebra_chain::parameters::subsidy::SubsidyError::FoundersRewardNotFound),
        BlockError::from(
            zebra_chain::parameters::subsidy::SubsidyError::OneTimeLockboxDisbursementNotFound,
        ),
    ] {
        assert!(VerifyBlockError::from(error).is_deterministic_rejection());
    }
    assert!(
        !VerifyBlockError::Commit(zs::CommitBlockError::OrphanQueueFull)
            .is_deterministic_rejection()
    );
    assert!(!VerifyBlockError::StateService {
        source: "state unavailable".into(),
        hash
    }
    .is_deterministic_rejection());
}

/// Untyped state errors stay retryable on both cached and ordinary commit paths.
#[tokio::test(flavor = "multi_thread")]
async fn untyped_commit_errors_do_not_record_rejections() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        for cached in [false, true] {
            let (mut verifier, _state, solved, _calls, _read) = verified_proposal().await;
            if !cached {
                verifier.proposals.send_modify(VecDeque::clear);
            }
            let state = tower::service_fn(move |request| async move {
                match request {
                    zs::Request::KnownBlock(_) => Ok::<_, BoxError>(zs::Response::KnownBlock(None)),
                    zs::Request::MiningCandidate(_) => Ok(zs::Response::MiningCandidate(None)),
                    zs::Request::CommitContextuallyVerifiedBlock(_) if cached => {
                        Err("untyped state commit failure".into())
                    }
                    zs::Request::CommitSemanticallyVerifiedBlock(_) if !cached => {
                        Err("untyped state commit failure".into())
                    }
                    zs::Request::RejectBlock(_) => {
                        panic!("an untyped commit error must not record a rejection")
                    }
                    other => panic!("unexpected state request: {other:?}"),
                }
            });
            verifier.state_service = Buffer::new(BoxService::new(state), 10);
            let error = verifier.oneshot(Request::Commit(solved)).await.unwrap_err();
            assert!(
                matches!(&error, VerifyBlockError::StateService { source, .. }
                    if source.to_string() == "untyped state commit failure"),
                "commit errors must preserve their unclassified source: {error:?}"
            );
            assert!(!error.is_deterministic_rejection());
        }
    })
    .await
    .expect("untyped commit failures return without attempting rejection authentication");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_fee_and_contextual_spend_failures_suppress_replay() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        for incorrect_fee in [false, true] {
            let (network, state, read) = state().await;
            let block = candidate(&network, &read, true).await;
            let dispatches = Arc::new(AtomicUsize::new(0));
            let count = dispatches.clone();
            let transactions = tower::service_fn(move |request: tx::BlockRequest| {
                count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if incorrect_fee {
                        Err::<tx::BlockResponse, BoxError>(TransactionError::IncorrectFee.into())
                    } else {
                        Ok(tx::BlockResponse {
                            tx_id: request.transaction.unmined_id(),
                            sigops: 0,
                            miner_fee: None,
                        })
                    }
                }
            });
            let error = SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
                .oneshot(Request::CommitEarlyRelay(block.clone())).await.unwrap_err();
            if incorrect_fee {
                assert!(matches!(&error, VerifyBlockError::EarlyRelay(source)
                    if matches!(**source, VerifyBlockError::Transaction(TransactionError::IncorrectFee))));
            } else {
                assert!(matches!(&error,
                    VerifyBlockError::Commit(zs::CommitBlockError::ValidateContextError(source))
                    if matches!(**source, zs::ValidateContextError::MissingTransparentOutput { .. })
                ), "{error:?}");
            }
            assert_eq!(dispatches.load(Ordering::SeqCst), block.transactions.len());
            let error = SemanticBlockVerifier::new(&network, state, transactions)
                .oneshot(Request::CommitEarlyRelay(block.clone())).await.unwrap_err();
            assert!(matches!(error, VerifyBlockError::EarlyRelayRejected),
                "incorrect_fee={incorrect_fee}: replay returned {error:?}, transaction dispatches={}",
                dispatches.load(Ordering::SeqCst));
            assert_eq!(dispatches.load(Ordering::SeqCst), block.transactions.len());
        }
    }).await.expect("deterministic failures must suppress fresh transaction work");
}
