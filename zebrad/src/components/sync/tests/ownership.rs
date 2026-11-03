//! Tests for sharing block download with the checkpoint sync engine.

#![allow(clippy::unwrap_in_result)]

use std::{sync::Arc, time::Duration};

use futures::FutureExt;
use tokio::{
    sync::{mpsc, watch},
    time::timeout,
};

use zebra_chain::{
    block::{self, Block, Height},
    chain_sync_status::ChainSyncStatus,
    chain_tip::mock::MockChainTip,
    serialization::ZcashDeserializeInto,
};
use zebra_consensus::Config as ConsensusConfig;
use zebra_network::InventoryResponse::Available;
use zebra_state::Config as StateConfig;
use zebra_test::mock_service::{MockService, PanicAssertion};

use zebra_network as zn;
use zebra_state as zs;

use crate::{
    components::{
        checkpoint_sync::{EngineConfig, LegacyOwnership, SyncOwner},
        sync::{self, Config},
        ChainSync,
    },
    config::ZebradConfig,
};

/// Maximum time to wait for a request to any test service. Time is paused.
const MAX_SERVICE_REQUEST_DELAY: Duration = Duration::from_millis(1000);

/// Longer than any wait in the syncer between a claim and its next request,
/// so a gate that does not block would be seen. Time is paused.
const GATE_OBSERVATION: Duration = Duration::from_secs(10 * 60);

/// The supervisor's ends of the ownership channels, as a test drives them.
struct Supervisor {
    owner: watch::Sender<SyncOwner>,
    ack: watch::Receiver<u64>,
    unit_lengths: mpsc::Sender<usize>,
}

/// Creates ownership channels like `checkpoint_sync::ownership_channels`,
/// with the owner starting at `owner`.
fn ownership_channels(owner: SyncOwner) -> (Supervisor, LegacyOwnership) {
    let (owner_sender, owner) = watch::channel(owner);
    let (ack_sender, ack) = watch::channel(0);
    let (unit_lengths_sender, unit_lengths) = mpsc::channel(16);

    (
        Supervisor {
            owner: owner_sender,
            ack,
            unit_lengths: unit_lengths_sender,
        },
        LegacyOwnership {
            owner,
            ack: ack_sender,
            unit_lengths,
        },
    )
}

fn engine(target: u32, generation: u64) -> SyncOwner {
    SyncOwner::Engine {
        target: Height(target),
        generation,
    }
}

type MockPeerSet = MockService<zn::Request, zn::Response, PanicAssertion>;
type MockState = MockService<zs::Request, zs::Response, PanicAssertion>;
type MockReadState = MockService<zs::ReadRequest, zs::ReadResponse, PanicAssertion>;
type MockVerifier = MockService<zebra_consensus::Request, block::Hash, PanicAssertion>;

/// A syncer on mock services that shares block download through `ownership`.
#[allow(clippy::type_complexity)]
fn syncer_with_ownership(
    ownership: LegacyOwnership,
) -> (
    ChainSync<MockPeerSet, MockState, MockReadState, MockVerifier, MockChainTip>,
    sync::SyncStatus,
    MockPeerSet,
    MockState,
    MockVerifier,
) {
    let config = ZebradConfig {
        consensus: ConsensusConfig::default(),
        state: StateConfig::ephemeral(),
        ..Default::default()
    };

    let peer_set = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let block_verifier_router = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let state_service = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let read_state_service = MockService::build()
        .with_max_request_delay(MAX_SERVICE_REQUEST_DELAY)
        .for_unit_tests();
    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
    let (misbehavior_tx, _misbehavior_rx) = mpsc::channel(1);

    let (chain_sync, sync_status) = ChainSync::new(
        &config,
        Height(0),
        peer_set.clone(),
        block_verifier_router.clone(),
        state_service.clone(),
        read_state_service,
        mock_chain_tip,
        misbehavior_tx,
    );

    (
        chain_sync.with_ownership(ownership),
        sync_status,
        peer_set,
        state_service,
        block_verifier_router,
    )
}

/// The `[sync]` defaults leave the engine off, and match the engine's own defaults.
#[test]
fn v2_checkpoint_sync_config_defaults() {
    let config = Config::default();

    assert!(!config.v2_checkpoint_sync, "the engine is opt-in");
    assert_eq!(config.v2_engine_config(), EngineConfig::default());
    assert_eq!(config.v2_lookahead_bytes, 512 * 1024 * 1024);
    assert_eq!(config.v2_min_peer_rate, 256 * 1024);
    assert_eq!(config.v2_target_peers, 8);
}

/// The `v2_*` fields map to the engine's parameters.
#[test]
fn v2_checkpoint_sync_config_maps_to_the_engine() {
    let config: Config = toml::from_str(
        r#"
        v2_checkpoint_sync = true
        v2_lookahead_bytes = 4194304
        v2_min_peer_rate = 1024
        v2_target_peers = 3
        "#,
    )
    .expect("the v2 fields are valid sync config");

    assert!(config.v2_checkpoint_sync);
    assert_eq!(
        config.v2_engine_config(),
        EngineConfig {
            lookahead_bytes: 4_194_304,
            min_peer_rate: 1024,
            target_peers: 3,
            ..EngineConfig::default()
        }
    );
    assert_eq!(
        config.download_concurrency_limit,
        Config::default().download_concurrency_limit,
        "unset fields keep their defaults"
    );
}

/// `v2_target_peers` and `v2_lookahead_bytes` values that would stall the
/// engine are raised to their floors.
#[test]
fn v2_checkpoint_sync_config_raises_low_values() {
    let _init_guard = zebra_test::init();

    let config: Config = toml::from_str(
        r#"
        v2_checkpoint_sync = true
        v2_lookahead_bytes = 0
        v2_min_peer_rate = 0
        v2_target_peers = 0
        "#,
    )
    .expect("the v2 fields are valid sync config");

    assert_eq!(
        config.v2_engine_config(),
        EngineConfig {
            lookahead_bytes: sync::MIN_V2_LOOKAHEAD_BYTES,
            min_peer_rate: 0,
            target_peers: sync::MIN_V2_TARGET_PEERS,
            ..EngineConfig::default()
        }
    );
    assert_eq!(sync::MIN_V2_TARGET_PEERS, 1);
    assert_eq!(sync::MIN_V2_LOOKAHEAD_BYTES, 2_000_000);
}

/// A syncer without the engine owns block download, and its gate never waits.
#[tokio::test(start_paused = true)]
async fn inert_ownership_never_blocks() {
    let _init_guard = zebra_test::init();

    let config = ZebradConfig {
        consensus: ConsensusConfig::default(),
        state: StateConfig::ephemeral(),
        ..Default::default()
    };
    let (mock_chain_tip, _mock_chain_tip_sender) = MockChainTip::new();
    let (misbehavior_tx, _misbehavior_rx) = mpsc::channel(1);
    let (mut chain_sync, _sync_status) = ChainSync::new(
        &config,
        Height(0),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        MockService::build().for_unit_tests(),
        mock_chain_tip,
        misbehavior_tx,
    );

    assert!(chain_sync.owns_download());
    chain_sync
        .wait_for_download_ownership()
        .now_or_never()
        .expect("the gate is open without the engine");
    assert!(chain_sync.owns_download());
}

/// While the engine owns block download, the gate blocks, acknowledges each
/// claim, and counts the engine's units as sync lengths. It opens when the
/// engine hands block download back.
#[tokio::test(start_paused = true)]
async fn ownership_gate_blocks_and_releases() {
    let _init_guard = zebra_test::init();

    let (mut supervisor, ownership) = ownership_channels(engine(4_000, 1));
    let (mut chain_sync, sync_status, mut peer_set, mut state_service, mut verifier) =
        syncer_with_ownership(ownership);

    assert!(!chain_sync.owns_download());
    assert!(
        !sync_status.is_close_to_tip(),
        "no sync lengths have been recorded yet"
    );

    {
        let wait = chain_sync.wait_for_download_ownership();
        tokio::pin!(wait);

        assert!(
            timeout(GATE_OBSERVATION, &mut wait).await.is_err(),
            "the gate blocks while the engine owns block download"
        );
        assert_eq!(
            *supervisor.ack.borrow_and_update(),
            1,
            "the claim is acknowledged"
        );

        // A small count, so that the pushed length is visible through the sync status.
        // The engine's real units are far longer than the close-to-tip threshold.
        supervisor
            .unit_lengths
            .send(1)
            .await
            .expect("the syncer holds the receiver");
        assert!(timeout(GATE_OBSERVATION, &mut wait).await.is_err());
        assert!(
            sync_status.is_close_to_tip(),
            "the engine's unit length is recorded while the gate waits"
        );

        // The engine hands back, and claims again before the syncer runs.
        supervisor.owner.send_replace(SyncOwner::Legacy);
        supervisor.owner.send_replace(engine(8_000, 2));
        assert!(
            timeout(GATE_OBSERVATION, &mut wait).await.is_err(),
            "a re-claim keeps the gate closed"
        );
        assert_eq!(
            *supervisor.ack.borrow_and_update(),
            2,
            "the re-claim is acknowledged"
        );

        supervisor.owner.send_replace(SyncOwner::Legacy);
        timeout(GATE_OBSERVATION, &mut wait)
            .await
            .expect("the gate opens when the engine hands block download back");
    }

    assert!(chain_sync.owns_download());

    // The gate itself never touches the network, the state, or the verifier.
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;
    verifier.expect_no_requests().await;
}

/// If the engine's supervisor stops without handing block download back, the
/// syncer takes it back.
#[tokio::test(start_paused = true)]
async fn ownership_gate_opens_when_the_supervisor_stops() {
    let _init_guard = zebra_test::init();

    let (supervisor, ownership) = ownership_channels(engine(4_000, 1));
    let (mut chain_sync, _sync_status, _peer_set, _state_service, _verifier) =
        syncer_with_ownership(ownership);

    {
        let wait = chain_sync.wait_for_download_ownership();
        tokio::pin!(wait);
        assert!(timeout(GATE_OBSERVATION, &mut wait).await.is_err());

        drop(supervisor);
        timeout(GATE_OBSERVATION, &mut wait)
            .await
            .expect("the gate opens when the supervisor is gone");
    }

    assert!(chain_sync.owns_download());
}

/// An engine claim in the middle of a sync round ends the round before the
/// next tip extension, queues no downloads for the hashes the round learns
/// after the claim, cancels the round's downloads, and is acknowledged. The
/// syncer starts no new round until the engine hands block download back.
#[tokio::test(start_paused = true)]
async fn engine_claim_mid_round_ends_the_round() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let (mut supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, mut block_verifier_router) =
        syncer_with_ownership(ownership);

    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();
    let block3_hash = zebra_test::vectors::BLOCK_MAINNET_3_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .hash();
    let block4_hash = zebra_test::vectors::BLOCK_MAINNET_4_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync.sync());

    // ChainSync::request_genesis: genesis is already in the state.
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips finds blocks 1 and 2.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![block1_hash, block2_hash]));
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }
    for hash in [block1_hash, block2_hash] {
        state_service
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
    }
    for block in [block1, block2] {
        peer_set
            .expect_request(zn::Request::BlockDownload {
                hash: block.hash(),
                advertiser: None,
            })
            .await
            .respond(zn::Response::Blocks(vec![Available((block, None))]));
    }
    for _ in 1..=2 {
        block_verifier_router
            .expect_request_that(|req| [block1_hash, block2_hash].contains(&req.block().hash()))
            .await
            .respond_with(|req| req.block().hash());
    }

    // The syncer is extending its tips when the engine claims block download.
    let extend = peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block1_hash],
            stop: None,
        })
        .await;
    supervisor.owner.send_replace(engine(4_000, 1));
    extend.respond(zn::Response::BlockHashes(vec![
        block2_hash,
        block3_hash,
        block4_hash,
    ]));
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    timeout(GATE_OBSERVATION, supervisor.ack.wait_for(|ack| *ack == 1))
        .await
        .expect("the syncer acknowledges the claim")
        .expect("the syncer holds the ack sender");

    // The hashes learned after the claim are not downloaded, and the round ended: the
    // syncer does not download blocks 3 or 4, extend its tips again, or start a new round.
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;
    block_verifier_router.expect_no_requests().await;

    // The engine hands back, and the syncer starts a new round.
    supervisor.owner.send_replace(SyncOwner::Legacy);
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// An engine claim during the restart delay between sync rounds is acknowledged
/// without waiting for the delay to end.
#[tokio::test(start_paused = true)]
async fn engine_claim_ends_the_restart_delay() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let (mut supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, _block_verifier_router) =
        syncer_with_ownership(ownership);

    let block0_hash = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync.sync());

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // The round finds nothing, and the syncer waits to restart.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));
    for _ in 0..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }
    state_service.expect_no_requests().await;
    peer_set.expect_no_requests().await;

    supervisor.owner.send_replace(engine(4_000, 1));
    timeout(
        Duration::from_secs(5),
        supervisor.ack.wait_for(|ack| *ack == 1),
    )
    .await
    .expect("the claim is acknowledged well before the restart delay ends")
    .expect("the syncer holds the ack sender");

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// An engine claim during the restart delay after a round that ran out of tips
/// cancels the downloads that round left in flight before it is acknowledged,
/// so none of them reach the verifier after the ack.
#[tokio::test(start_paused = true)]
async fn engine_claim_after_exhausted_tips_cancels_downloads() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let (mut supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, mut block_verifier_router) =
        syncer_with_ownership(ownership);

    let block0_hash = zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .hash();
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES.zcash_deserialize_into()?;
    let block1_hash = block1.hash();
    let block2: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_2_BYTES.zcash_deserialize_into()?;
    let block2_hash = block2.hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync.sync());

    // ChainSync::request_genesis: genesis is already in the state.
    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    // ChainSync::obtain_tips finds blocks 1 and 2.
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));
    peer_set
        .expect_request(zn::Request::FindBlocks {
            known_blocks: vec![block0_hash],
            stop: None,
        })
        .await
        .respond(zn::Response::BlockHashes(vec![block1_hash, block2_hash]));
    state_service
        .expect_request(zs::Request::KnownBlock(block1_hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    for _ in 0..(sync::FANOUT - 1) {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block0_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test obtain tips error")));
    }
    for hash in [block1_hash, block2_hash] {
        state_service
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
    }

    // The downloads reach the network, and stay unanswered.
    let mut held_downloads = Vec::new();
    for hash in [block1_hash, block2_hash] {
        held_downloads.push(
            peer_set
                .expect_request(zn::Request::BlockDownload {
                    hash,
                    advertiser: None,
                })
                .await,
        );
    }

    // ChainSync::extend_tips finds nothing, so the round runs out of tips and ends
    // with both downloads in flight.
    for _ in 0..sync::FANOUT {
        peer_set
            .expect_request(zn::Request::FindBlocks {
                known_blocks: vec![block1_hash],
                stop: None,
            })
            .await
            .respond(Err(zn::BoxError::from("synthetic test extend tips error")));
    }

    // Let the syncer reach the restart delay, well within the download timeout.
    tokio::time::sleep(Duration::from_secs(1)).await;

    supervisor.owner.send_replace(engine(4_000, 1));
    timeout(
        Duration::from_secs(5),
        supervisor.ack.wait_for(|ack| *ack == 1),
    )
    .await
    .expect("the claim is acknowledged well before the restart delay ends")
    .expect("the syncer holds the ack sender");

    // The peers answer after the ack, but the downloads were cancelled before it.
    for (download, block) in held_downloads.into_iter().zip([block1, block2]) {
        download.respond(zn::Response::Blocks(vec![Available((block, None))]));
    }
    block_verifier_router.expect_no_requests().await;
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// Starts `chain_sync` on a state that already holds genesis, and answers its
/// genesis check.
async fn sync_from_genesis(
    chain_sync: ChainSync<MockPeerSet, MockState, MockReadState, MockVerifier, MockChainTip>,
    state_service: &mut MockState,
) -> Result<
    (
        tokio::task::JoinHandle<Result<(), color_eyre::Report>>,
        block::Hash,
    ),
    crate::BoxError,
> {
    let block0: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES.zcash_deserialize_into()?;
    let block0_hash = block0.hash();

    let chain_sync_task_handle = tokio::spawn(chain_sync.sync());

    state_service
        .expect_request(zs::Request::KnownBlock(block0_hash))
        .await
        .respond(zs::Response::KnownBlock(Some(zs::KnownBlock::BestChain)));

    Ok((chain_sync_task_handle, block0_hash))
}

/// While spentness hints may apply, the syncer starts no sync round after
/// genesis until the hints decision is made, so it cannot commit block 1
/// before the checkpoint sync engine claims block download.
#[tokio::test(start_paused = true)]
async fn hints_gate_holds_the_syncer_after_genesis() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let (_supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, mut verifier) =
        syncer_with_ownership(ownership);
    let (decided, gate) = watch::channel(false);
    let chain_sync = chain_sync.with_hints_gate(gate, None);

    let (chain_sync_task_handle, block0_hash) =
        sync_from_genesis(chain_sync, &mut state_service).await?;

    // With `require`, the syncer waits as long as acquisition takes.
    tokio::time::sleep(GATE_OBSERVATION).await;
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;
    verifier.expect_no_requests().await;

    // The decision is made, and the syncer starts its first round.
    decided.send_replace(true);
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// An engine claim while the hints gate is closed is acknowledged at once, and
/// a hand-back before the hints decision still starts no sync round. A dropped
/// gate counts as decided.
#[tokio::test(start_paused = true)]
async fn hints_gate_acknowledges_engine_claims() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let (mut supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, _verifier) =
        syncer_with_ownership(ownership);
    let (decided, gate) = watch::channel(false);
    let chain_sync = chain_sync.with_hints_gate(gate, None);

    let (chain_sync_task_handle, block0_hash) =
        sync_from_genesis(chain_sync, &mut state_service).await?;

    supervisor.owner.send_replace(engine(4_000, 1));
    timeout(GATE_OBSERVATION, supervisor.ack.wait_for(|ack| *ack == 1))
        .await
        .expect("the syncer acknowledges the claim while the hints gate is closed")
        .expect("the syncer holds the ack sender");

    supervisor.owner.send_replace(SyncOwner::Legacy);
    tokio::time::sleep(GATE_OBSERVATION).await;
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;

    drop(decided);
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}

/// With a timeout (`auto`), the syncer syncs without hints once it passes,
/// even if the hints decision never comes.
#[tokio::test(start_paused = true)]
async fn hints_gate_falls_back_to_unhinted_sync() -> Result<(), crate::BoxError> {
    let _init_guard = zebra_test::init();

    let fallback = 2 * GATE_OBSERVATION;
    let (_supervisor, ownership) = ownership_channels(SyncOwner::Legacy);
    let (chain_sync, _sync_status, mut peer_set, mut state_service, _verifier) =
        syncer_with_ownership(ownership);
    let (_decided, gate) = watch::channel(false);
    let chain_sync = chain_sync.with_hints_gate(gate, Some(fallback));

    let (chain_sync_task_handle, block0_hash) =
        sync_from_genesis(chain_sync, &mut state_service).await?;

    tokio::time::sleep(GATE_OBSERVATION).await;
    peer_set.expect_no_requests().await;
    state_service.expect_no_requests().await;

    tokio::time::sleep(GATE_OBSERVATION).await;
    state_service
        .expect_request(zs::Request::BlockLocator)
        .await
        .respond(zs::Response::BlockLocator(vec![block0_hash]));

    let chain_sync_result = chain_sync_task_handle.now_or_never();
    assert!(
        chain_sync_result.is_none(),
        "unexpected error or panic in chain sync task: {chain_sync_result:?}",
    );

    Ok(())
}
