//! Admission, scoring, and state-serving overlap for negotiated early relay.

use super::*;
use zebra_chain::{
    amount::NonNegative, block::ChainHistoryBlockTxAuthCommitmentHash,
    parameters::testnet::ConfiguredActivationHeights,
};
use zebra_test::mock_service::MockService;

mod rejections;

async fn state() -> (Network, ProposalTestState, zs::ReadStateService) {
    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let (state, read, _, _) = zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
    let state = Buffer::new(state, 10);
    state
        .clone()
        .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
            block::genesis::regtest_genesis_block().into(),
        ))
        .await
        .unwrap();
    (network, state, read)
}

async fn candidate(network: &Network, read: &zs::ReadStateService, spend: bool) -> Arc<Block> {
    let zs::ReadResponse::ChainInfo(info) = read
        .clone()
        .oneshot(zs::ReadRequest::ChainInfo)
        .await
        .unwrap()
    else {
        panic!("chain info response")
    };
    let height = info.tip_height.next().unwrap();
    let mut transactions = vec![Arc::new(Transaction::test_v5(
        NetworkUpgrade::Nu5,
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            zebra_chain::parameters::subsidy::block_subsidy(height, network).unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        height,
    ))];
    if spend {
        transactions.push(Arc::new(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [8; 32].into(),
                    index: 0,
                },
                unlock_script: transparent::Script::new(&[0x51]),
                sequence: u32::MAX,
            }],
            vec![transparent::Output::new(
                Amount::<NonNegative>::try_from(1).unwrap(),
                transparent::Script::new(&[0x51]),
            )],
            LockTime::unlocked(),
            height,
        )));
    }
    let block = Block {
        header: Arc::new(block::Header {
            version: 4,
            previous_block_hash: info.tip_hash,
            merkle_root: transactions.iter().map(|tx| tx.hash()).collect(),
            commitment_bytes: [0; 32].into(),
            time: info.cur_time.into(),
            difficulty_threshold: info.expected_difficulty,
            nonce: [0; 32].into(),
            solution: equihash::Solution::Regtest([0; 36]),
        }),
        transactions,
    };
    seal_candidate(network, block, info.chain_history_root)
}

fn seal_candidate(
    network: &Network,
    mut block: Block,
    history_root: Option<block::ChainHistoryMmrRootHash>,
) -> Arc<Block> {
    let height = block.coinbase_height().unwrap();
    Arc::make_mut(&mut block.header).commitment_bytes =
        <[u8; 32]>::from(ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            &history_root.unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
            &block.auth_data_root(),
        ))
        .into();
    // Regtest deliberately disables Equihash. Still solve and check its actual difficulty filter;
    // the separate historical-header test below exercises production Equihash on untouched bytes.
    (0u32..100_000)
        .find(|nonce| {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&nonce.to_le_bytes());
            Arc::make_mut(&mut block.header).nonce = bytes.into();
            check::difficulty_is_valid(&block.header, network, &height, &block.hash()).is_ok()
        })
        .expect("a regtest target is reached within the bounded nonce search");
    Arc::new(block)
}

fn forge_authorizing_data(block: &Arc<Block>) -> Arc<Block> {
    let mut forged = (**block).clone();
    let tx = forged
        .transactions
        .iter_mut()
        .find(|tx| tx.version() == 5 && !tx.is_coinbase() && tx.has_transparent_inputs())
        .unwrap();
    let mut inputs = tx.inputs();
    let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
        unreachable!()
    };
    *unlock_script = transparent::Script::new(&[0x52]);
    *tx = Arc::new((**tx).clone().with_transparent_inputs(inputs));
    assert_eq!(forged.header, block.header);
    assert!(forged
        .transactions
        .iter()
        .map(|tx| tx.hash())
        .eq(block.transactions.iter().map(|tx| tx.hash())));
    assert_ne!(forged.auth_data_root(), block.auth_data_root());
    Arc::new(forged)
}

async fn cache(read: &zs::ReadStateService) -> zs::EarlyRelayCache {
    let zs::ReadResponse::EarlyRelayCache(cache) = read
        .clone()
        .oneshot(zs::ReadRequest::EarlyRelayCache)
        .await
        .unwrap()
    else {
        panic!("cache response")
    };
    cache
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_authenticated_tx_failure_is_nonpunitive() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let block = candidate(&network, &read, true).await;
        let cache = cache(&read).await;
        let changes = cache.subscribe();
        let mut transactions: MockService<tx::BlockRequest, tx::BlockResponse, _, BoxError> =
            MockService::build().for_unit_tests();
        let verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions.clone());
        let verification = tokio::spawn(verifier.oneshot(Request::CommitEarlyRelay(block.clone())));
        let first = transactions.expect_request_that(|_| true).await;
        let second = transactions.expect_request_that(|_| true).await;
        assert_eq!(*changes.borrow(), Some(block.hash()));
        assert!(Arc::ptr_eq(&cache.get(&block.hash()).unwrap(), &block));
        assert!(
            !verification.is_finished(),
            "body is servable before transaction verification"
        );
        first.respond_error(TransactionError::BadBalance.into());
        second.respond_error(TransactionError::BadBalance.into());
        let error = verification.await.unwrap().unwrap_err();
        assert!(matches!(error, VerifyBlockError::EarlyRelay(_)));
        assert_eq!(error.misbehavior_score(), 0);
        assert!(cache.get(&block.hash()).is_none());
        let transactions = tower::service_fn(|_| async {
            Err::<tx::BlockResponse, BoxError>(TransactionError::BadBalance.into())
        });
        let error = SemanticBlockVerifier::new(&network, state, transactions)
            .oneshot(Request::Commit(block))
            .await
            .unwrap_err();
        assert_eq!(
            error.misbehavior_score(),
            100,
            "ordinary sources retain punishment"
        );
    })
    .await
    .expect("early-relay verification must finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_rejects_forged_authorizing_data_before_transactions() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let block = candidate(&network, &read, true).await;
        let cache = cache(&read).await;
        let admission = state
            .clone()
            .oneshot(zs::Request::EarlyRelay {
                block: block.clone(),
                require_authentication: true,
            })
            .await
            .unwrap();
        assert!(matches!(admission, zs::Response::EarlyRelay(Some(_))));
        assert!(cache.get(&block.hash()).is_some());
        drop(admission);
        assert!(cache.get(&block.hash()).is_none());
        let forged = forge_authorizing_data(&block);
        check::difficulty_is_valid(
            &forged.header,
            &network,
            &forged.coinbase_height().unwrap(),
            &forged.hash(),
        )
        .unwrap();
        let transactions = tower::service_fn(
            |_| -> std::future::Ready<Result<tx::BlockResponse, BoxError>> {
                panic!("forged bodies must never reach transaction validation")
            },
        );
        let error = SemanticBlockVerifier::new(&network, state.clone(), transactions)
            .oneshot(Request::CommitEarlyRelay(forged))
            .await
            .unwrap_err();
        assert!(error.is_auth_commitment_mismatch());
        assert_eq!(error.misbehavior_score(), 100);
        assert!(cache.get(&block.hash()).is_none());
        // A forged variant must not poison later admission of the genuine same-hash body.
        let zs::Response::EarlyRelay(Some(guard)) = state
            .oneshot(zs::Request::EarlyRelay {
                block,
                require_authentication: true,
            })
            .await
            .unwrap()
        else {
            panic!("the genuine body must still authenticate after a forged delivery")
        };
        assert!(!guard.is_rejected());
    })
    .await
    .expect("forged-body rejection must finish");
}

#[test]
fn early_relay_forgery_preserves_real_mainnet_pow_and_merkle_root() {
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let forged = forge_authorizing_data(&block);
    for block in [block, forged] {
        check::difficulty_is_valid(
            &block.header,
            &Network::Mainnet,
            &block.coinbase_height().unwrap(),
            &block.hash(),
        )
        .unwrap();
        check::equihash_solution_is_valid(&block.header).unwrap();
        let hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        check::merkle_root_validity(&Network::Mainnet, &block, &hashes).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_waits_for_parent_and_authenticates_non_tip_parent() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (network, source, source_read) = state().await;
        let (_, receiver, receiver_read) = state().await;
        let parent = candidate(&network, &source_read, false).await;
        let commit_parent = zs::Request::CommitSemanticallyVerifiedBlock(
            zs::CheckpointVerifiedBlock::from(parent).into(),
        );
        source.clone().oneshot(commit_parent.clone()).await.unwrap();
        let child = candidate(&network, &source_read, true).await;
        let competitor = candidate(&network, &source_read, false).await;
        let cache = cache(&receiver_read).await;
        let mut admission = tokio::spawn(receiver.clone().oneshot(zs::Request::EarlyRelay {
            block: child.clone(),
            require_authentication: true,
        }));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut admission,)
                .await
                .is_err(),
            "unknown parents must wait without granting authentication"
        );
        assert!(cache.get(&child.hash()).is_none());
        receiver.clone().oneshot(commit_parent).await.unwrap();
        let admitted = admission.await.unwrap().unwrap();
        assert!(matches!(admitted, zs::Response::EarlyRelay(Some(_))));
        drop(admitted);
        assert!(cache.get(&child.hash()).is_none());
        receiver
            .clone()
            .oneshot(zs::Request::CommitSemanticallyVerifiedBlock(
                zs::CheckpointVerifiedBlock::from(competitor).into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            receiver
                .clone()
                .oneshot(zs::Request::EarlyRelay {
                    block: child.clone(),
                    require_authentication: true,
                })
                .await
                .unwrap(),
            zs::Response::EarlyRelay(Some(_))
        ));
        assert!(
            cache.get(&child.hash()).is_none(),
            "receiving forks must not publish them"
        );
        assert!(matches!(
            receiver
                .oneshot(zs::Request::EarlyRelay {
                    block: child,
                    require_authentication: false,
                })
                .await
                .unwrap(),
            zs::Response::EarlyRelay(None)
        ));
    })
    .await
    .expect("pipelined parent admission must finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_pre_nu5_keeps_ordinary_scoring() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let block: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&9]
            .zcash_deserialize_into()
            .unwrap();
        let state = tower::service_fn(|request| async move {
            Ok::<_, BoxError>(match request {
                zs::Request::KnownBlock(_) => zs::Response::KnownBlock(None),
                zs::Request::MiningCandidate(_) => zs::Response::MiningCandidate(None),
                _ => panic!("pre-NU5 negotiated bodies use ordinary validation"),
            })
        });
        let transactions = tower::service_fn(|_| async {
            Err::<tx::BlockResponse, BoxError>(TransactionError::BadBalance.into())
        });
        let error = SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
            .oneshot(Request::CommitEarlyRelay(block))
            .await
            .unwrap_err();
        assert!(matches!(error, VerifyBlockError::Transaction(_)));
        assert_eq!(error.misbehavior_score(), 100);
    })
    .await
    .expect("pre-NU5 validation must finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_bad_pow_and_malformed_bodies_still_score() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let valid: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES
            .zcash_deserialize_into()
            .unwrap();
        for invalidity in 0..3 {
            let mut block = (*valid).clone();
            match invalidity {
                0 => Arc::make_mut(&mut block.header).solution = equihash::Solution::for_proposal(),
                1 => {
                    block.transactions.pop();
                }
                _ => {
                    let tx = block.transactions[0].clone();
                    let count = usize::try_from(block::MAX_BLOCK_BYTES).unwrap()
                        / tx.zcash_serialized_size()
                        + 1;
                    block.transactions = vec![tx; count];
                }
            }
            let state = tower::service_fn(|request| async move {
                assert!(matches!(request, zs::Request::KnownBlock(_)));
                Ok::<_, BoxError>(zs::Response::KnownBlock(None))
            });
            let transactions = tower::service_fn(|_| {
                std::future::pending::<Result<tx::BlockResponse, BoxError>>()
            });
            let error = SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
                .oneshot(Request::CommitEarlyRelay(Arc::new(block)))
                .await
                .unwrap_err();
            assert_eq!(error.misbehavior_score(), 100);
            assert!(!matches!(error, VerifyBlockError::EarlyRelay(_)));
        }
    })
    .await
    .expect("malicious admission failures must finish");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_relay_cancelled_verifier_withdraws_body() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let block = candidate(&network, &read, true).await;
        let cache = cache(&read).await;
        let mut transactions: MockService<tx::BlockRequest, tx::BlockResponse, _, BoxError> =
            MockService::build().for_unit_tests();
        let verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions.clone());
        let verification = tokio::spawn(verifier.oneshot(Request::CommitEarlyRelay(block.clone())));
        let pending = transactions.expect_request_that(|_| true).await;
        assert!(cache.get(&block.hash()).is_some());
        verification.abort();
        assert!(verification.await.unwrap_err().is_cancelled());
        assert!(cache.get(&block.hash()).is_none());
        drop(pending);
        let zs::Response::EarlyRelay(Some(guard)) = state
            .oneshot(zs::Request::EarlyRelay {
                block,
                require_authentication: true,
            })
            .await
            .unwrap()
        else {
            panic!("cancellation must not remove the authentication proof")
        };
        assert!(!guard.is_rejected());
    })
    .await
    .expect("verifier cancellation must release early retention");
}

/// Local invalidation prevents early replay without blaming a peer for this node's policy.
#[tokio::test(flavor = "multi_thread")]
async fn early_relay_rejects_locally_invalidated_hash() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (network, state, read) = state().await;
        let parent = candidate(&network, &read, false).await;
        let parent_hash = parent.hash();
        state
            .clone()
            .oneshot(zs::Request::CommitSemanticallyVerifiedBlock(
                zs::CheckpointVerifiedBlock::from(parent).into(),
            ))
            .await
            .unwrap();
        let block = candidate(&network, &read, false).await;
        state
            .clone()
            .oneshot(zs::Request::CommitSemanticallyVerifiedBlock(
                zs::CheckpointVerifiedBlock::from(block.clone()).into(),
            ))
            .await
            .unwrap();
        state
            .clone()
            .oneshot(zs::Request::InvalidateBlock(block.hash()))
            .await
            .unwrap();
        // The invalidate reply precedes publication of the updated read snapshot.
        while !matches!(
            read.clone().oneshot(zs::ReadRequest::Tip).await.unwrap(),
            zs::ReadResponse::Tip(Some((_, hash))) if hash == parent_hash
        ) {
            tokio::task::yield_now().await;
        }
        let cache = cache(&read).await;
        for require_authentication in [false, true] {
            let error = state
                .clone()
                .oneshot(zs::Request::EarlyRelay {
                    block: block.clone(),
                    require_authentication,
                })
                .await
                .unwrap_err()
                .downcast::<zs::ValidateContextError>()
                .unwrap();
            assert!(matches!(
                *error,
                zs::ValidateContextError::BlockPreviouslyInvalidated { block_hash, .. }
                    if block_hash == block.hash()
            ));
            assert_eq!(
                VerifyBlockError::EarlyRelayAdmission(*error).misbehavior_score(),
                0
            );
            assert!(cache.get(&block.hash()).is_none());
        }
    })
    .await
    .expect("local invalidation must reject admission without waiting for transactions");
}

#[test]
fn early_relay_missing_parent_remains_retryable() {
    let error = crate::router::RouterError::from(VerifyBlockError::EarlyRelayUnavailable);
    assert!(error.is_resource_pressure());
    assert_eq!(error.misbehavior_score(), 0);
}
