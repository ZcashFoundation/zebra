//! Proposal rejection revokes old work without revoking ordinary fee-only refreshes.

use zebra_node_services::mempool::MempoolChange;

use super::*;

#[tokio::test]
async fn rejected_served_generation_revokes_old_work_on_fallback_response() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::Mainnet;
        let height = NetworkUpgrade::Nu5.activation_height(&network).unwrap();
        let hash = Hash([1; 32]);
        let now = DateTime32::now();
        let chain_info = GetBlockTemplateChainInfo {
            expected_difficulty: CompactDifficulty::from(ExpandedDifficulty::from(U256::one())),
            tip_height: height,
            tip_hash: hash,
            cur_time: now,
            min_time: now,
            max_time: now.saturating_add(Duration32::from_minutes(90)),
            chain_history_root: fake_history_tree(&network).hash(),
            chain_value_pools: Default::default(),
        };
        let read_state = tower::service_fn(move |request| {
            std::future::ready(Ok::<_, BoxError>(match request {
                ReadRequest::Tip => ReadResponse::Tip(Some((height, hash))),
                ReadRequest::ChainInfo => ReadResponse::ChainInfo(chain_info.clone()),
                ReadRequest::MiningCandidate => ReadResponse::MiningCandidate(None),
                ReadRequest::MiningCandidateChanges => {
                    ReadResponse::MiningCandidateChanges(Default::default())
                }
                other => panic!("unexpected state request: {other:?}"),
            }))
        });
        let transaction = |lock_height| {
            VerifiedUnminedTx::new(
                Arc::new(Transaction::test_v1(
                    vec![],
                    vec![],
                    transaction::LockTime::Height(Height(lock_height)),
                ))
                .into(),
                10_000.try_into().unwrap(),
                0,
                0,
                Arc::new(vec![]),
            )
            .unwrap()
        };
        let first = transaction(1);
        let second = transaction(2);
        let (pool, pool_rx) = watch::channel(vec![first.clone()]);
        let (snapshots_allowed, snapshot_rx) = watch::channel(true);
        let mempool = tower::service_fn(move |request| {
            let mut allowed = snapshot_rx.clone();
            let pool = pool_rx.clone();
            async move {
                if matches!(request, mempool::Request::FullTransactions) {
                    allowed.wait_for(|allowed| *allowed).await.unwrap();
                }
                Ok::<_, BoxError>(match request {
                    mempool::Request::FullTransactions => mempool::Response::FullTransactions {
                        transactions: pool.borrow().clone(),
                        transaction_dependencies: Default::default(),
                        last_seen_tip_hash: hash,
                    },
                    mempool::Request::TransactionIds => mempool::Response::TransactionIds(
                        pool.borrow().iter().map(|tx| tx.transaction.id).collect(),
                    ),
                    other => panic!("unexpected mempool request: {other:?}"),
                })
            }
        });
        let mut verifier: MockService<zebra_consensus::Request, Hash, _, BoxError> =
            MockService::build()
                .with_max_request_delay(Duration::from_secs(30))
                .for_unit_tests();
        let (tip, tip_sender) = MockChainTip::new();
        tip_sender.send_best_tip_height(height);
        tip_sender.send_best_tip_hash(hash);
        tip_sender.send_best_tip_block_time(chrono::Utc::now());
        let mut sync = MockSyncStatus::default();
        sync.set_is_close_to_tip(true);
        let rpc = template_regression_rpc_with_verifier(
            network,
            mempool,
            read_state,
            tip,
            sync,
            verifier.clone(),
        );
        let original_a = rpc
            .get_block_template(None)
            .await
            .unwrap()
            .try_into_template()
            .unwrap();
        assert_eq!(original_a.transactions.len(), 1);
        let late_check_a = verifier
            .expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            })
            .await;

        // The fallback check stays pending while the updater independently accepts A.
        snapshots_allowed.send_replace(false);
        let (events, _) = broadcast::channel(8);
        let _updater = tokio_util::task::AbortOnDropHandle::new(
            rpc.spawn_block_template_updater(&MempoolTxSubscriber::new(events.clone()))
                .unwrap(),
        );
        let checked_a = verifier
            .expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            })
            .await;
        assert_eq!(checked_a.request().block(), late_check_a.request().block());
        let accepted_a_hash = checked_a.request().block().hash();
        checked_a.respond(accepted_a_hash);
        let second_id = second.transaction.id;
        pool.send_replace(vec![first.clone(), second]);
        events
            .send(MempoolChange::added([second_id].into_iter().collect()))
            .unwrap();
        snapshots_allowed.send_replace(true);
        let check_b = verifier
            .expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            })
            .await;
        let failed_b = rpc
            .get_block_template(None)
            .await
            .unwrap()
            .try_into_template()
            .unwrap();
        assert_eq!(failed_b.transactions.len(), 2);
        let waiting_b = rpc.get_block_template(Some(GetBlockTemplateParameters {
            long_poll_id: Some(failed_b.long_poll_id),
            ..Default::default()
        }));
        tokio::pin!(waiting_b);
        assert!(futures::poll!(&mut waiting_b).is_pending());
        snapshots_allowed.send_replace(false);
        check_b.respond_error("rejected newer served proposal".into());
        let restored_a = waiting_b.await.unwrap().try_into_template().unwrap();
        assert_eq!(restored_a.transactions, original_a.transactions);
        assert_eq!(restored_a.previous_block_hash, failed_b.previous_block_hash);
        assert_eq!(restored_a.max_time, failed_b.max_time);
        assert_eq!(restored_a.submit_old, Some(false));

        // A's older, independent check must revoke work issued after B's rejection too.
        let waiting_a = rpc.get_block_template(Some(GetBlockTemplateParameters {
            long_poll_id: Some(restored_a.long_poll_id),
            ..Default::default()
        }));
        tokio::pin!(waiting_a);
        assert!(futures::poll!(&mut waiting_a).is_pending());
        pool.send_replace(vec![]);
        snapshots_allowed.send_replace(true);
        late_check_a.respond_error("restored older proposal also rejected".into());
        let replacement = waiting_a.await.unwrap().try_into_template().unwrap();
        assert!(replacement.transactions.is_empty());
        assert_eq!(
            replacement.previous_block_hash,
            restored_a.previous_block_hash
        );
        assert_eq!(replacement.max_time, restored_a.max_time);
        assert_eq!(replacement.submit_old, Some(false));

        let added_id = first.transaction.id;
        pool.send_replace(vec![first]);
        events
            .send(MempoolChange::added([added_id].into_iter().collect()))
            .unwrap();
        let next_check = verifier
            .expect_request_that(|request| {
                matches!(request, zebra_consensus::Request::CheckProposal(_))
            })
            .await;
        let checked_hash = next_check.request().block().hash();
        next_check.respond(checked_hash);
        let refreshed = rpc
            .get_block_template(Some(GetBlockTemplateParameters {
                long_poll_id: Some(replacement.long_poll_id),
                ..Default::default()
            }))
            .await
            .unwrap()
            .try_into_template()
            .unwrap();
        assert!(refreshed
            .transactions
            .iter()
            .any(|tx| tx.hash == added_id.mined_id()));
        assert_eq!(refreshed.submit_old, Some(true));
    })
    .await
    .expect("proposal rejection and a subsequent ordinary refresh must complete");
}
