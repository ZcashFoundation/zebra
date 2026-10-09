//! Automatic startup waits for committed mining context without hiding caller errors.

use tracing::instrument::WithSubscriber;
use tracing_subscriber::{layer::SubscriberExt, Layer};
use zebra_chain::{
    block::{genesis::regtest_genesis_block, Block},
    chain_sync_status::MockSyncStatus,
    parameters::testnet::{ConfiguredCheckpoints, Parameters},
    serialization::ZcashDeserialize,
};
use zebra_network::address_book_peers::MockAddressBookPeers;
use zebra_rpc::methods::{GetInfoResponse, RpcImpl, RpcServer};

use super::*;

/// Each test owns the warning channel consumed by its RPC, not the process-wide logger.
struct WarningLog(watch::Sender<Option<(String, tracing::Level, chrono::DateTime<chrono::Utc>)>>);

impl<S: tracing::Subscriber> Layer<S> for WarningLog {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let level = *event.metadata().level();
        if level == tracing::Level::WARN || level == tracing::Level::ERROR {
            self.0
                .send_replace(Some((format!("{event:?}"), level, chrono::Utc::now())));
        }
    }
}

/// Poll the owner on its actual future/timer wakeups and the same tip notifications as the queue
/// checker. In particular, a waiting owner must resume when genesis is committed.
async fn drive_until<F: Future>(
    templates: &mut BlockTemplates,
    storage: Option<(&Storage, block::Hash)>,
    changes: &mut watch::Receiver<Option<(Height, block::Hash)>>,
    future: F,
) -> F::Output {
    tokio::pin!(future);
    loop {
        let _ = changes.borrow_and_update();
        let changed = changes.changed();
        tokio::pin!(changed);
        let result = futures::future::poll_fn(|cx| {
            assert!(matches!(templates.poll(cx, storage), Poll::Ready(Ok(()))));
            if let Poll::Ready(result) = future.as_mut().poll(cx) {
                return Poll::Ready(Some(result));
            }
            if let Poll::Ready(result) = changed.as_mut().poll(cx) {
                result.expect("the state keeps the mining subscription open");
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await;
        if let Some(result) = result {
            return result;
        }
    }
}

#[tokio::test]
async fn automatic_startup_waits_for_context_and_canopy_without_rpc_warnings() {
    let _init_guard = zebra_test::init();
    // The second network uses real checkpoint fixtures, with Canopy activating at the next
    // block after checkpoint sync. A genesis tip alone must not start unsupported template work.
    let delayed_canopy = Parameters::build()
        .with_genesis_hash(
            Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&0])
                .unwrap()
                .hash(),
        )
        .unwrap()
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(2),
            ..Default::default()
        })
        .unwrap()
        .with_halving_interval(800_000)
        .unwrap()
        .with_target_difficulty_limit(Network::Mainnet.target_difficulty_limit())
        .unwrap()
        .with_funding_streams(Vec::new())
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(
            (0..=1)
                .map(|height| {
                    (
                        Height(height),
                        Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&height])
                            .unwrap()
                            .hash(),
                    )
                })
                .collect(),
        ))
        .unwrap()
        .to_network()
        .unwrap();

    for network in [Network::new_regtest(Default::default()), delayed_canopy] {
        let (warnings, warning_rx) = watch::channel(None);
        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::WARN)
            .with(WarningLog(warnings));
        timeout(Duration::from_secs(30), async {
            let max_checkpoint = Height(if network.is_regtest() { 0 } else { 1 });
            let (state, read, tip, _) = zebra_state::init(
                zebra_state::Config::ephemeral(),
                &network,
                max_checkpoint,
                0,
            )
            .await;
            let state = Buffer::new(state, 1);
            let (verifier, _, background, cutoff) = zebra_consensus::router::init_test(
                zebra_consensus::Config::default(),
                &network,
                state.clone(),
            )
            .await;
            let address = default_miner_address(network.kind(), &MinerAddressType::Transparent);
            let miner_params = MinerParams::from(Address::decode(&network, address).unwrap());
            let ReadResponse::MiningTipChange(changes) = read
                .clone()
                .oneshot(ReadRequest::MiningTipChange)
                .await
                .unwrap()
            else {
                unreachable!("state returns a mining subscription")
            };
            let mut wakeups = changes.receiver.clone();
            let (mut templates, mut published, requests) = BlockTemplates::new(
                network.clone(),
                Some(miner_params.clone()),
                Buffer::new(BoxService::new(read.clone()), 1),
                Buffer::new(BoxService::new(verifier.clone()), 1),
                changes,
            );
            let mempool: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
            let (sync_status, _recent_syncs) =
                crate::components::sync::SyncStatus::new_for_network(&network);
            let (chain_metrics, _chain_metrics_rx) =
                crate::components::health::ChainTipMetrics::channel();
            let mut progress = Box::pin(crate::components::sync::show_block_chain_progress(
                network.clone(),
                tip.clone(),
                sync_status,
                chain_metrics,
            ));
            futures::future::poll_fn(|cx| {
                assert!(progress.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let (rpc, queue) = RpcImpl::new(
                network.clone(),
                zebra_rpc::config::mining::Config {
                    miner_address: Some(address.parse().unwrap()),
                    ..Default::default()
                },
                false,
                "0.0.1",
                "startup test",
                Buffer::new(mempool, 1),
                state.clone(),
                Buffer::new(read.clone(), 1),
                verifier.clone(),
                cutoff,
                MockSyncStatus::default(),
                tip,
                MockAddressBookPeers::default(),
                warning_rx,
                None,
            );
            let rpc = rpc.with_block_templates(published.clone(), requests.clone());
            let no_errors =
                serde_json::to_value(GetInfoResponse::default()).unwrap()["errors"].clone();
            let storage = Storage::new(&super::super::super::Config::default());
            // Regtest deliberately has no enabled transaction storage at startup.
            let snapshot = (!network.is_regtest()).then_some((&storage, network.genesis_hash()));
            drive_until(
                &mut templates,
                snapshot,
                &mut wakeups,
                sleep(Duration::from_millis(50)),
            )
            .await;
            assert!(published.borrow().is_none());
            assert_eq!(
                serde_json::to_value(rpc.get_info().await.unwrap()).unwrap()["errors"],
                no_errors
            );

            // A foreground request still receives the actual empty-state error promptly.
            let (response, result) = tokio::sync::oneshot::channel();
            requests
                .try_send(BlockTemplateRequest {
                    miner_params,
                    response,
                })
                .unwrap();
            assert!(drive_until(&mut templates, snapshot, &mut wakeups, result)
                .await
                .unwrap()
                .is_err());

            let genesis = if network.is_regtest() {
                regtest_genesis_block()
            } else {
                Arc::new(Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&0]).unwrap())
            };
            if !network.is_regtest() {
                state
                    .clone()
                    .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
                        genesis.clone().into(),
                    ))
                    .await
                    .unwrap();
                drive_until(
                    &mut templates,
                    snapshot,
                    &mut wakeups,
                    sleep(Duration::from_millis(50)),
                )
                .await;
                assert!(published.borrow().is_none());
                assert_eq!(
                    serde_json::to_value(rpc.get_info().await.unwrap()).unwrap()["errors"],
                    no_errors
                );
            }

            let publish = drive_until(&mut templates, snapshot, &mut wakeups, published.changed());
            let commit = async {
                let block = if network.is_regtest() {
                    genesis
                } else {
                    Arc::new(
                        Block::zcash_deserialize(zebra_test::vectors::MAINNET_BLOCKS[&1]).unwrap(),
                    )
                };
                state
                    .clone()
                    .oneshot(zebra_state::Request::CommitCheckpointVerifiedBlock(
                        block.into(),
                    ))
                    .await
                    .unwrap();
            };
            let (published_result, ()) = tokio::join!(publish, commit);
            published_result.unwrap();
            let template = published.borrow().clone().unwrap();
            assert_eq!(template.height(), max_checkpoint.next().unwrap().0);
            let proposal = proposal_block_from_template(&template, None, &network).unwrap();
            verifier
                .oneshot(zebra_consensus::Request::CheckProposal(Arc::new(proposal)))
                .await
                .unwrap();
            assert_eq!(
                serde_json::to_value(rpc.get_info().await.unwrap()).unwrap()["errors"],
                no_errors
            );
            if network.is_regtest() {
                rpc.get_block_template(None).await.unwrap();
            }
            queue.abort();
            background.state_checkpoint_verify_handle.abort();
        })
        .with_subscriber(subscriber)
        .await
        .expect("startup context and its first validated template must make progress");
    }
}
