//! Exact solved submissions join a pending proposal, but cannot reuse a failed check.

use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::watch;

use super::*;

/// A solved block above Regtest's last mandatory checkpoint, with its proposal and live parent.
async fn regtest_proposal() -> (Network, ProposalTestState, Arc<Block>, Arc<Block>) {
    let network = Network::new_regtest(Default::default());
    let (state, read_state, _, _) = zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
    let state = Buffer::new(state, 10);
    let genesis = block::genesis::regtest_genesis_block();
    state
        .clone()
        .oneshot(zs::Request::CommitCheckpointVerifiedBlock(genesis.into()))
        .await
        .unwrap();
    let zs::ReadResponse::ChainInfo(info) = read_state
        .clone()
        .oneshot(zs::ReadRequest::ChainInfo)
        .await
        .unwrap()
    else {
        unreachable!("ChainInfo returns the current mining context")
    };

    let height = Height(1);
    let coinbase = Arc::new(Transaction::test_v4(
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![transparent::Output::new(
            zebra_chain::parameters::subsidy::block_subsidy(height, &network).unwrap(),
            transparent::Script::new(&[0x51]),
        )],
        LockTime::unlocked(),
        height,
    ));
    let proposal = Arc::new(Block {
        header: Arc::new(block::Header {
            version: 4,
            previous_block_hash: info.tip_hash,
            merkle_root: [coinbase.hash()].into_iter().collect(),
            commitment_bytes: info
                .chain_history_root
                .map(|root| root.bytes_in_serialized_order())
                .unwrap_or([0; 32])
                .into(),
            time: info.cur_time.into(),
            difficulty_threshold: info.expected_difficulty,
            nonce: [0; 32].into(),
            solution: equihash::Solution::for_proposal(),
        }),
        transactions: vec![coinbase],
    });
    let mut solved = (*proposal).clone();
    Arc::make_mut(&mut solved.header).nonce = [1; 32].into();
    Arc::make_mut(&mut solved.header).solution = equihash::Solution::Regtest([0; 36]);
    (network, state, proposal, Arc::new(solved))
}

#[tokio::test(flavor = "multi_thread")]
async fn solved_block_joins_in_flight_proposal() {
    exercise_join(false, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_in_flight_proposal_does_not_accept_submission() {
    exercise_join(true, false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelled_in_flight_proposal_falls_back_to_full_verification() {
    exercise_join(false, true, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn evicted_proposal_keeps_existing_joiners_but_new_submissions_verify_fully() {
    exercise_join(false, false, true).await;
}

async fn exercise_join(fail: bool, cancel: bool, evict: bool) {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (network, state, proposal, solved) = regtest_proposal().await;
        let transaction_count = solved.transactions.len();
        let (release, released) = watch::channel(false);
        let (cold_release, cold_released) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let dispatched = Arc::new(AtomicUsize::new(0));
        let transactions = tower::service_fn({
            let calls = calls.clone();
            let dispatched = dispatched.clone();
            move |request: tx::BlockRequest| {
                let mut released = released.clone();
                let mut cold_released = cold_released.clone();
                let calls = calls.clone();
                let index = dispatched.fetch_add(1, Ordering::SeqCst);
                async move {
                    if evict && index >= transaction_count {
                        cold_released.wait_for(|ready| *ready).await.unwrap();
                    } else {
                        released.wait_for(|ready| *ready).await.unwrap();
                    }
                    calls.fetch_add(1, Ordering::SeqCst);
                    if fail {
                        Err::<tx::BlockResponse, BoxError>(TransactionError::BadBalance.into())
                    } else {
                        Ok(tx::BlockResponse {
                            tx_id: request.transaction.unmined_id(),
                            miner_fee: None,
                            sigops: 0,
                        })
                    }
                }
            }
        });
        let mut verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions);
        let joined = verifier.in_flight_joins.clone();
        let check = verifier
            .ready()
            .await
            .unwrap()
            .call(Request::CheckProposal(proposal.clone()));
        let mut checking = tokio::spawn(check);
        while verifier.in_flight_proposals.0.lock().unwrap().is_empty() {
            assert!(
                !checking.is_finished(),
                "proposal must register its identity"
            );
            tokio::task::yield_now().await;
        }
        verifier.in_flight_proposals.0.lock().unwrap()[0].eligible_at = Instant::now();
        assert!(verifier.in_flight_proposals.has_header(&proposal.header));
        let bytes = solved.zcash_serialize_to_vec().unwrap();
        let decoded = verifier.prepared_blocks().deserialize(&bytes).unwrap();
        assert!(decoded.matched.is_none(), "no verdict is published yet");
        let submitted = tokio::spawn(
            verifier
                .ready()
                .await
                .unwrap()
                .call(Request::CommitDecoded(decoded, None)),
        );
        while joined.load(Ordering::SeqCst) == 0 {
            assert!(
                !submitted.is_finished(),
                "submission must join the in-flight check"
            );
            tokio::task::yield_now().await;
        }
        assert!(
            verifier.proposals.borrow().is_empty(),
            "an in-flight check is not valid evidence"
        );

        let mut cold = None;
        let mut evictions = Vec::new();
        if evict {
            let key = proposal_key_and_size(&proposal).0;
            assert!(verifier.in_flight_proposals.lookup(key).is_some());
            for suffix in 1..=8 {
                evictions.push(
                    verifier
                        .in_flight_proposals
                        .register([suffix; 32], &proposal.header)
                        .expect("distinct in-flight identities"),
                );
            }
            for entry in verifier.in_flight_proposals.0.lock().unwrap().iter_mut() {
                entry.eligible_at = Instant::now();
            }
            assert_eq!(verifier.in_flight_proposals.0.lock().unwrap().len(), 8);
            assert!(verifier.in_flight_proposals.lookup(key).is_none());
            while dispatched.load(Ordering::SeqCst) < transaction_count {
                tokio::task::yield_now().await;
            }
            cold = Some(tokio::spawn(verifier.ready().await.unwrap().call(
                Request::CommitDecoded(DecodedBlock::deserialize(&bytes).unwrap(), None),
            )));
            // The second submission cannot subscribe to the evicted identity; it dispatches
            // its own transaction checks while the first joiner is still waiting.
            while dispatched.load(Ordering::SeqCst) < transaction_count * 2 {
                assert!(
                    !cold.as_ref().unwrap().is_finished(),
                    "cold verification must start"
                );
                assert_eq!(joined.load(Ordering::SeqCst), 1);
                tokio::task::yield_now().await;
            }
            assert_eq!(joined.load(Ordering::SeqCst), 1);
        }
        if cancel {
            checking.abort();
            assert!((&mut checking).await.unwrap_err().is_cancelled());
        }
        release.send(true).unwrap();
        if fail {
            assert!((&mut checking).await.unwrap().is_err());
            assert!(submitted.await.unwrap().is_err());
            assert_eq!(calls.load(Ordering::SeqCst), transaction_count * 2);
            assert!(matches!(
                state
                    .clone()
                    .oneshot(zs::Request::KnownBlock(solved.hash()))
                    .await
                    .unwrap(),
                zs::Response::KnownBlock(None)
            ));
        } else if evict {
            assert_eq!((&mut checking).await.unwrap().unwrap(), proposal.hash());
            assert_eq!(submitted.await.unwrap().unwrap(), solved.hash());
            cold_release.send(true).unwrap();
            assert!(cold.unwrap().await.unwrap().is_err());
            assert_eq!(calls.load(Ordering::SeqCst), transaction_count * 2);
            assert_eq!(evictions.len(), 8);
        } else {
            if !cancel {
                assert_eq!((&mut checking).await.unwrap().unwrap(), proposal.hash());
            }
            assert_eq!(submitted.await.unwrap().unwrap(), solved.hash());
            assert_eq!(calls.load(Ordering::SeqCst), transaction_count);
        }
    })
    .await
    .expect("joining a proposal cannot stall the mined submission");
}

#[tokio::test(flavor = "multi_thread")]
async fn ordinary_and_early_relay_blocks_do_not_join_pending_proposals() {
    verify_without_join(Submission::Commit).await;
    verify_without_join(Submission::EarlyRelay).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn newly_started_proposal_does_not_delay_a_solved_block() {
    verify_without_join(Submission::YoungDecoded).await;
}

#[derive(Clone, Copy)]
enum Submission {
    Commit,
    EarlyRelay,
    YoungDecoded,
}

async fn verify_without_join(submission: Submission) {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let (network, state, proposal, solved) = regtest_proposal().await;
        let (release, released) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let transaction_count = solved.transactions.len();
        let transactions = tower::service_fn({
            let calls = calls.clone();
            move |request: tx::BlockRequest| {
                let mut released = released.clone();
                let call = calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    if call < transaction_count {
                        released.wait_for(|ready| *ready).await.unwrap();
                    }
                    Ok::<_, BoxError>(tx::BlockResponse {
                        tx_id: request.transaction.unmined_id(),
                        miner_fee: None,
                        sigops: 0,
                    })
                }
            }
        });
        let mut verifier = SemanticBlockVerifier::new(&network, state, transactions);
        let joins = verifier.in_flight_joins.clone();
        let checking = tokio::spawn(
            verifier
                .ready()
                .await
                .unwrap()
                .call(Request::CheckProposal(proposal.clone())),
        );
        while calls.load(Ordering::SeqCst) < transaction_count {
            assert!(!checking.is_finished(), "proposal checks are held");
            tokio::task::yield_now().await;
        }
        let key = proposal_key_and_size(&proposal).0;
        let eligible_at = match submission {
            Submission::YoungDecoded => Instant::now() + Duration::from_secs(60),
            Submission::Commit | Submission::EarlyRelay => Instant::now(),
        };
        verifier.in_flight_proposals.0.lock().unwrap()[0].eligible_at = eligible_at;
        assert_eq!(
            verifier.in_flight_proposals.lookup(key).is_some(),
            !matches!(submission, Submission::YoungDecoded)
        );
        let request = match submission {
            Submission::Commit => Request::Commit(solved.clone()),
            Submission::EarlyRelay => Request::CommitEarlyRelay(
                DecodedBlock::deserialize(&solved.zcash_serialize_to_vec().unwrap()).unwrap(),
            ),
            Submission::YoungDecoded => Request::CommitDecoded(
                DecodedBlock::deserialize(&solved.zcash_serialize_to_vec().unwrap()).unwrap(),
                None,
            ),
        };
        assert_eq!(
            verifier.ready().await.unwrap().call(request).await.unwrap(),
            solved.hash()
        );
        assert_eq!(joins.load(Ordering::SeqCst), 0);
        assert_eq!(calls.load(Ordering::SeqCst), transaction_count * 2);
        checking.abort();
        assert!(checking.await.unwrap_err().is_cancelled());
        drop(release);
    })
    .await
    .expect("cold verification must not wait for an in-flight proposal");
}
