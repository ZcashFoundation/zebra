//! A rejected candidate response cannot start coinbase-only template work after its read returns.

use super::*;

#[tokio::test]
async fn rejected_candidate_read_does_not_start_template_work() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(30), async {
        let network = Network::new_regtest(
            zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            }
            .into(),
        );
        let (state, read_state, tip, _tip_changes) = zebra_state::populated_state(
            [zebra_chain::block::genesis::regtest_genesis_block()],
            &network,
        )
        .await;
        let miner = MinerParams::from(
            Address::decode(
                &network,
                default_miner_address(network.kind(), &MinerAddressType::Transparent),
            )
            .unwrap(),
        );
        let info = fetch_chain_info(read_state.clone()).await.unwrap();
        let template = BlockTemplateResponse::from_mempool(
            &network,
            &CoinbaseCache::default(),
            &miner,
            &info,
            LongPollInput::new(
                info.tip_height,
                info.tip_hash,
                info.max_time,
                std::iter::empty(),
            )
            .generate_id(),
            vec![],
            Default::default(),
        )
        .unwrap();
        let mut block =
            proposal_block_from_template(&template, BlockTemplateTimeSource::CurTime, &network)
                .unwrap();
        let height = block.coinbase_height().unwrap();
        // The commitments authenticate this body, but its missing spend fails full validation.
        block.transactions.push(Arc::new(Transaction::test_v5(
            NetworkUpgrade::Nu5,
            vec![zebra_chain::transparent::Input::PrevOut {
                outpoint: zebra_chain::transparent::OutPoint {
                    hash: [8; 32].into(),
                    index: 0,
                },
                unlock_script: zebra_chain::transparent::Script::new(&[0x51]),
                sequence: u32::MAX,
            }],
            vec![zebra_chain::transparent::Output::new(
                1i64.try_into().unwrap(),
                zebra_chain::transparent::Script::new(&[0x51]),
            )],
            zebra_chain::transaction::LockTime::unlocked(),
            height,
        )));
        Arc::make_mut(&mut block.header).merkle_root =
            block.transactions.iter().map(|tx| tx.hash()).collect();
        Arc::make_mut(&mut block.header).commitment_bytes = <[u8; 32]>::from(
            block::ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &info
                    .chain_history_root
                    .unwrap_or(block::CHAIN_HISTORY_ACTIVATION_RESERVED.into()),
                &block.auth_data_root(),
            ),
        )
        .into();
        let block = Arc::new(block);
        let hash = block.hash();
        let ReadResponse::MiningCandidateChanges(mut candidates) = read_state
            .clone()
            .oneshot(ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("state provides mining subscriptions")
        };
        let zebra_state::Response::MiningCandidate(Some(guard)) = state
            .clone()
            .oneshot(zebra_state::Request::MiningCandidate(block.clone()))
            .await
            .unwrap()
        else {
            panic!("the subscription enables speculative work")
        };
        guard.body_bound();
        candidates.changed().await;
        assert!(candidates.is_current(hash));

        let read_started = Arc::new(tokio::sync::Notify::new());
        let fallback_started = Arc::new(tokio::sync::Notify::new());
        let (release, released) = watch::channel(false);
        let observed_state = tower::service_fn({
            let read_state = read_state.clone();
            let read_started = read_started.clone();
            let fallback_started = fallback_started.clone();
            move |request| {
                let read_state = read_state.clone();
                let read_started = read_started.clone();
                let fallback_started = fallback_started.clone();
                let mut released = released.clone();
                async move {
                    match request {
                        ReadRequest::MiningCandidateChanges => read_state.oneshot(request).await,
                        ReadRequest::MiningCandidate => {
                            let response = read_state.oneshot(request).await?;
                            assert!(matches!(&response, ReadResponse::MiningCandidate(Some(_))));
                            read_started.notify_one();
                            released.wait_for(|released| *released).await.unwrap();
                            Ok(response)
                        }
                        _ => {
                            // Stop ordinary tip work so it cannot hide a speculative cache write.
                            fallback_started.notify_one();
                            futures::future::pending().await
                        }
                    }
                }
            }
        });
        let mempool: zebra_test::mock_service::MockService<
            mempool::Request,
            mempool::Response,
            _,
            zebra_node_services::BoxError,
        > = zebra_test::mock_service::MockService::build().for_unit_tests();
        let verifier = tower::service_fn(|request: VerifyRequest| async move {
            Ok::<_, zebra_consensus::BoxError>(request.block().hash())
        });
        let coinbases = CoinbaseCache::default();
        let cache = TemplateCache::default();
        let (_events, events) = broadcast::channel(1);
        let mut updater = tokio::task::JoinSet::new();
        updater.spawn(run(
            network.clone(),
            miner,
            coinbases.clone(),
            cache.clone(),
            mempool,
            events,
            observed_state,
            tip,
            MockSyncStatus::default(),
            verifier,
        ));
        read_started.notified().await;
        assert_eq!(
            state
                .oneshot(zebra_state::Request::RejectBlock(block))
                .await
                .unwrap(),
            zebra_state::Response::Rejected(true),
        );
        assert!(!candidates.is_current(hash));
        release.send_replace(true);
        fallback_started.notified().await;
        let built = !coinbases.entries.lock().unwrap().transactions.is_empty();
        assert!(
            !built,
            "a detached rejected response must not start coinbase construction"
        );
        assert!(cache
            .template_for_tip(hash, &network, DateTime32::now())
            .is_none());
        updater.abort_all();
        while updater.join_next().await.is_some() {}
    })
    .await
    .expect("rejection must invalidate detached template context before work starts");
}
