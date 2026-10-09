//! End-to-end proposal reuse and mining-only speculative state tests.

use super::super::{Request, SemanticBlockVerifier};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tower::{buffer::Buffer, util::BoxService, ServiceExt};
use zebra_chain::{
    block::{Block, Height},
    parameters::{
        testnet::{ConfiguredActivationHeights, ConfiguredCheckpoints, Parameters},
        Network,
    },
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::UnminedTxId,
    work::{difficulty::ParameterDifficulty, equihash},
};
use zebra_state::{self as zs, ReadRequest, ReadResponse};

fn block(height: u32) -> Arc<Block> {
    Arc::new(Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&height]).unwrap())
}

fn network(disable_pow: bool) -> Network {
    // Keep actual Mainnet fixture PoW and slow-start subsidy, but activate Heartwood/Canopy
    // at the tested child. Its reserved activation commitment is the fixture's zero root.
    Parameters::build()
        .with_genesis_hash(block(0).hash())
        .unwrap()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(2),
            ..Default::default()
        })
        .unwrap()
        // Early Blossom retains the fixture's slow start but needs a shorter capped schedule.
        .with_halving_interval(800_000)
        .unwrap()
        .with_target_difficulty_limit(Network::Mainnet.target_difficulty_limit())
        .unwrap()
        .with_funding_streams(Vec::new())
        .with_disable_pow(disable_pow)
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (Height(0), block(0).hash()),
            (Height(1), block(1).hash()),
        ]))
        .unwrap()
        .to_network()
        .unwrap()
}

async fn state(
    disable_pow: bool,
) -> (
    Buffer<BoxService<zs::Request, zs::Response, zs::BoxError>, zs::Request>,
    zs::ReadStateService,
    Network,
) {
    let network = network(disable_pow);
    let (state, read, _, _) = zs::init(zs::Config::ephemeral(), &network, Height(1), 0).await;
    let state = Buffer::new(state, 1);
    for height in 0..=1 {
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                block(height).into(),
            ))
            .await
            .unwrap();
    }
    (state, read, network)
}

#[tokio::test(flavor = "multi_thread")]
async fn completed_proposal_skips_transaction_verification_and_checks_pow() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        for hint in 0..3 {
            let (state, read, network) = state(false).await;
            let calls = Arc::new(AtomicUsize::new(0));
            let transactions = tower::service_fn({
                let calls = calls.clone();
                move |request: crate::transaction::BlockRequest| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        Ok::<_, zs::BoxError>(crate::transaction::BlockResponse {
                            tx_id: UnminedTxId::from(request.transaction.as_ref()),
                            miner_fee: None,
                            sigops: 0,
                        })
                    }
                }
            });
            let verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions);
            let solved = block(2);
            let mut proposal = (*solved).clone();
            Arc::make_mut(&mut proposal.header).nonce = [0; 32].into();
            Arc::make_mut(&mut proposal.header).solution = equihash::Solution::Common([0; 1344]);
            verifier
                .clone()
                .oneshot(Request::CheckProposal(Arc::new(proposal)))
                .await
                .unwrap();
            let verified = calls.load(Ordering::SeqCst);
            assert!(verified > 0);

            let mut malformed = (*solved).clone();
            Arc::make_mut(&mut malformed.header).solution = equihash::Solution::Common([0; 1344]);
            let pow_network = network.clone();
            let malformed = tokio::task::spawn_blocking(move || {
                for nonce in 0u64..1_000_000 {
                    Arc::make_mut(&mut malformed.header).nonce.0[..8]
                        .copy_from_slice(&nonce.to_le_bytes());
                    let hash = malformed.hash();
                    if super::super::check::difficulty_is_valid(
                        &malformed.header,
                        &pow_network,
                        &Height(2),
                        &hash,
                    )
                    .is_ok()
                    {
                        return Arc::new(malformed);
                    }
                }
                panic!("bounded nonce search must find a difficulty-valid malformed solution")
            })
            .await
            .unwrap();
            assert!(matches!(
                state
                    .clone()
                    .oneshot(zs::Request::ReusableBlockProposal(malformed.clone()))
                    .await
                    .unwrap(),
                zs::Response::ReusableBlockProposal(Some(_))
            ));
            assert!(
                matches!(
                    verifier.clone().oneshot(Request::Commit(malformed)).await,
                    Err(super::super::VerifyBlockError::Equihash { .. })
                ),
                "Equihash must be freshly checked on an exact cache match"
            );
            assert_eq!(calls.load(Ordering::SeqCst), verified);

            for change in 0..5 {
                let mut different = (*solved).clone();
                match change {
                    0 => Arc::make_mut(&mut different.header).version += 1,
                    1 => {
                        let header = Arc::make_mut(&mut different.header);
                        header.time += chrono::Duration::seconds(1);
                    }
                    2 => {
                        Arc::make_mut(&mut different.header).difficulty_threshold =
                            zebra_chain::work::difficulty::INVALID_COMPACT_DIFFICULTY
                    }
                    3 => Arc::make_mut(&mut different.header).previous_block_hash = block(0).hash(),
                    _ => different
                        .transactions
                        .push(different.transactions.last().unwrap().clone()),
                }
                assert!(matches!(
                    state
                        .clone()
                        .oneshot(zs::Request::ReusableBlockProposal(Arc::new(
                            different.clone()
                        )))
                        .await
                        .unwrap(),
                    zs::Response::ReusableBlockProposal(None)
                ));
                assert!(matches!(
                    state
                        .clone()
                        .oneshot(zs::Request::ReusableBlockProposalWithWorkId {
                            block: Arc::new(different),
                            work_id: zs::proposal_key(&solved),
                        })
                        .await
                        .unwrap(),
                    zs::Response::ReusableBlockProposal(None)
                ));
            }
            let request = match hint {
                0 => Request::Commit(solved.clone()),
                1 => Request::CommitWithWorkId {
                    block: solved.clone(),
                    work_id: zs::proposal_key(&solved),
                },
                _ => Request::CommitWithWorkId {
                    block: solved.clone(),
                    work_id: [0; 32],
                },
            };
            verifier.oneshot(request).await.unwrap();
            let expected = if hint == 2 { verified * 2 } else { verified };
            assert_eq!(
                calls.load(Ordering::SeqCst),
                expected,
                "exact hints skip proofs, wrong hints fall back to full verification"
            );
            assert_eq!(
                read.clone().oneshot(ReadRequest::Tip).await.unwrap(),
                ReadResponse::Tip(Some((Height(2), solved.hash())))
            );
            assert!(matches!(
                state
                    .oneshot(zs::Request::ReusableBlockProposal(solved))
                    .await
                    .unwrap(),
                zs::Response::ReusableBlockProposal(None)
            ));
        }
    })
    .await
    .expect("proposal reuse test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn speculation_precedes_proofs_and_rejection_restores_validated_work() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, read, network) = state(false).await;
        let ReadResponse::MiningTipChange(mut updates) = read
            .clone()
            .oneshot(ReadRequest::MiningTipChange)
            .await
            .unwrap()
        else {
            panic!("wrong subscription response")
        };
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let transactions = tower::service_fn({
            let entered = entered.clone();
            let release = release.clone();
            move |_: crate::transaction::BlockRequest| {
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    entered.notify_one();
                    release.notified().await;
                    Err::<crate::transaction::BlockResponse, zs::BoxError>(
                        "deliberate proof rejection".into(),
                    )
                }
            }
        });
        let child = block(2);
        let verifier = SemanticBlockVerifier::new(&network, state, transactions);
        let verify = tokio::spawn(verifier.clone().oneshot(Request::Commit(child.clone())));
        entered.notified().await;
        assert_eq!(
            read.clone().oneshot(ReadRequest::MiningTip).await.unwrap(),
            ReadResponse::Tip(Some((Height(2), child.hash())))
        );
        assert_eq!(
            read.clone().oneshot(ReadRequest::Tip).await.unwrap(),
            ReadResponse::Tip(Some((Height(1), block(1).hash())))
        );
        assert_eq!(
            read.clone()
                .oneshot(ReadRequest::Block(child.hash().into()))
                .await
                .unwrap(),
            ReadResponse::Block(None)
        );
        let ReadResponse::ChainInfo(info) = read
            .clone()
            .oneshot(ReadRequest::MiningChainInfo)
            .await
            .unwrap()
        else {
            panic!("wrong mining info response")
        };
        assert_eq!(info.tip_hash, child.hash());
        let ReadResponse::ChainInfo(validated) =
            read.clone().oneshot(ReadRequest::ChainInfo).await.unwrap()
        else {
            panic!("wrong validated info response")
        };
        assert_eq!(validated.tip_hash, block(1).hash());
        updates.receiver.borrow_and_update();
        release.notify_one();
        assert!(verify.await.unwrap().is_err());
        updates.receiver.changed().await.unwrap();
        assert_eq!(
            *updates.receiver.borrow_and_update(),
            Some((Height(1), block(1).hash()))
        );
        assert_eq!(
            read.clone().oneshot(ReadRequest::MiningTip).await.unwrap(),
            ReadResponse::Tip(Some((Height(1), block(1).hash())))
        );
        let verify = tokio::spawn(verifier.oneshot(Request::Commit(child)));
        entered.notified().await;
        updates.receiver.borrow_and_update();
        verify.abort();
        assert!(verify.await.unwrap_err().is_cancelled());
        updates.receiver.changed().await.unwrap();
        assert_eq!(
            *updates.receiver.borrow_and_update(),
            Some((Height(1), block(1).hash()))
        );
        assert_eq!(
            read.oneshot(ReadRequest::MiningTip).await.unwrap(),
            ReadResponse::Tip(Some((Height(1), block(1).hash())))
        );
    })
    .await
    .expect("speculative verification test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn private_parent_requires_writer_admission_and_never_exposes_an_uncommitted_body() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, read, network) = state(false).await;
        let solved = block(2);
        let ReadResponse::MiningTipChange(mut updates) = read
            .clone()
            .oneshot(ReadRequest::MiningTipChange)
            .await
            .unwrap()
        else {
            panic!("wrong mining subscription response")
        };
        assert!(
            matches!(
                state
                    .clone()
                    .oneshot(zs::Request::AdmitPreparedMiningBlock(solved.clone()))
                    .await
                    .unwrap(),
                zs::Response::MiningStaged(None)
            ),
            "uncached candidates never reserve a private parent"
        );

        let state = tower::util::BoxCloneService::new(state);
        let entered_commit = Arc::new(tokio::sync::Notify::new());
        let release_commit = Arc::new(tokio::sync::Notify::new());
        let gated_state = tower::service_fn({
            let state = state.clone();
            let entered = entered_commit.clone();
            let release = release_commit.clone();
            move |request: zs::Request| {
                let state = state.clone();
                let entered = entered.clone();
                let release = release.clone();
                async move {
                    if matches!(&request, zs::Request::CommitSemanticallyVerifiedBlock(_)) {
                        entered.notify_one();
                        release.notified().await;
                    }
                    state.oneshot(request).await
                }
            }
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let transactions = tower::service_fn({
            let calls = calls.clone();
            move |request: crate::transaction::BlockRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok::<_, zs::BoxError>(crate::transaction::BlockResponse {
                        tx_id: UnminedTxId::from(request.transaction.as_ref()),
                        miner_fee: None,
                        sigops: 0,
                    })
                }
            }
        });
        let verifier = SemanticBlockVerifier::new(&network, gated_state, transactions);
        verifier
            .clone()
            .oneshot(Request::CheckProposal(solved.clone()))
            .await
            .unwrap();
        let verified = calls.load(Ordering::SeqCst);
        assert_eq!(
            *updates.receiver.borrow(),
            Some((Height(1), block(1).hash())),
            "preparing a proposal does not change the mining parent"
        );

        // Even an operator request that cannot invalidate a finalized block discards old admission
        // context. A stale completed proposal must not admit before it is validated again.
        assert!(state
            .clone()
            .oneshot(zs::Request::InvalidateBlock(block(0).hash()))
            .await
            .is_err());
        assert!(matches!(
            state
                .clone()
                .oneshot(zs::Request::AdmitPreparedMiningBlock(solved.clone()))
                .await
                .unwrap(),
            zs::Response::MiningStaged(None)
        ));
        verifier
            .clone()
            .oneshot(Request::CheckProposal(solved.clone()))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), verified * 2);

        let commit = tokio::spawn(verifier.clone().oneshot(Request::Commit(solved.clone())));
        entered_commit.notified().await;
        assert_eq!(
            *updates.receiver.borrow_and_update(),
            Some((Height(2), solved.hash()))
        );
        assert!(
            matches!(
                state
                    .clone()
                    .oneshot(zs::Request::AdmitPreparedMiningBlock(solved.clone()))
                    .await
                    .unwrap(),
                zs::Response::MiningStaged(None)
            ),
            "one direct child reservation is exclusive"
        );
        assert_eq!(
            read.clone().oneshot(ReadRequest::Tip).await.unwrap(),
            ReadResponse::Tip(Some((Height(1), block(1).hash())))
        );
        assert_eq!(
            read.clone()
                .oneshot(ReadRequest::Block(solved.hash().into()))
                .await
                .unwrap(),
            ReadResponse::Block(None)
        );
        release_commit.notify_one();
        assert_eq!(commit.await.unwrap().unwrap(), solved.hash());
        assert_eq!(
            calls.load(Ordering::SeqCst),
            verified * 2,
            "admitted submission reuses completed transaction verification"
        );
        updates.receiver.changed().await.unwrap();
        assert_eq!(
            *updates.receiver.borrow_and_update(),
            Some((Height(2), solved.hash()))
        );
        assert_eq!(
            read.clone()
                .oneshot(ReadRequest::Block(solved.hash().into()))
                .await
                .unwrap(),
            ReadResponse::Block(Some(solved.clone()))
        );
        let mut next = (*block(3)).clone();
        let ReadResponse::ChainInfo(info) =
            read.clone().oneshot(ReadRequest::ChainInfo).await.unwrap()
        else {
            panic!("wrong chain info response")
        };
        Arc::make_mut(&mut next.header).commitment_bytes =
            <[u8; 32]>::from(info.chain_history_root.unwrap()).into();
        let next = Arc::new(next);
        verifier
            .oneshot(Request::CheckProposal(next.clone()))
            .await
            .unwrap();
        assert!(matches!(
            state
                .clone()
                .oneshot(zs::Request::ReusableBlockProposal(next.clone()))
                .await
                .unwrap(),
            zs::Response::ReusableBlockProposal(Some(_))
        ));
        let ReadResponse::MiningTipChange(mut updates) = read
            .clone()
            .oneshot(ReadRequest::MiningTipChange)
            .await
            .unwrap()
        else {
            panic!("wrong mining subscription response")
        };
        state
            .clone()
            .oneshot(zs::Request::InvalidateBlock(solved.hash()))
            .await
            .unwrap();
        while *updates.receiver.borrow_and_update() != Some((Height(1), block(1).hash())) {
            updates.receiver.changed().await.unwrap();
        }
        assert!(
            matches!(
                state
                    .oneshot(zs::Request::ReusableBlockProposal(next))
                    .await
                    .unwrap(),
                zs::Response::ReusableBlockProposal(None)
            ),
            "invalidating the exact parent discards completed child validation"
        );
        assert_eq!(
            read.oneshot(ReadRequest::Tip).await.unwrap(),
            ReadResponse::Tip(Some((Height(1), block(1).hash())))
        );
    })
    .await
    .expect("writer admission test must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn short_solution_proposal_cannot_authorize_an_oversized_long_solution() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let _init_guard = zebra_test::init();
        let (state, _, network) = state(true).await;
        let mut short = (*block(2)).clone();
        Arc::make_mut(&mut short.header).solution = equihash::Solution::Regtest([0; 36]);
        let original = &short.transactions[0];
        let inputs = original.inputs();
        let lock_time = original
            .lock_time()
            .unwrap_or_else(zebra_chain::transaction::LockTime::unlocked);
        let mut outputs = original.outputs();
        outputs[0].lock_script = zebra_chain::transparent::Script::new(&[]);
        short.transactions[0] = Arc::new(zebra_chain::transaction::Transaction::test_v1(
            inputs.clone(),
            outputs.clone(),
            lock_time,
        ));
        let target = usize::try_from(zebra_chain::block::MAX_BLOCK_BYTES).unwrap() - 100;
        // A two-million-byte script has a five-byte CompactSize instead of the empty script's one.
        let script_len = target - short.zcash_serialized_size() - 4;
        outputs[0].lock_script = zebra_chain::transparent::Script::new(&vec![0; script_len]);
        short.transactions[0] = Arc::new(zebra_chain::transaction::Transaction::test_v1(
            inputs, outputs, lock_time,
        ));
        Arc::make_mut(&mut short.header).merkle_root =
            short.transactions.iter().map(|tx| tx.hash()).collect();
        let short = Arc::new(short);
        let mut long = (*short).clone();
        Arc::make_mut(&mut long.header).solution = equihash::Solution::Common([0; 1344]);
        let long = Arc::new(long);
        assert_eq!(short.zcash_serialized_size(), target);
        assert!(
            u64::try_from(long.zcash_serialized_size()).unwrap()
                > zebra_chain::block::MAX_BLOCK_BYTES
        );
        assert_eq!(zs::proposal_key(&short), zs::proposal_key(&long));
        let calls = Arc::new(AtomicUsize::new(0));
        let transactions = tower::service_fn({
            let calls = calls.clone();
            move |request: crate::transaction::BlockRequest| {
                calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    Ok::<_, zs::BoxError>(crate::transaction::BlockResponse {
                        tx_id: UnminedTxId::from(request.transaction.as_ref()),
                        miner_fee: None,
                        sigops: 0,
                    })
                }
            }
        });
        let verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions);
        verifier
            .clone()
            .oneshot(Request::CheckProposal(short.clone()))
            .await
            .unwrap();
        assert!(
            calls.load(Ordering::SeqCst) > 0,
            "short proposal completed semantic verification"
        );
        assert!(
            matches!(
                state
                    .oneshot(zs::Request::ReusableBlockProposal(short))
                    .await
                    .unwrap(),
                zs::Response::ReusableBlockProposal(None)
            ),
            "valid short proposals above the conservative normalized cap are not cached"
        );
        assert!(matches!(
            verifier.oneshot(Request::Commit(long)).await,
            Err(super::super::VerifyBlockError::Block {
                source: crate::error::BlockError::BlockTooLarge { .. }
            })
        ));
    })
    .await
    .expect("solution-size boundary test must not stall");
}
