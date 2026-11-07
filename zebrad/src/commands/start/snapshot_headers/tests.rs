//! Tests for headers-first sync before a state snapshot bootstrap.

use std::{sync::Arc, time::Duration};

use chrono::Duration as TimeDelta;
use tokio::sync::watch;
use tower::ServiceExt;

use zebra_chain::{
    block::{self, genesis::regtest_genesis_block, Height},
    chain_tip::ChainTip,
    parameters::Network,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};
use zebra_network as zn;
use zebra_state as zs;

use super::{commit_genesis, wait_for_header_tip, ReopenableChainTip, HEADER_STALL_TIMEOUT};
use crate::components::state_snapshot::bootstrap::{check_synced_headers, BootstrapError};

/// The genesis header, and `count` synthetic headers above it.
fn headers(count: u32) -> Vec<Arc<block::Header>> {
    let genesis = regtest_genesis_block().header.clone();
    let mut headers = vec![genesis.clone()];
    for height in 1..=count {
        let mut header = *headers[headers.len() - 1];
        header.previous_block_hash = header.hash();
        header.time = genesis.time + TimeDelta::seconds(75 * i64::from(height));
        headers.push(Arc::new(header));
    }
    headers
}

fn commitment(headers: &[Arc<block::Header>], height: u32) -> StateSnapshotCommitment {
    StateSnapshotCommitment {
        height: Height(height),
        block_hash: headers[height as usize].hash(),
        manifest: ObjectHash([1; 32]),
    }
}

/// The synced headers from the snapshot to the checkpoint replace the bootstrap's own header
/// fetch, and a snapshot block that isn't on them is rejected.
#[test]
fn synced_headers_check_the_snapshot_block() {
    let _init_guard = zebra_test::init();
    let chain = headers(20);
    let checkpoint = (Height(15), chain[15].hash());
    let synced = chain[10..=15].to_vec();

    assert_eq!(
        check_synced_headers(synced.clone(), commitment(&chain, 10), checkpoint).unwrap(),
        chain[11..=15].to_vec(),
        "the headers above the snapshot height"
    );

    let mut wrong = commitment(&chain, 10);
    wrong.block_hash = chain[9].hash();
    assert!(matches!(
        check_synced_headers(synced.clone(), wrong, checkpoint),
        Err(BootstrapError::Rejected(_))
    ));

    // Headers that stop short of the checkpoint aren't the snapshot's fault.
    assert!(matches!(
        check_synced_headers(chain[10..15].to_vec(), commitment(&chain, 10), checkpoint),
        Err(BootstrapError::Aborted(_))
    ));
    assert!(matches!(
        check_synced_headers(
            synced,
            commitment(&chain, 10),
            (Height(15), chain[14].hash())
        ),
        Err(BootstrapError::Aborted(_))
    ));
}

/// The node waits for its header chain to reach the checkpoint after the snapshot, and gives up
/// once the header chain stops growing.
#[tokio::test(start_paused = true)]
async fn waiting_for_headers_gives_up_when_they_stall() {
    let _init_guard = zebra_test::init();
    let (tip_sender, mut header_tip) = watch::channel(None);

    let waiter = tokio::spawn(async move {
        let result = wait_for_header_tip(&mut header_tip, Height(100)).await;
        (result, header_tip)
    });
    tokio::time::sleep(HEADER_STALL_TIMEOUT / 2).await;
    tip_sender.send_replace(Some(Height(50)));
    tokio::time::sleep(HEADER_STALL_TIMEOUT / 2).await;
    tip_sender.send_replace(Some(Height(100)));
    let (result, mut header_tip) = waiter.await.unwrap();
    assert!(result.is_ok(), "the tip kept growing to the target");

    let stalled = wait_for_header_tip(&mut header_tip, Height(200)).await;
    assert!(matches!(stalled, Err(BootstrapError::Aborted(_))));
}

/// On Regtest, the node commits the genesis block it has, without asking a peer.
#[tokio::test]
async fn regtest_genesis_commits_without_peers() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(Default::default());
    let (state, _read_state, latest_chain_tip, _chain_tip_change) =
        zs::init_test_services(&network).await;
    let peers = tower::service_fn(|_: zn::Request| async {
        Err::<zn::Response, crate::BoxError>("no peers".into())
    });

    commit_genesis(peers, state, &network).await.unwrap();
    assert_eq!(
        latest_chain_tip.best_tip_height_and_hash(),
        Some((Height(0), network.genesis_hash()))
    );
}

/// The network's chain tip follows the state the node reopens.
#[tokio::test]
async fn the_network_chain_tip_follows_a_reopened_state() {
    let _init_guard = zebra_test::init();
    let network = Network::new_regtest(Default::default());
    let (_closed_state, _read_state, closed_tip, _change) = zs::init_test_services(&network).await;
    let (reopened_state, _read_state, reopened_tip, _change) =
        zs::init_test_services(&network).await;

    let (switch, mut tip) = ReopenableChainTip::new(closed_tip, true);
    tip.mark_best_tip_seen();
    assert_eq!(tip.best_tip_height(), None);
    assert!(
        tip.is_at_or_near_network_tip(chrono::Utc::now()),
        "a node that syncs headers for a bootstrap doesn't need old blocks"
    );
    switch.end_bootstrap();
    assert!(!tip.is_at_or_near_network_tip(chrono::Utc::now()));

    reopened_state
        .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
            regtest_genesis_block().into(),
        ))
        .await
        .unwrap();
    switch.reopen(reopened_tip);

    tokio::time::timeout(Duration::from_secs(10), tip.best_tip_changed())
        .await
        .expect("reopening the state changes the tip")
        .unwrap();
    assert_eq!(
        tip.best_tip_height_and_hash(),
        Some((Height(0), network.genesis_hash()))
    );
}

/// The header-only state closes, so the bootstrap can replace it and the node can reopen it.
#[tokio::test(flavor = "multi_thread")]
async fn the_header_only_state_closes_and_reopens() {
    use crate::{commands::StartCmd, config::ZebradConfig};
    use tower::builder::ServiceBuilder;

    let _init_guard = zebra_test::init();
    let cache = tempfile::tempdir().unwrap();
    let mut config = ZebradConfig::default();
    config.network.network =
        Network::new_regtest(zebra_chain::parameters::testnet::RegtestParameters {
            activation_heights: zebra_chain::parameters::testnet::ConfiguredActivationHeights {
                nu5: Some(1),
                ..Default::default()
            },
            ..Default::default()
        });
    config.state.cache_dir = cache.path().to_path_buf();
    config.state.ephemeral = false;
    config.state.prune_blocks_older_than = Some(zs::MIN_PRUNING_RETENTION);
    let mode = zebra_chain::parameters::spentness_hints::Mode::Off;

    let (hints, state_service, read_state, latest_chain_tip, chain_tip_change) =
        StartCmd::open_state(&config, Height(0), mode, zs::MemoryLimits::default()).await;
    let state = ServiceBuilder::new().buffer(10).service(state_service);
    let peers = tower::service_fn(|_: zn::Request| async {
        Err::<zn::Response, crate::BoxError>("no peers".into())
    });
    commit_genesis(peers, state.clone(), &config.network.network)
        .await
        .unwrap();
    let (_switch, _network_tip) = ReopenableChainTip::new(latest_chain_tip.clone(), true);

    tokio::time::timeout(
        Duration::from_secs(30),
        StartCmd::close_state((hints, state, read_state, latest_chain_tip, chain_tip_change)),
    )
    .await
    .expect("the state closes");
    // The state's metrics task stops on its next poll, which the bootstrap's installation waits
    // for before it touches the database.
    tokio::time::sleep(Duration::from_secs(1)).await;

    let reopened = tokio::time::timeout(
        Duration::from_secs(30),
        StartCmd::open_state(&config, Height(0), mode, zs::MemoryLimits::default()),
    )
    .await
    .expect("the state reopens");
    assert_eq!(
        reopened.3.best_tip_height_and_hash(),
        Some((Height(0), config.network.network.genesis_hash()))
    );
}
