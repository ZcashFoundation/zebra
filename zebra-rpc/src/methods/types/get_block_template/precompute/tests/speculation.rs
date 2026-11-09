//! Speculation must not repeat rejected proofs or predict across a mempool reset.

use super::*;

/// Creates a live candidate with a transparent miner so scheduling checks do not need proofs.
async fn live_candidate() -> (
    Network,
    MinerParams,
    Arc<Block>,
    Arc<GetBlockTemplateChainInfo>,
    MiningCandidateChanges,
    (zebra_state::MiningCandidateGuard, impl Clone),
) {
    let network = Network::new_regtest(
        zebra_chain::parameters::testnet::ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let genesis = zebra_chain::block::genesis::regtest_genesis_block();
    let genesis_hash = genesis.hash();
    let (state, read_state, _tip, _changes) =
        zebra_state::populated_state([genesis], &network).await;
    let ReadResponse::MiningCandidateChanges(mut candidates) = read_state
        .clone()
        .oneshot(ReadRequest::MiningCandidateChanges)
        .await
        .unwrap()
    else {
        panic!("state returns the requested candidate subscription");
    };
    let miner = MinerParams::from(
        Address::decode(
            &network,
            default_miner_address(network.kind(), &MinerAddressType::Transparent),
        )
        .unwrap(),
    );
    let mempool = tower::service_fn(move |_: mempool::Request| async move {
        Ok::<_, zebra_node_services::BoxError>(mempool::Response::FullTransactions {
            transactions: vec![],
            transaction_dependencies: Default::default(),
            last_seen_tip_hash: genesis_hash,
        })
    });
    let (template, _) = build(
        &network,
        &miner,
        &CoinbaseCache::default(),
        read_state.clone(),
        Some(mempool),
        None,
        None,
    )
    .await
    .unwrap()
    .unwrap();
    let block = Arc::new(
        proposal_block_from_template(&template, BlockTemplateTimeSource::CurTime, &network)
            .unwrap(),
    );
    let zebra_state::Response::MiningCandidate(Some(guard)) = state
        .clone()
        .oneshot(zebra_state::Request::MiningCandidate(block.clone()))
        .await
        .unwrap()
    else {
        panic!("the subscription enables candidate publication");
    };
    candidates.changed().await;
    let info = mining_candidate(read_state).await.unwrap();
    (network, miner, block, info, candidates, (guard, state))
}

#[tokio::test]
async fn speculative_fees_skip_mempool_reset_boundaries() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let (_, miner, block, info, candidates, _guard) = live_candidate().await;
        let network = zebra_chain::parameters::testnet::Parameters::build()
            .with_activation_heights(
                zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                    nu7: Some(4_200_000),
                    ..Default::default()
                },
            )
            .unwrap()
            .clear_funding_streams()
            .to_network()
            .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mempool = tower::service_fn({
            let calls = calls.clone();
            move |_: mempool::Request| {
                calls.fetch_add(1, Ordering::Relaxed);
                async {
                    Err::<mempool::Response, zebra_node_services::BoxError>(
                        "unused snapshot".into(),
                    )
                }
            }
        });
        for height in [
            NetworkUpgrade::Nu7.activation_height(&network).unwrap(),
            network
                .temporary_orchard_disabling_soft_fork_height()
                .unwrap(),
        ] {
            let mut info = (*info).clone();
            info.tip_height = (height - 1).unwrap();
            precompute_fee_coinbase(
                network.clone(),
                miner.clone(),
                CoinbaseCache::default(),
                mempool.clone(),
                candidates.clone(),
                Arc::new(info),
                block.clone(),
            )
            .await;
            assert_eq!(
                calls.load(Ordering::Relaxed),
                0,
                "reset boundaries cannot use the old pool"
            );
        }
    })
    .await
    .expect("reset-boundary predictions must return without reading the pool");
}

#[tokio::test]
async fn speculative_fee_budget_survives_rollback_to_the_committed_parent() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(Duration::from_secs(20), async {
        let (network, miner, block, info, candidates, _guard) = live_candidate().await;
        let parent = block.header.previous_block_hash;
        let mempool = tower::service_fn(move |_: mempool::Request| async move {
            Ok::<_, zebra_node_services::BoxError>(mempool::Response::FullTransactions {
                transactions: vec![],
                transaction_dependencies: Default::default(),
                last_seen_tip_hash: parent,
            })
        });
        let coinbases = CoinbaseCache::default();
        precompute_fee_coinbase(
            network.clone(),
            miner.clone(),
            coinbases.clone(),
            mempool,
            candidates.clone(),
            info.clone(),
            block.clone(),
        )
        .await;
        assert!(coinbases.get(Height(2), Amount::zero(), None).is_some());
        // Rejection restores the committed parent's context, evicting the speculative coinbase.
        coinbases.select(Height(1), None);
        precompute_fee_coinbase(
            network,
            miner,
            coinbases.clone(),
            mempool,
            candidates,
            info,
            block,
        )
        .await;
        assert!(
            coinbases.get(Height(2), Amount::zero(), None).is_none(),
            "replaying a rejected candidate must not rebuild its evicted proof"
        );
    })
    .await
    .expect("rejected-candidate speculation must remain bounded");
}
