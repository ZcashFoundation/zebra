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
            Height(1),
        ));
        block.transactions.push(Arc::new(shielded));
        Arc::make_mut(&mut block.header).merkle_root =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        // This fixture extends genesis, before a chain-history root exists.
        let block = seal_candidate(&network, block, None);
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
async fn early_relay_transient_failures_retry_without_recording_rejection() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let block = candidate(&network, &read, true).await;
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
            for delivery in 1..=2 {
                let error =
                    SemanticBlockVerifier::new(&network, state.clone(), transactions.clone())
                        .oneshot(Request::CommitEarlyRelay(block.clone()))
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
