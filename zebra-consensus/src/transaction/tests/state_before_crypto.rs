//! State rejection and backpressure must precede cryptographic verification.

use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::transaction::UTXO_LOOKUP_TIMEOUT;

/// A proof-valid, fee-sufficient transaction must reach the production unknown-anchor rejection
/// without a peer penalty or a Sapling verifier submission. This is not a P2P performance test.
#[test]
fn mempool_unknown_sapling_anchor_rejection_precedes_proof_verification() {
    let _init_guard = zebra_test::init();

    // The writer, buffer worker, and RocksDB metrics task all belong to this runtime. It shuts
    // down before the test returns; only the global cryptographic workers use the shared runtime.
    let state_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the state runtime must initialize");
    let (state, _read_state, _, _) =
        state_runtime.block_on(zebra_state::init_test_services(&Network::Mainnet));

    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let (network_upgrade, tx) = cacheable_mainnet_sapling_transaction();
        let height = tx
            .expiry_height()
            .expect("a V4 or V5 transaction has an expiry height");
        let fee = crate::transaction::miner_fee(&tx, &HashMap::new())
            .expect("a Sapling-only fixture has a fee without transparent UTXOs");
        assert!(fee >= zip317::conventional_fee(&tx));
        let item = sapling_item(&tx, network_upgrade);
        timeout(
            test_timeout(),
            crate::primitives::sapling::verify_single(item.clone()),
        )
        .await
        .expect("Sapling verification completes")
        .expect("the mainnet fixture has valid Sapling proofs and signatures");
        let transaction: zebra_chain::transaction::UnminedTx = Arc::new(tx).into();
        zip317::mempool_checks(
            zip317::unpaid_actions(&transaction, fee),
            fee,
            transaction.size,
        )
        .expect("the fixture independently satisfies the mempool fee policy");
        let verifier = crate::primitives::sapling::VERIFIER.isolated();

        let result = SAPLING_CACHE_VERIFIER
            .scope(verifier.clone(), async {
                timeout(
                    test_timeout(),
                    MempoolTxVerifier::new_for_tests(&Network::Mainnet, state).oneshot(
                        MempoolRequest {
                            transaction,
                            height,
                        },
                    ),
                )
                .await
                .expect("mempool verification completes")
            })
            .await;

        assert!(
            matches!(
                &result,
                Err(TransactionError::ValidateContextError(error))
                    if matches!(**error, ValidateContextError::UnknownSaplingAnchor { .. })
            ),
            "the production state must reject the unknown Sapling anchor: {result:?}"
        );
        assert_eq!(
            result
                .expect_err("the unknown anchor must reject the transaction")
                .mempool_misbehavior_score(),
            0,
            "a contextual rejection must not penalize an honest peer on another tip"
        );
        assert_eq!(
            crate::primitives::sapling::inner_calls_for(&verifier, &item),
            0,
            "a transaction rejected by the state must not reach the Sapling verifier"
        );
    });
}

/// A pending state response must hold back proofs, then a valid response must allow verification.
#[test]
fn mempool_valid_state_response_releases_proof_verification() {
    let _init_guard = zebra_test::init();

    zebra_test::MULTI_THREADED_RUNTIME.block_on(async {
        let (network_upgrade, tx) = cacheable_mainnet_sapling_transaction();
        let height = tx
            .expiry_height()
            .expect("a V4 or V5 transaction has an expiry height");
        let item = sapling_item(&tx, network_upgrade);
        let verifier = crate::primitives::sapling::VERIFIER.isolated();
        SAPLING_CACHE_VERIFIER
            .scope(verifier.clone(), async {
                let mut state: CacheTestState = MockService::build().for_prop_tests();
                let verification =
                    MempoolTxVerifier::new_for_tests(&Network::Mainnet, state.clone()).oneshot(
                        MempoolRequest {
                            transaction: Arc::new(tx).into(),
                            height,
                        },
                    );
                tokio::pin!(verification);
                assert!(futures::poll!(&mut verification).is_pending());
                let response = state
                    .expect_request_that(|request| {
                        matches!(
                            request,
                            zebra_state::Request::CheckBestChainTipNullifiersAndAnchors(_)
                        )
                    })
                    .await
                    .expect("mempool verification must check nullifiers and anchors");
                assert_eq!(
                    crate::primitives::sapling::inner_calls_for(&verifier, &item),
                    0,
                    "proof verification must wait for the state response"
                );
                response.respond(zebra_state::Response::ValidBestChainTipNullifiersAndAnchors);
                timeout(test_timeout(), verification)
                    .await
                    .expect("mempool verification completes")
                    .expect("the valid state response must allow the real fixture to verify");
                assert_eq!(
                    crate::primitives::sapling::inner_calls_for(&verifier, &item),
                    1,
                    "a transaction accepted by state must reach the isolated verifier"
                );
            })
            .await;
    });
}

/// The state deadline includes service readiness as well as a pending response, without proofs.
#[test]
fn mempool_anchor_state_readiness_and_response_keep_their_deadline() {
    let _init_guard = zebra_test::init();
    let verifier = {
        let _runtime_guard = zebra_test::MULTI_THREADED_RUNTIME.enter();
        crate::primitives::sapling::VERIFIER.isolated()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .start_paused(true)
        .build()
        .expect("the paused test runtime must initialize");

    runtime.block_on(async {
        for block_readiness in [true, false] {
            let (network_upgrade, tx) = cacheable_mainnet_sapling_transaction();
            let height = tx
                .expiry_height()
                .expect("a V4 or V5 transaction has an expiry height");
            let item = sapling_item(&tx, network_upgrade);
            let calls = Arc::new(AtomicUsize::new(0));
            let state_calls = calls.clone();
            let state = tower::limit::ConcurrencyLimit::new(
                service_fn(move |_: zebra_state::Request| {
                    state_calls.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<Result<zebra_state::Response, crate::BoxError>>()
                }),
                1,
            );
            let mut held = state.clone();
            if block_readiness {
                held.ready()
                    .await
                    .expect("the unused state permit must be available");
            }
            let started = tokio::time::Instant::now();
            let result = SAPLING_CACHE_VERIFIER
                .scope(verifier.clone(), async {
                    timeout(
                        UTXO_LOOKUP_TIMEOUT + std::time::Duration::from_secs(1),
                        MempoolTxVerifier::new_for_tests(&Network::Mainnet, state).oneshot(
                            MempoolRequest {
                                transaction: Arc::new(tx).into(),
                                height,
                            },
                        ),
                    )
                    .await
                    .expect("the anchor check must use the existing state deadline")
                })
                .await;
            assert!(matches!(
                result,
                Err(TransactionError::InternalDowncastError(_))
            ));
            assert_eq!(started.elapsed(), UTXO_LOOKUP_TIMEOUT);
            assert_eq!(calls.load(Ordering::SeqCst), usize::from(!block_readiness));
            assert_eq!(
                crate::primitives::sapling::inner_calls_for(&verifier, &item),
                0,
                "state backpressure must never submit proofs"
            );
        }
    });
}
