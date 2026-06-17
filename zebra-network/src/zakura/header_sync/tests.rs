use super::*;
use super::{config::*, error::*, events::*, reactor::*, validation::*, wire::*};
use crate::zakura::testkit::TraceCapture;
use chrono::Duration;
use metrics::{
    Counter, CounterFn, Gauge, Histogram, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};
use std::{
    collections::BTreeMap,
    sync::{Mutex, OnceLock},
};
use zebra_chain::{
    parameters::{
        testnet::{
            ConfiguredActivationHeights, ConfiguredCheckpoints, Parameters, RegtestParameters,
        },
        Network,
    },
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    work::{difficulty::CompactDifficulty, equihash::Solution},
};
use zebra_test::vectors::{
    BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES, BLOCK_MAINNET_3_BYTES, BLOCK_MAINNET_4_BYTES,
    BLOCK_MAINNET_GENESIS_BYTES, BLOCK_TESTNET_GENESIS_BYTES,
};

#[derive(Default)]
struct HeaderSyncMetricsRecorder {
    counters: Mutex<BTreeMap<String, u64>>,
}

struct RecordedCounter {
    name: String,
    recorder: &'static HeaderSyncMetricsRecorder,
}

impl CounterFn for RecordedCounter {
    fn increment(&self, value: u64) {
        let mut counters = self.recorder.counters.lock().expect("metrics mutex ok");
        let counter = counters.entry(self.name.clone()).or_default();
        *counter = counter.saturating_add(value);
    }

    fn absolute(&self, value: u64) {
        let mut counters = self.recorder.counters.lock().expect("metrics mutex ok");
        counters.insert(self.name.clone(), value);
    }
}

impl Recorder for HeaderSyncMetricsRecorder {
    fn describe_counter(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_gauge(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn describe_histogram(&self, _key: KeyName, _unit: Option<Unit>, _description: SharedString) {}

    fn register_counter(&self, key: &Key, _metadata: &Metadata<'_>) -> Counter {
        Counter::from_arc(Arc::new(RecordedCounter {
            name: key.name().to_string(),
            recorder: header_sync_metrics_recorder(),
        }))
    }

    fn register_gauge(&self, _key: &Key, _metadata: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, _key: &Key, _metadata: &Metadata<'_>) -> Histogram {
        Histogram::noop()
    }
}

fn header_sync_metrics_recorder() -> &'static HeaderSyncMetricsRecorder {
    static RECORDER: OnceLock<HeaderSyncMetricsRecorder> = OnceLock::new();
    let recorder = RECORDER.get_or_init(HeaderSyncMetricsRecorder::default);
    let _ = metrics::set_global_recorder(recorder);
    recorder
}

fn metric_value(name: &str) -> u64 {
    let recorder = header_sync_metrics_recorder();
    recorder
        .counters
        .lock()
        .expect("metrics mutex ok")
        .get(name)
        .copied()
        .unwrap_or_default()
}

fn metric_snapshot(names: &[&'static str]) -> BTreeMap<&'static str, u64> {
    names
        .iter()
        .copied()
        .map(|name| (name, metric_value(name)))
        .collect()
}

fn assert_metric_incremented(snapshot: &BTreeMap<&'static str, u64>, name: &'static str) {
    assert!(
        metric_value(name) > snapshot.get(name).copied().unwrap_or_default(),
        "expected metric {name} to increment"
    );
}

fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
    Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
}

fn mainnet_header(bytes: &[u8]) -> Arc<block::Header> {
    mainnet_block(bytes).header.clone()
}

async fn validate_headers_stateless_after_equihash_acceptance(
    headers: Vec<Arc<block::Header>>,
    context: HeaderSyncValidationContext<'_>,
) -> Result<(), HeaderSyncWireError> {
    validate_header_count(headers.len(), context.decode_context)?;
    validate_internal_continuity(&headers)?;
    validate_header_times(&headers, context.now, context.start_height)?;
    validate_solution_sizes(&headers, context.network)?;
    tokio::task::spawn_blocking(move || {
        for header in headers {
            let hash = block::Hash::from(header.as_ref());
            validate_difficulty_filter(hash, header.difficulty_threshold)?;
        }
        Ok(())
    })
    .await?
}

fn headers_context(count: u32, peer_cap: u32) -> HeaderSyncDecodeContext {
    HeaderSyncDecodeContext::for_headers_response(
        HeaderSyncRequestContract::new(block::Height(1), count).unwrap(),
        peer_cap,
    )
}

struct ReactorFixture {
    handle: HeaderSyncHandle,
    actions: mpsc::Receiver<HeaderSyncAction>,
    task: JoinHandle<()>,
}

impl Drop for ReactorFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn peer(byte: u8) -> ZakuraPeerId {
    ZakuraPeerId::new(vec![byte; 32]).expect("test peer id is within bounds")
}

fn regtest_network() -> Network {
    Network::new_regtest(Default::default())
}

fn checkpoint_testnet_with_hash(
    checkpoint_height: block::Height,
    checkpoint_hash: block::Hash,
) -> (Network, block::Hash) {
    let mainnet = Network::Mainnet;
    let network = Parameters::build()
        .with_network_name("HeadersyncCheckpointTest")
        .expect("custom network name is valid")
        .with_genesis_hash(mainnet.genesis_hash())
        .expect("mainnet genesis hash is valid")
        .with_activation_heights(ConfiguredActivationHeights {
            overwinter: Some(1),
            sapling: Some(2),
            blossom: Some(3),
            heartwood: Some(4),
            canopy: Some(4),
            ..Default::default()
        })
        .expect("custom activation heights are in order")
        .clear_funding_streams()
        .with_checkpoints(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (block::Height(0), mainnet.genesis_hash()),
            (checkpoint_height, checkpoint_hash),
        ]))
        .expect("custom checkpoints are valid")
        .to_network()
        .expect("custom testnet parameters are valid");

    (network, checkpoint_hash)
}

fn checkpoint_regtest(checkpoint_height: block::Height) -> (Network, block::Hash) {
    let checkpoint_hash = block::Hash::from(mainnet_header(&BLOCK_MAINNET_1_BYTES).as_ref());
    checkpoint_regtest_with_hash(checkpoint_height, checkpoint_hash)
}

fn checkpoint_regtest_with_hash(
    checkpoint_height: block::Height,
    checkpoint_hash: block::Hash,
) -> (Network, block::Hash) {
    let default_regtest = regtest_network();
    let params = RegtestParameters {
        checkpoints: Some(ConfiguredCheckpoints::HeightsAndHashes(vec![
            (block::Height(0), default_regtest.genesis_hash()),
            (checkpoint_height, checkpoint_hash),
        ])),
        ..Default::default()
    };

    (Network::new_regtest(params), checkpoint_hash)
}

fn startup_for(
    network: Network,
    anchor: (block::Height, block::Hash),
    best_header_tip: Option<(block::Height, block::Hash)>,
) -> HeaderSyncStartup {
    let mut startup = HeaderSyncStartup::new(
        network,
        anchor,
        HeaderSyncFrontiers {
            finalized_height: anchor.0,
            verified_block_tip: anchor.0,
        },
        best_header_tip,
        ZakuraHeaderSyncConfig::default(),
        LOCAL_MAX_MESSAGE_BYTES,
    );
    startup.range_state_actions_enabled = true;
    startup.inbound_new_block_acceptance_enabled = true;
    startup
}

#[test]
fn startup_new_is_passive_until_local_hooks_are_wired() {
    let network = Network::Mainnet;
    let anchor = (block::Height(0), network.genesis_hash());
    let startup = HeaderSyncStartup::new(
        network,
        anchor,
        HeaderSyncFrontiers {
            finalized_height: anchor.0,
            verified_block_tip: anchor.0,
        },
        Some(anchor),
        ZakuraHeaderSyncConfig::default(),
        LOCAL_MAX_MESSAGE_BYTES,
    );

    assert!(!startup.range_state_actions_enabled);
    assert!(!startup.inbound_new_block_acceptance_enabled);
}

fn startup_with_timeout(
    network: Network,
    anchor: (block::Height, block::Hash),
    request_timeout: std::time::Duration,
) -> HeaderSyncStartup {
    let mut startup = startup_for(network, anchor, None);
    startup.request_timeout = request_timeout;
    startup
}

fn spawn_test_reactor(startup: HeaderSyncStartup) -> ReactorFixture {
    let (handle, actions, task) = spawn_header_sync_reactor(startup).unwrap();
    ReactorFixture {
        handle,
        actions,
        task,
    }
}

async fn next_action(actions: &mut mpsc::Receiver<HeaderSyncAction>) -> HeaderSyncAction {
    tokio::time::timeout(std::time::Duration::from_secs(5), actions.recv())
        .await
        .expect("action arrives before timeout")
        .expect("reactor action channel stays open")
}

async fn next_non_query_action(actions: &mut mpsc::Receiver<HeaderSyncAction>) -> HeaderSyncAction {
    loop {
        let action = next_action(actions).await;
        if !matches!(
            action,
            HeaderSyncAction::QueryBestHeaderTip
                | HeaderSyncAction::QueryMissingBlockBodies { .. }
                | HeaderSyncAction::QueryHeadersByHeightRange { .. }
        ) {
            return action;
        }
    }
}

async fn next_query_headers_action(
    actions: &mut mpsc::Receiver<HeaderSyncAction>,
) -> HeaderSyncAction {
    loop {
        let action = next_action(actions).await;
        if matches!(action, HeaderSyncAction::QueryHeadersByHeightRange { .. }) {
            return action;
        }
    }
}

async fn assert_no_commit_or_misbehavior(actions: &mut mpsc::Receiver<HeaderSyncAction>) {
    while let Ok(Some(action)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), actions.recv()).await
    {
        assert!(
            !matches!(
                action,
                HeaderSyncAction::CommitHeaderRange { .. } | HeaderSyncAction::Misbehavior { .. }
            ),
            "unexpected commit or misbehavior action: {action:?}"
        );
    }
}

async fn connect_peer(fixture: &ReactorFixture, peer_id: ZakuraPeerId) {
    fixture
        .handle
        .send(HeaderSyncEvent::PeerConnected(peer_id))
        .await
        .unwrap();
}

async fn advertise_tip(
    fixture: &ReactorFixture,
    peer_id: ZakuraPeerId,
    anchor_height: block::Height,
    tip_height: block::Height,
    max_headers_per_response: u32,
    max_inflight_requests: u16,
) {
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id,
            msg: HeaderSyncMessage::Status(HeaderSyncStatus {
                tip_height,
                tip_hash: block::Hash([9; 32]),
                anchor_height,
                max_headers_per_response,
                max_inflight_requests,
            }),
        })
        .await
        .unwrap();
}

#[test]
fn codec_round_trips_status() {
    let status = HeaderSyncStatus {
        tip_height: block::Height(10),
        tip_hash: block::Hash([9; 32]),
        anchor_height: block::Height(1),
        max_headers_per_response: DEFAULT_HS_RANGE,
        max_inflight_requests: DEFAULT_HS_MAX_INFLIGHT,
    };
    let message = HeaderSyncMessage::Status(status);

    let encoded = message.encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn codec_round_trips_get_headers() {
    let message = HeaderSyncMessage::GetHeaders {
        start_height: block::Height(42),
        count: DEFAULT_HS_RANGE,
    };

    let encoded = message.encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn codec_round_trips_headers_with_bounded_vector() {
    let headers = vec![mainnet_header(&BLOCK_MAINNET_1_BYTES)];
    let message = HeaderSyncMessage::Headers(headers);

    let encoded = message.encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, headers_context(1, 1)).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn codec_round_trips_new_block() {
    let message = HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_1_BYTES));

    let encoded = message.encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()).unwrap();

    assert_eq!(decoded, message);
}

#[test]
fn codec_rejects_unknown_message_types_and_trailing_bytes() {
    assert!(matches!(
        HeaderSyncMessage::decode(&[99], HeaderSyncDecodeContext::control()),
        Err(HeaderSyncWireError::UnknownMessageType(99))
    ));

    let mut encoded = HeaderSyncMessage::GetHeaders {
        start_height: block::Height(1),
        count: 1,
    }
    .encode()
    .unwrap();
    encoded.push(0);

    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()),
        Err(HeaderSyncWireError::TrailingBytes)
    ));
}

#[test]
fn frame_decode_rejects_oversized_payload_length_before_allocating() {
    let mut bytes = Vec::new();
    bytes
        .write_u16::<LittleEndian>(u16::from(MSG_HS_STATUS))
        .unwrap();
    bytes.write_u16::<LittleEndian>(0).unwrap();
    bytes
        .write_u32::<LittleEndian>(MAX_HS_MESSAGE_BYTES as u32 + 1)
        .unwrap();

    assert!(Frame::decode(&bytes, MAX_HS_MESSAGE_BYTES as u32).is_err());
}

#[test]
fn decode_rejects_header_counts_over_contract_caps() {
    let mut encoded = vec![MSG_HS_HEADERS];
    encoded.write_u32::<LittleEndian>(MAX_HS_RANGE + 1).unwrap();
    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, headers_context(MAX_HS_RANGE, MAX_HS_RANGE)),
        Err(HeaderSyncWireError::HeaderCountLimit { .. })
    ));

    let mut encoded = vec![MSG_HS_HEADERS];
    encoded.write_u32::<LittleEndian>(2).unwrap();
    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, headers_context(1, MAX_HS_RANGE)),
        Err(HeaderSyncWireError::HeaderCountLimit { actual: 2, max: 1 })
    ));

    let mut encoded = vec![MSG_HS_HEADERS];
    encoded.write_u32::<LittleEndian>(2).unwrap();
    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, headers_context(MAX_HS_RANGE, 1)),
        Err(HeaderSyncWireError::HeaderCountLimit { actual: 2, max: 1 })
    ));
}

#[test]
fn headers_codec_does_not_use_legacy_160_header_cap() {
    let header = mainnet_header(&BLOCK_MAINNET_1_BYTES);
    let headers = vec![header; 161];
    let message = HeaderSyncMessage::Headers(headers);

    let encoded = message.encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, headers_context(161, 161)).unwrap();

    match decoded {
        HeaderSyncMessage::Headers(headers) => assert_eq!(headers.len(), 161),
        _ => panic!("decoded message must be Headers"),
    }
}

#[test]
fn get_headers_rejects_invalid_counts() {
    assert!(HeaderSyncMessage::GetHeaders {
        start_height: block::Height(1),
        count: 0,
    }
    .encode()
    .is_err());

    assert!(HeaderSyncMessage::GetHeaders {
        start_height: block::Height(1),
        count: MAX_HS_RANGE + 1,
    }
    .encode()
    .is_err());
}

#[test]
fn advertised_defaults_and_clamping_match_design() {
    let config = ZakuraHeaderSyncConfig::default();
    assert_eq!(config.max_headers_per_response, DEFAULT_HS_RANGE);
    assert_eq!(config.max_inflight_requests, DEFAULT_HS_MAX_INFLIGHT);
    assert_eq!(
        ZakuraHeaderSyncConfig {
            max_inflight_requests: u16::MAX,
            ..ZakuraHeaderSyncConfig::default()
        }
        .advertised_max_inflight_requests(),
        LOCAL_MAX_HS_INFLIGHT_PER_PEER
    );

    let status = HeaderSyncStatus {
        max_headers_per_response: MAX_HS_RANGE + 10,
        ..HeaderSyncStatus::default()
    };
    let encoded = HeaderSyncMessage::Status(status).encode().unwrap();
    let decoded = HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()).unwrap();
    match decoded {
        HeaderSyncMessage::Status(status) => {
            assert_eq!(status.max_headers_per_response, MAX_HS_RANGE);
        }
        _ => panic!("decoded message must be Status"),
    }
}

#[test]
fn header_serialized_sizes_are_exact_and_message_cap_has_headroom() {
    let mainnet = mainnet_header(&BLOCK_MAINNET_GENESIS_BYTES);
    let mut mainnet_bytes = Vec::new();
    mainnet.zcash_serialize(&mut mainnet_bytes).unwrap();
    assert_eq!(mainnet_bytes.len(), COMMON_HEADER_BYTES);

    let testnet = mainnet_header(&BLOCK_TESTNET_GENESIS_BYTES);
    let mut testnet_bytes = Vec::new();
    testnet.zcash_serialize(&mut testnet_bytes).unwrap();
    assert_eq!(testnet_bytes.len(), COMMON_HEADER_BYTES);

    let mut regtest = *mainnet;
    regtest.solution = Solution::Regtest([0; 36]);
    let mut regtest_bytes = Vec::new();
    regtest.zcash_serialize(&mut regtest_bytes).unwrap();
    assert_eq!(regtest_bytes.len(), REGTEST_HEADER_BYTES);

    let default_response_bytes = HEADER_SYNC_MESSAGE_TYPE_BYTES
        + HEADER_SYNC_COUNT_BYTES
        + COMMON_HEADER_BYTES * DEFAULT_HS_RANGE as usize;
    assert!(default_response_bytes < MAX_HS_MESSAGE_BYTES);
    assert!(MAX_HS_MESSAGE_BYTES < LOCAL_MAX_MESSAGE_BYTES as usize);
}

#[test]
fn request_and_serving_counts_are_clamped_by_byte_budget() {
    let count = clamp_header_sync_request_count(
        MAX_HS_RANGE,
        MAX_HS_RANGE,
        &Network::Mainnet,
        LOCAL_MAX_MESSAGE_BYTES,
    );

    assert!(count < MAX_HS_RANGE);
    let headers =
        vec![mainnet_header(&BLOCK_MAINNET_1_BYTES); usize::try_from(count).unwrap() + 100];
    let headers =
        truncate_headers_to_byte_budget(headers, &Network::Mainnet, LOCAL_MAX_MESSAGE_BYTES);
    let encoded = HeaderSyncMessage::Headers(headers).encode().unwrap();

    assert!(encoded.len() <= MAX_HS_MESSAGE_BYTES);
    assert!(encoded.len() + FRAME_HEADER_BYTES <= LOCAL_MAX_MESSAGE_BYTES as usize);
}

#[tokio::test(flavor = "current_thread")]
async fn reactor_starts_from_storage_frontiers_and_publishes_watch() {
    let network = regtest_network();
    let best = (block::Height(7), block::Hash([7; 32]));
    let startup = HeaderSyncStartup::new(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        HeaderSyncFrontiers {
            finalized_height: block::Height(2),
            verified_block_tip: block::Height(5),
        },
        Some(best),
        ZakuraHeaderSyncConfig::default(),
        LOCAL_MAX_MESSAGE_BYTES,
    );
    let fixture = spawn_test_reactor(startup);

    assert_eq!(fixture.handle.best_header_tip(), best);
    assert_eq!(*fixture.handle.subscribe_tip().borrow(), best);
}

#[tokio::test(flavor = "current_thread")]
async fn restart_rebuilds_schedule_from_durable_best_tip_and_peer_status() {
    let network = regtest_network();
    let best = (block::Height(4), block::Hash([4; 32]));
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some(best),
    ));
    let peer_id = peer(41);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id,
        block::Height(0),
        block::Height(8),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;

    loop {
        if let HeaderSyncAction::SendMessage {
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
            ..
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(start_height, block::Height(5));
            assert_eq!(count, 4);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn handle_sends_events_and_peer_connect_sends_status_first() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(1);

    connect_peer(&fixture, peer_id.clone()).await;

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::SendMessage { peer, msg } => {
            assert_eq!(peer, peer_id);
            assert!(matches!(msg, HeaderSyncMessage::Status(_)));
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn status_updates_peer_caps_and_scheduler_respects_them() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(2);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(10),
        2,
        u16::MAX,
    )
    .await;

    let mut saw_get_headers = false;
    for _ in 0..4 {
        if let HeaderSyncAction::SendMessage {
            peer,
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, peer_id);
            assert_eq!(start_height, block::Height(1));
            assert_eq!(count, 2);
            saw_get_headers = true;
            break;
        }
    }
    assert!(saw_get_headers);
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_limits_v1_to_one_outstanding_request_per_peer() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(31);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(20),
        2,
        u16::MAX,
    )
    .await;

    let mut get_headers_count = 0;
    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.actions.recv(),
    )
    .await
    {
        if matches!(
            action,
            HeaderSyncAction::SendMessage {
                peer,
                msg: HeaderSyncMessage::GetHeaders { .. },
            } if peer == peer_id
        ) {
            get_headers_count += 1;
        }
    }

    assert_eq!(get_headers_count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_fans_out_same_forward_range_to_three_peers() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peers = [peer(3), peer(4), peer(5)];

    for peer_id in peers.clone() {
        connect_peer(&fixture, peer_id.clone()).await;
        advertise_tip(&fixture, peer_id, block::Height(0), block::Height(5), 5, 1).await;
    }

    let mut requested = HashSet::new();
    while requested.len() < HEADER_SYNC_FANOUT {
        if let HeaderSyncAction::SendMessage {
            peer,
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(start_height, block::Height(1));
            assert_eq!(count, 5);
            requested.insert(peer);
        }
    }

    assert_eq!(requested.len(), HEADER_SYNC_FANOUT);
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_narrows_large_ranges_before_tracking_fanout() {
    let network = Network::Mainnet;
    let first_checkpoint = network
        .checkpoint_list()
        .min_height_in_range(block::Height(1)..)
        .expect("mainnet has a checkpoint above genesis");
    let best_header_hash = block::Hash([3; 32]);
    let start = next_height(first_checkpoint).expect("checkpoint height has successor");
    let unclamped_tip = block::Height(
        start
            .0
            .checked_add(MAX_HS_RANGE)
            .expect("test range fits in height"),
    );
    let clamped_count = clamp_header_sync_request_count(
        MAX_HS_RANGE,
        MAX_HS_RANGE,
        &network,
        LOCAL_MAX_MESSAGE_BYTES,
    );
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((first_checkpoint, best_header_hash)),
    ));
    let peers = [peer(37), peer(38), peer(39), peer(40)];

    for peer_id in peers.clone() {
        connect_peer(&fixture, peer_id.clone()).await;
        advertise_tip(
            &fixture,
            peer_id,
            block::Height(0),
            unclamped_tip,
            MAX_HS_RANGE,
            1,
        )
        .await;
    }

    let mut requested = HashSet::new();
    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.actions.recv(),
    )
    .await
    {
        if let HeaderSyncAction::SendMessage {
            peer,
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
        } = action
        {
            assert_eq!(start_height, start);
            assert_eq!(count, clamped_count);
            assert!(
                requested.insert(peer),
                "scheduler must not duplicate a clamped chunk for one peer"
            );
        }
    }
    assert_eq!(requested.len(), HEADER_SYNC_FANOUT);

    let chunk_tip = height_after_count(start, clamped_count)
        .and_then(previous_height)
        .expect("clamped range has a tip");
    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitted {
            start_height: start,
            tip_height: chunk_tip,
            tip_hash: block::Hash([4; 32]),
        })
        .await
        .unwrap();

    loop {
        if let HeaderSyncAction::SendMessage {
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
            ..
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(
                start_height,
                next_height(chunk_tip).expect("committed chunk tip has successor")
            );
            assert_eq!(count, clamped_count);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_creates_checkpoint_forward_before_backward_ranges() {
    let (network, checkpoint_hash) = checkpoint_regtest(block::Height(3));
    let mut fixture = spawn_test_reactor(startup_for(
        network,
        (block::Height(3), checkpoint_hash),
        Some((block::Height(3), checkpoint_hash)),
    ));
    let peer_id = peer(6);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id,
        block::Height(0),
        block::Height(8),
        DEFAULT_HS_RANGE,
        10,
    )
    .await;

    loop {
        if let HeaderSyncAction::SendMessage {
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
            ..
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(start_height, block::Height(4));
            assert_eq!(count, 5);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn scheduler_creates_backward_checkpoint_terminating_ranges() {
    let (network, checkpoint_hash) = checkpoint_regtest(block::Height(3));
    let mut fixture = spawn_test_reactor(startup_for(
        network,
        (block::Height(3), checkpoint_hash),
        Some((block::Height(3), checkpoint_hash)),
    ));
    let peer_id = peer(7);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id,
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        10,
    )
    .await;

    loop {
        if let HeaderSyncAction::SendMessage {
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
            ..
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(start_height, block::Height(1));
            assert_eq!(count, 3);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incoming_headers_match_outstanding_before_commit() {
    let checkpoint_hash = block::Hash::from(mainnet_header(&BLOCK_MAINNET_3_BYTES).as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let first_checkpoint = block::Height(3);
    let start = block::Height(4);
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((first_checkpoint, checkpoint_hash)),
    ));
    let peer_id = peer(8);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(&fixture, peer_id.clone(), block::Height(0), start, 1, 1).await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![mainnet_header(&BLOCK_MAINNET_4_BYTES)]),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::CommitHeaderRange {
            peer,
            start_height,
            finalized,
            ..
        } => {
            assert_eq!(peer, peer_id);
            assert_eq!(start_height, start);
            assert!(!finalized);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn headers_over_outstanding_contract_reports_response_too_long_without_flooding() {
    let network = Network::Mainnet;
    let first_checkpoint = network
        .checkpoint_list()
        .min_height_in_range(block::Height(1)..)
        .expect("mainnet has a checkpoint above genesis");
    let previous_hash = block::Hash([1; 32]);
    let start = next_height(first_checkpoint).expect("checkpoint height has successor");
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((first_checkpoint, previous_hash)),
    ));
    let peer_id = peer(61);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(start.0 + 1),
        1,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { count: 1, .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![
                mainnet_header(&BLOCK_MAINNET_1_BYTES),
                mainnet_header(&BLOCK_MAINNET_2_BYTES),
            ]),
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, peer_id);
                assert_eq!(reason, HeaderSyncMisbehavior::ResponseTooLong);
                break;
            }
            HeaderSyncAction::ForwardNewBlock { .. } => {
                panic!("backfill Headers must never produce tip-flood forwarding")
            }
            _ => {}
        }
    }
    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn matching_headers_are_statelessly_validated_before_commit() {
    let network = Network::Mainnet;
    let first_checkpoint = network
        .checkpoint_list()
        .min_height_in_range(block::Height(1)..)
        .expect("mainnet has a checkpoint above genesis");
    let two_before_checkpoint = block::Height(
        first_checkpoint
            .0
            .checked_sub(2)
            .expect("mainnet first checkpoint has two predecessors"),
    );
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((two_before_checkpoint, block::Hash([1; 32]))),
    ));
    let peer_id = peer(32);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        first_checkpoint,
        2,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    let mut bad_second = *mainnet_header(&BLOCK_MAINNET_2_BYTES);
    bad_second.previous_block_hash = block::Hash([7; 32]);
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![
                mainnet_header(&BLOCK_MAINNET_1_BYTES),
                Arc::new(bad_second),
            ]),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
        }
        action => panic!("unexpected action: {action:?}"),
    }
    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_async_header_commit_failure_reports_peer_disconnect() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(62);

    connect_peer(&fixture, peer_id.clone()).await;
    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitFailed {
            peer: peer_id.clone(),
            start_height: block::Height(1),
            count: 1,
            kind: HeaderSyncCommitFailureKind::InvalidPeerRange,
        })
        .await
        .unwrap();

    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn peer_disconnect_removes_outstanding_requests_for_that_peer() {
    let network = Network::Mainnet;
    let first_checkpoint = network
        .checkpoint_list()
        .min_height_in_range(block::Height(1)..)
        .expect("mainnet has a checkpoint above genesis");
    let previous_checkpoint_height =
        previous_height(first_checkpoint).expect("checkpoint above genesis has predecessor");
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((previous_checkpoint_height, block::Hash([1; 32]))),
    ));
    let peer_id = peer(11);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        first_checkpoint,
        1,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::PeerDisconnected(peer_id.clone()))
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![mainnet_header(&BLOCK_MAINNET_1_BYTES)]),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::UnsolicitedHeaders);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn timed_out_range_retries_with_another_peer() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_with_timeout(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        std::time::Duration::from_millis(1),
    ));
    let first_peer = peer(12);
    let second_peer = peer(13);

    connect_peer(&fixture, first_peer.clone()).await;
    advertise_tip(
        &fixture,
        first_peer,
        block::Height(0),
        block::Height(2),
        2,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    connect_peer(&fixture, second_peer.clone()).await;
    advertise_tip(
        &fixture,
        second_peer.clone(),
        block::Height(0),
        block::Height(2),
        2,
        1,
    )
    .await;

    loop {
        if let HeaderSyncAction::SendMessage { peer, msg } =
            next_non_query_action(&mut fixture.actions).await
        {
            if matches!(msg, HeaderSyncMessage::GetHeaders { .. }) {
                assert_eq!(peer, second_peer);
                break;
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn covered_hedged_outstanding_ranges_do_not_commit_twice() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let first_peer = peer(33);
    let second_peer = peer(34);

    for peer_id in [first_peer.clone(), second_peer.clone()] {
        connect_peer(&fixture, peer_id.clone()).await;
        advertise_tip(&fixture, peer_id, block::Height(0), block::Height(2), 2, 1).await;
    }

    let mut requested = HashSet::new();
    while requested.len() < 2 {
        if let HeaderSyncAction::SendMessage { peer, msg } =
            next_non_query_action(&mut fixture.actions).await
        {
            if matches!(msg, HeaderSyncMessage::GetHeaders { .. }) {
                requested.insert(peer);
            }
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitted {
            start_height: block::Height(1),
            tip_height: block::Height(2),
            tip_hash: block::Hash([2; 32]),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: second_peer,
            msg: HeaderSyncMessage::Headers(Vec::new()),
        })
        .await
        .unwrap();

    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_commit_failure_retries_without_peer_misbehavior() {
    let checkpoint_hash = block::Hash::from(mainnet_header(&BLOCK_MAINNET_3_BYTES).as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let start = block::Height(4);
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((block::Height(3), checkpoint_hash)),
    ));
    let first_peer = peer(35);
    let second_peer = peer(36);

    for peer_id in [first_peer.clone(), second_peer.clone()] {
        connect_peer(&fixture, peer_id.clone()).await;
        advertise_tip(&fixture, peer_id, block::Height(0), start, 1, 1).await;
    }

    loop {
        if let HeaderSyncAction::SendMessage { peer, msg } =
            next_non_query_action(&mut fixture.actions).await
        {
            if matches!(msg, HeaderSyncMessage::GetHeaders { .. }) && peer == first_peer {
                break;
            }
        }
    }
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: first_peer.clone(),
            msg: HeaderSyncMessage::Headers(vec![mainnet_header(&BLOCK_MAINNET_4_BYTES)]),
        })
        .await
        .unwrap();
    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::Misbehavior { .. } => {
                panic!("valid headers must not be scored before local commit failure")
            }
            HeaderSyncAction::CommitHeaderRange {
                peer,
                start_height,
                headers,
                ..
            } => {
                assert_eq!(peer, first_peer);
                assert_eq!(start_height, start);
                assert_eq!(headers.len(), 1);
                break;
            }
            _ => {}
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitFailed {
            peer: first_peer.clone(),
            start_height: start,
            count: 1,
            kind: HeaderSyncCommitFailureKind::Local,
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::Misbehavior { .. } => {
                panic!("local commit failure must not score peer")
            }
            HeaderSyncAction::SendMessage {
                peer,
                msg:
                    HeaderSyncMessage::GetHeaders {
                        start_height,
                        count,
                    },
            } if peer == first_peer || peer == second_peer => {
                assert_eq!(start_height, start);
                assert_eq!(count, 1);
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn material_tip_advance_sends_rate_limited_unsolicited_status() {
    let network = regtest_network();
    let mut startup = startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    );
    startup.status_refresh_interval = std::time::Duration::from_secs(60);
    let mut fixture = spawn_test_reactor(startup);
    let peer_id = peer(14);

    connect_peer(&fixture, peer_id.clone()).await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::Status(_),
                ..
            }
        ) {
            break;
        }
    }

    for height in [block::Height(1), block::Height(2)] {
        fixture
            .handle
            .send(HeaderSyncEvent::HeaderRangeCommitted {
                start_height: height,
                tip_height: height,
                tip_hash: block::Hash(
                    [u8::try_from(height.0).expect("test heights fit in u8"); 32],
                ),
            })
            .await
            .unwrap();
    }

    let mut status_count = 0;
    while let Ok(Some(action)) =
        tokio::time::timeout(std::time::Duration::from_millis(20), fixture.actions.recv()).await
    {
        if matches!(
            action,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::Status(_),
                ..
            }
        ) {
            status_count += 1;
        }
    }

    assert_eq!(status_count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn full_block_committed_covers_outstanding_height() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(42);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(1),
        1,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::FullBlockCommitted {
            height: block::Height(1),
            hash: block::Hash([1; 32]),
            header: mainnet_header(&BLOCK_MAINNET_1_BYTES),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id,
            msg: HeaderSyncMessage::Headers(Vec::new()),
        })
        .await
        .unwrap();

    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_unseen_valid_new_block_is_seen_and_forwarded_to_eligible_peers() {
    let network = Network::Mainnet;
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let hash = block.hash();
    let height = block.coinbase_height().expect("test block has height");
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let source = peer(46);
    let eligible = peer(47);
    let redundant = peer(48);

    for peer_id in [source.clone(), eligible.clone(), redundant.clone()] {
        connect_peer(&fixture, peer_id).await;
    }
    advertise_tip(
        &fixture,
        source.clone(),
        block::Height(0),
        block::Height(0),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    advertise_tip(
        &fixture,
        eligible.clone(),
        block::Height(0),
        block::Height(0),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    advertise_tip(
        &fixture,
        redundant.clone(),
        block::Height(0),
        height,
        DEFAULT_HS_RANGE,
        1,
    )
    .await;

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: source.clone(),
            msg: HeaderSyncMessage::NewBlock(block.clone()),
        })
        .await
        .unwrap();

    let mut saw_pipeline_fact = false;
    let mut forwarded = Vec::new();
    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        fixture.actions.recv(),
    )
    .await
    {
        match action {
            HeaderSyncAction::NewBlockReceived {
                peer,
                height: action_height,
                hash: action_hash,
                ..
            } => {
                assert_eq!(peer, source);
                assert_eq!(action_height, height);
                assert_eq!(action_hash, hash);
                saw_pipeline_fact = true;
                fixture
                    .handle
                    .send(HeaderSyncEvent::NewBlockAccepted {
                        peer: source.clone(),
                        height,
                        hash,
                        block: block.clone(),
                    })
                    .await
                    .unwrap();
            }
            HeaderSyncAction::ForwardNewBlock {
                source: action_source,
                peer,
                height: action_height,
                hash: action_hash,
                ..
            } => {
                assert_eq!(action_source, Some(source.clone()));
                assert_eq!(action_height, height);
                assert_eq!(action_hash, hash);
                forwarded.push(peer);
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                panic!("valid NewBlock must not score {peer:?}: {reason:?}");
            }
            _ => {}
        }
    }

    assert!(saw_pipeline_fact);
    assert_eq!(forwarded, vec![eligible]);

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: source.clone(),
            msg: HeaderSyncMessage::NewBlock(block),
        })
        .await
        .unwrap();

    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.actions.recv(),
    )
    .await
    {
        if matches!(
            action,
            HeaderSyncAction::ForwardNewBlock { .. }
                | HeaderSyncAction::NewBlockReceived { .. }
                | HeaderSyncAction::Misbehavior { .. }
        ) {
            panic!("duplicate NewBlock must be cheap-deduped without scoring: {action:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_new_block_dedups_pending_acceptance_without_scoring() {
    let network = Network::Mainnet;
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let hash = block.hash();
    let height = block.coinbase_height().expect("test block has height");
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let first_peer = peer(52);
    let duplicate_peer = peer(53);
    let eligible_peer = peer(54);

    for peer_id in [
        first_peer.clone(),
        duplicate_peer.clone(),
        eligible_peer.clone(),
    ] {
        connect_peer(&fixture, peer_id.clone()).await;
        advertise_tip(
            &fixture,
            peer_id,
            block::Height(0),
            block::Height(0),
            DEFAULT_HS_RANGE,
            1,
        )
        .await;
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: first_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(block.clone()),
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::NewBlockReceived {
                peer,
                height: action_height,
                hash: action_hash,
                ..
            } => {
                assert_eq!(peer, first_peer);
                assert_eq!(action_height, height);
                assert_eq!(action_hash, hash);
                break;
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                panic!("first valid NewBlock must not score {peer:?}: {reason:?}");
            }
            _ => {}
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: duplicate_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(block.clone()),
        })
        .await
        .unwrap();

    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.actions.recv(),
    )
    .await
    {
        if matches!(
            action,
            HeaderSyncAction::NewBlockReceived { .. } | HeaderSyncAction::Misbehavior { .. }
        ) {
            panic!("pending duplicate NewBlock must not re-enter acceptance or score: {action:?}");
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::NewBlockAccepted {
            peer: first_peer,
            height,
            hash,
            block,
        })
        .await
        .unwrap();

    let mut forwarded = HashSet::new();
    while forwarded.len() < 2 {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::ForwardNewBlock {
                peer,
                height: action_height,
                hash: action_hash,
                ..
            } => {
                assert_eq!(action_height, height);
                assert_eq!(action_hash, hash);
                forwarded.insert(peer);
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                panic!("accepted duplicate flow must not score {peer:?}: {reason:?}");
            }
            _ => {}
        }
    }
    assert_eq!(forwarded, HashSet::from([duplicate_peer, eligible_peer]));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_full_block_commit_prevents_later_new_block_regossip() {
    let network = Network::Mainnet;
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let height = block.coinbase_height().expect("test block has height");
    let hash = block.hash();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let source = peer(49);
    let destination = peer(50);

    for peer_id in [source.clone(), destination] {
        connect_peer(&fixture, peer_id).await;
    }
    fixture
        .handle
        .send(HeaderSyncEvent::FullBlockCommitted {
            height,
            hash,
            header: block.header.clone(),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: source,
            msg: HeaderSyncMessage::NewBlock(block),
        })
        .await
        .unwrap();

    while let Ok(Some(action)) = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.actions.recv(),
    )
    .await
    {
        if matches!(action, HeaderSyncAction::ForwardNewBlock { .. }) {
            panic!("locally committed block must not be gossiped twice: {action:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_and_malformed_new_block_report_disconnect() {
    let network = Network::Mainnet;
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let unknown_peer = peer(63);
    let invalid_peer = peer(51);
    let malformed_peer = peer(52);
    connect_peer(&fixture, invalid_peer.clone()).await;
    connect_peer(&fixture, malformed_peer.clone()).await;

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: unknown_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_1_BYTES)),
        })
        .await
        .unwrap();
    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, unknown_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::UnknownPeer);
            break;
        }
    }

    let mut bad_block = (*mainnet_block(&BLOCK_MAINNET_1_BYTES)).clone();
    let mut bad_header = *bad_block.header;
    bad_header.nonce[0] ^= 1;
    bad_block.header = Arc::new(bad_header);
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: invalid_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(Arc::new(bad_block)),
        })
        .await
        .unwrap();

    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, invalid_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidNewBlock);
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireDecodeFailed {
            peer: malformed_peer.clone(),
            error: Arc::new(HeaderSyncWireError::UnknownMessageType(MSG_HS_NEW_BLOCK)),
        })
        .await
        .unwrap();

    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, malformed_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::MalformedMessage);
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rapid_status_updates_and_new_block_spam_report_disconnect() {
    let network = Network::Mainnet;
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let status_peer = peer(53);
    let block_peer = peer(54);
    connect_peer(&fixture, status_peer.clone()).await;
    connect_peer(&fixture, block_peer.clone()).await;

    for _ in 0..2 {
        advertise_tip(
            &fixture,
            status_peer.clone(),
            block::Height(0),
            block::Height(1),
            DEFAULT_HS_RANGE,
            1,
        )
        .await;
    }

    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, status_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::StatusSpam);
            break;
        }
    }

    for bytes in [
        BLOCK_MAINNET_1_BYTES.as_slice(),
        BLOCK_MAINNET_2_BYTES.as_slice(),
    ] {
        fixture
            .handle
            .send(HeaderSyncEvent::WireMessage {
                peer: block_peer.clone(),
                msg: HeaderSyncMessage::NewBlock(mainnet_block(bytes)),
            })
            .await
            .unwrap();
    }

    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, block_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::NewBlockSpam);
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_block_spam_does_not_poison_seen_cache() {
    let network = Network::Mainnet;
    let first_block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let second_block = mainnet_block(&BLOCK_MAINNET_2_BYTES);
    let second_hash = second_block.hash();
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let spam_peer = peer(56);
    let honest_peer = peer(57);
    let destination = peer(58);

    for peer_id in [spam_peer.clone(), honest_peer.clone(), destination] {
        connect_peer(&fixture, peer_id).await;
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: spam_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(first_block),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::NewBlockReceived { hash, .. } if hash != second_hash
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: spam_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(second_block.clone()),
        })
        .await
        .unwrap();
    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, spam_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::NewBlockSpam);
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: honest_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(second_block),
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::NewBlockReceived { peer, hash, .. } if hash == second_hash => {
                assert_eq!(peer, honest_peer);
                break;
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                panic!("honest retry must not be deduped or scored: {peer:?} {reason:?}");
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_new_block_does_not_forward_or_poison_seen_cache() {
    let network = Network::Mainnet;
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let hash = block.hash();
    let height = block.coinbase_height().expect("test block has height");
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let source = peer(59);
    let retry_peer = peer(60);
    let destination = peer(61);

    for peer_id in [source.clone(), retry_peer.clone(), destination] {
        connect_peer(&fixture, peer_id).await;
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: source.clone(),
            msg: HeaderSyncMessage::NewBlock(block.clone()),
        })
        .await
        .unwrap();

    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::NewBlockReceived { peer, hash: action_hash, .. }
                if peer == source && action_hash == hash
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::NewBlockRejected {
            peer: source.clone(),
            hash,
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, source);
                assert_eq!(reason, HeaderSyncMisbehavior::InvalidNewBlock);
                break;
            }
            HeaderSyncAction::ForwardNewBlock { .. } => {
                panic!("rejected NewBlock must not be forwarded");
            }
            _ => {}
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: retry_peer.clone(),
            msg: HeaderSyncMessage::NewBlock(block),
        })
        .await
        .unwrap();

    loop {
        match next_non_query_action(&mut fixture.actions).await {
            HeaderSyncAction::NewBlockReceived {
                peer,
                height: action_height,
                hash: action_hash,
                ..
            } => {
                assert_eq!(peer, retry_peer);
                assert_eq!(action_height, height);
                assert_eq!(action_hash, hash);
                break;
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                panic!("retry after rejection must not be scored: {peer:?} {reason:?}");
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_get_headers_requires_status_and_respects_serving_cap() {
    let network = regtest_network();
    let mut startup = startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    );
    startup.config.max_headers_per_response = 3;
    startup.config.max_inflight_requests = 2;
    let mut fixture = spawn_test_reactor(startup);
    let no_status_peer = peer(59);
    let requester = peer(60);

    connect_peer(&fixture, no_status_peer.clone()).await;
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: no_status_peer.clone(),
            msg: HeaderSyncMessage::GetHeaders {
                start_height: block::Height(1),
                count: 1,
            },
        })
        .await
        .unwrap();
    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, no_status_peer);
            assert_eq!(reason, HeaderSyncMisbehavior::GetHeadersSpam);
            break;
        }
    }

    connect_peer(&fixture, requester.clone()).await;
    advertise_tip(
        &fixture,
        requester.clone(),
        block::Height(0),
        block::Height(0),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;

    for start in [block::Height(1), block::Height(4)] {
        fixture
            .handle
            .send(HeaderSyncEvent::WireMessage {
                peer: requester.clone(),
                msg: HeaderSyncMessage::GetHeaders {
                    start_height: start,
                    count: 3,
                },
            })
            .await
            .unwrap();
        match next_query_headers_action(&mut fixture.actions).await {
            HeaderSyncAction::QueryHeadersByHeightRange {
                peer,
                start: action_start,
                count,
            } => {
                assert_eq!(peer, requester);
                assert_eq!(action_start, start);
                assert_eq!(count, 3);
            }
            action => panic!("unexpected action: {action:?}"),
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: requester.clone(),
            msg: HeaderSyncMessage::GetHeaders {
                start_height: block::Height(7),
                count: 1,
            },
        })
        .await
        .unwrap();
    loop {
        if let HeaderSyncAction::Misbehavior { peer, reason } =
            next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(peer, requester);
            assert_eq!(reason, HeaderSyncMisbehavior::GetHeadersSpam);
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeResponseFinished {
            peer: requester.clone(),
            start_height: block::Height(1),
            requested_count: 1,
            returned_count: 0,
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: requester.clone(),
            msg: HeaderSyncMessage::GetHeaders {
                start_height: block::Height(8),
                count: 1,
            },
        })
        .await
        .unwrap();
    match next_query_headers_action(&mut fixture.actions).await {
        HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
            assert_eq!(peer, requester);
            assert_eq!(start, block::Height(8));
            assert_eq!(count, 1);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_get_headers_over_cap_disconnects_without_state_read() {
    let network = regtest_network();
    let mut startup = startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    );
    startup.config.max_headers_per_response = 3;
    let mut fixture = spawn_test_reactor(startup);
    let requester = peer(61);

    connect_peer(&fixture, requester.clone()).await;
    advertise_tip(
        &fixture,
        requester.clone(),
        block::Height(0),
        block::Height(0),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: requester.clone(),
            msg: HeaderSyncMessage::GetHeaders {
                start_height: block::Height(1),
                count: 4,
            },
        })
        .await
        .unwrap();

    loop {
        match next_action(&mut fixture.actions).await {
            HeaderSyncAction::QueryHeadersByHeightRange { .. } => {
                panic!("over-cap GetHeaders must not query state");
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, requester);
                assert_eq!(reason, HeaderSyncMisbehavior::GetHeadersTooLong);
                break;
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn header_sync_jsonl_trace_captures_status_range_dedup_and_disconnect() {
    let network = Network::Mainnet;
    let mut capture = TraceCapture::for_test(
        "header_sync_jsonl_trace_captures_status_range_dedup_and_disconnect",
    )
    .unwrap();
    let first_checkpoint = network
        .checkpoint_list()
        .min_height_in_range(block::Height(1)..)
        .expect("mainnet has a checkpoint above genesis");
    let checkpoint_hash = network
        .checkpoint_list()
        .hash(first_checkpoint)
        .expect("checkpoint height has a hash");
    let mut startup = startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((first_checkpoint, checkpoint_hash)),
    );
    startup.trace = ZakuraTrace::new(capture.tracer(), "01");
    let fixture = spawn_test_reactor(startup);
    let peer_id = peer(55);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        next_height(first_checkpoint).expect("checkpoint has a successor"),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    fixture
        .handle
        .send(HeaderSyncEvent::FullBlockCommitted {
            height: block::Height(1),
            hash: mainnet_block(&BLOCK_MAINNET_1_BYTES).hash(),
            header: mainnet_header(&BLOCK_MAINNET_1_BYTES),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_1_BYTES)),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireDecodeFailed {
            peer: peer_id,
            error: Arc::new(HeaderSyncWireError::UnknownMessageType(99)),
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    capture.flush().await;
    let reader = capture.reader().unwrap();
    let header_sync = reader.table(HEADER_SYNC_TABLE.table());

    assert!(header_sync.count(hs_trace::HEADER_STATUS_SENT) >= 1);
    assert!(header_sync.count(hs_trace::HEADER_STATUS_RECEIVED) >= 1);
    assert!(header_sync.count(hs_trace::HEADER_GET_HEADERS_SENT) >= 1);
    assert!(header_sync.count(hs_trace::HEADER_NEW_BLOCK_DEDUPED) >= 1);
    assert!(header_sync.count(hs_trace::HEADER_PEER_DISCONNECT_REQUESTED) >= 1);

    for row in header_sync.rows() {
        assert!(
            row.get("block").is_none() && row.get("headers").is_none(),
            "header-sync trace rows must not contain full payloads: {row:?}"
        );
    }

    let _ = capture.finish().await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn header_sync_metrics_record_status_range_new_block_dedup_and_disconnect() {
    let metrics = [
        "sync.header.peer.status.sent",
        "sync.header.peer.status.received",
        "sync.header.request.sent",
        "sync.header.response.received",
        "sync.header.range.committed",
        "sync.header.tip.new_block.received",
        "sync.header.tip.new_block.deduped",
        "sync.header.peer.disconnect",
    ];
    let before = metric_snapshot(&metrics);

    let first_checkpoint = block::Height(3);
    let checkpoint_hash = block::Hash::from(mainnet_header(&BLOCK_MAINNET_3_BYTES).as_ref());
    let (network, _) = checkpoint_testnet_with_hash(first_checkpoint, checkpoint_hash);
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        Some((first_checkpoint, checkpoint_hash)),
    ));
    let peer_id = peer(56);

    connect_peer(&fixture, peer_id.clone()).await;
    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::SendMessage {
            msg: HeaderSyncMessage::Status(_),
            ..
        } => {}
        action => panic!("unexpected action: {action:?}"),
    }

    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(4),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![mainnet_header(&BLOCK_MAINNET_4_BYTES)]),
        })
        .await
        .unwrap();
    let committed_hash = match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::CommitHeaderRange {
            start_height,
            headers,
            ..
        } => {
            assert_eq!(
                start_height,
                next_height(first_checkpoint).expect("checkpoint has a successor")
            );
            block::Hash::from(headers.last().expect("one header").as_ref())
        }
        action => panic!("unexpected action: {action:?}"),
    };

    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitted {
            start_height: next_height(first_checkpoint).expect("checkpoint has a successor"),
            tip_height: next_height(first_checkpoint).expect("checkpoint has a successor"),
            tip_hash: committed_hash,
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::FullBlockCommitted {
            height: block::Height(1),
            hash: mainnet_block(&BLOCK_MAINNET_1_BYTES).hash(),
            header: mainnet_header(&BLOCK_MAINNET_1_BYTES),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::NewBlock(mainnet_block(&BLOCK_MAINNET_1_BYTES)),
        })
        .await
        .unwrap();
    fixture
        .handle
        .send(HeaderSyncEvent::WireDecodeFailed {
            peer: peer_id,
            error: Arc::new(HeaderSyncWireError::UnknownMessageType(99)),
        })
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    for metric in metrics {
        assert_metric_incremented(&before, metric);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn unsolicited_headers_are_misbehavior_but_empty_headers_retry() {
    let network = regtest_network();
    let mut fixture = spawn_test_reactor(startup_with_timeout(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        std::time::Duration::from_millis(20),
    ));
    let peer_id = peer(9);

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(Vec::new()),
        })
        .await
        .unwrap();
    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::UnsolicitedHeaders);
        }
        action => panic!("unexpected action: {action:?}"),
    }

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(1),
        1,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }
    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(Vec::new()),
        })
        .await
        .unwrap();
    assert!(
        matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ),
        "empty Headers for an outstanding range should retry without disconnecting"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn committed_range_updates_best_tip_watch_and_does_not_advance_finality() {
    let network = regtest_network();
    let fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let mut tip = fixture.handle.subscribe_tip();
    let tip_hash = block::Hash([12; 32]);

    fixture
        .handle
        .send(HeaderSyncEvent::HeaderRangeCommitted {
            start_height: block::Height(1),
            tip_height: block::Height(1),
            tip_hash,
        })
        .await
        .unwrap();

    tip.changed().await.unwrap();
    assert_eq!(*tip.borrow(), (block::Height(1), tip_hash));
    assert_ne!(fixture.handle.best_header_tip().0, block::Height(0));
}

#[tokio::test(flavor = "current_thread")]
async fn forward_genesis_backfill_reaches_checkpoint_before_finalized_commit() {
    let headers = [
        mainnet_header(&BLOCK_MAINNET_1_BYTES),
        mainnet_header(&BLOCK_MAINNET_2_BYTES),
        mainnet_header(&BLOCK_MAINNET_3_BYTES),
    ];
    let checkpoint_hash = block::Hash::from(headers[2].as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(43);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if let HeaderSyncAction::SendMessage {
            msg:
                HeaderSyncMessage::GetHeaders {
                    start_height,
                    count,
                },
            ..
        } = next_non_query_action(&mut fixture.actions).await
        {
            assert_eq!(start_height, block::Height(1));
            assert_eq!(count, 3);
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(headers.to_vec()),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::CommitHeaderRange {
            peer,
            start_height,
            headers,
            finalized,
            ..
        } => {
            assert_eq!(peer, peer_id);
            assert_eq!(start_height, block::Height(1));
            assert_eq!(headers.len(), 3);
            assert!(finalized);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn truncated_finalized_backfill_is_rejected_before_commit() {
    let headers = [
        mainnet_header(&BLOCK_MAINNET_1_BYTES),
        mainnet_header(&BLOCK_MAINNET_2_BYTES),
        mainnet_header(&BLOCK_MAINNET_3_BYTES),
    ];
    let checkpoint_hash = block::Hash::from(headers[2].as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let mut fixture = spawn_test_reactor(startup_for(
        network.clone(),
        (block::Height(0), network.genesis_hash()),
        None,
    ));
    let peer_id = peer(44);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(headers[..2].to_vec()),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
        }
        action => panic!("unexpected action: {action:?}"),
    }
    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "current_thread")]
async fn backward_checkpoint_backfill_accepts_linking_run_as_finalized() {
    let headers = [
        mainnet_header(&BLOCK_MAINNET_1_BYTES),
        mainnet_header(&BLOCK_MAINNET_2_BYTES),
        mainnet_header(&BLOCK_MAINNET_3_BYTES),
    ];
    let checkpoint_hash = block::Hash::from(headers[2].as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let mut fixture = spawn_test_reactor(startup_for(
        network,
        (block::Height(3), checkpoint_hash),
        Some((block::Height(3), checkpoint_hash)),
    ));
    let peer_id = peer(45);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(headers.to_vec()),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::CommitHeaderRange {
            peer,
            start_height,
            headers,
            finalized,
            ..
        } => {
            assert_eq!(peer, peer_id);
            assert_eq!(start_height, block::Height(1));
            assert_eq!(headers.len(), 3);
            assert!(finalized);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_backfill_rejects_non_contiguous_run_before_commit() {
    let (network, checkpoint_hash) = checkpoint_regtest(block::Height(3));
    let mut fixture = spawn_test_reactor(startup_for(
        network,
        (block::Height(3), checkpoint_hash),
        Some((block::Height(3), checkpoint_hash)),
    ));
    let peer_id = peer(10);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![
                mainnet_header(&BLOCK_MAINNET_GENESIS_BYTES),
                mainnet_header(&BLOCK_MAINNET_GENESIS_BYTES),
                mainnet_header(&BLOCK_MAINNET_GENESIS_BYTES),
            ]),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
        }
        action => panic!("unexpected action: {action:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn header_response_that_does_not_link_to_anchor_is_misbehavior_before_commit() {
    let checkpoint_hash = block::Hash::from(mainnet_header(&BLOCK_MAINNET_3_BYTES).as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), checkpoint_hash);
    let anchor = (block::Height(0), network.genesis_hash());
    let mut fixture = spawn_test_reactor(startup_for(network, anchor, Some(anchor)));
    let peer_id = peer(46);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(4),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(vec![mainnet_header(&BLOCK_MAINNET_2_BYTES)]),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
        }
        action => panic!("unexpected action: {action:?}"),
    }
    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "current_thread")]
async fn checkpoint_backfill_rejects_checkpoint_hash_mismatch_before_commit() {
    let headers = [
        mainnet_header(&BLOCK_MAINNET_1_BYTES),
        mainnet_header(&BLOCK_MAINNET_2_BYTES),
        mainnet_header(&BLOCK_MAINNET_3_BYTES),
    ];
    let divergent_checkpoint_hash = block::Hash::from(headers[0].as_ref());
    let (network, _) = checkpoint_testnet_with_hash(block::Height(3), divergent_checkpoint_hash);
    let mut fixture = spawn_test_reactor(startup_for(
        network,
        (block::Height(3), divergent_checkpoint_hash),
        Some((block::Height(3), divergent_checkpoint_hash)),
    ));
    let peer_id = peer(46);

    connect_peer(&fixture, peer_id.clone()).await;
    advertise_tip(
        &fixture,
        peer_id.clone(),
        block::Height(0),
        block::Height(3),
        DEFAULT_HS_RANGE,
        1,
    )
    .await;
    loop {
        if matches!(
            next_non_query_action(&mut fixture.actions).await,
            HeaderSyncAction::SendMessage {
                msg: HeaderSyncMessage::GetHeaders { .. },
                ..
            }
        ) {
            break;
        }
    }

    fixture
        .handle
        .send(HeaderSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: HeaderSyncMessage::Headers(headers.to_vec()),
        })
        .await
        .unwrap();

    match next_non_query_action(&mut fixture.actions).await {
        HeaderSyncAction::Misbehavior { peer, reason } => {
            assert_eq!(peer, peer_id);
            assert_eq!(reason, HeaderSyncMisbehavior::InvalidRange);
        }
        action => panic!("unexpected action: {action:?}"),
    }
    assert_no_commit_or_misbehavior(&mut fixture.actions).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_validation_accepts_valid_contiguous_headers() {
    let headers = vec![mainnet_header(&BLOCK_MAINNET_1_BYTES)];
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(1, DEFAULT_HS_RANGE),
    };

    validate_headers_stateless(headers, context).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_validation_rejects_non_contiguous_and_future_headers() {
    let mut second = *mainnet_header(&BLOCK_MAINNET_1_BYTES);
    second.previous_block_hash = block::Hash([1; 32]);
    let headers = vec![
        mainnet_header(&BLOCK_MAINNET_GENESIS_BYTES),
        Arc::new(second),
    ];
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(0),
        decode_context: headers_context(2, DEFAULT_HS_RANGE),
    };
    assert!(matches!(
        validate_headers_stateless(headers, context).await,
        Err(HeaderSyncWireError::NonContiguousHeaders)
    ));

    let mut future = *mainnet_header(&BLOCK_MAINNET_1_BYTES);
    future.time = Utc::now() + Duration::hours(3);
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(1, DEFAULT_HS_RANGE),
    };
    assert!(matches!(
        validate_headers_stateless(vec![Arc::new(future)], context).await,
        Err(HeaderSyncWireError::Time(_))
    ));
}

#[test]
fn range_link_validation_rejects_non_linking_headers() {
    let genesis = mainnet_block(&BLOCK_MAINNET_GENESIS_BYTES);
    let block1 = mainnet_header(&BLOCK_MAINNET_1_BYTES);
    let block2 = mainnet_header(&BLOCK_MAINNET_2_BYTES);

    let mut bad_first = *block1;
    bad_first.previous_block_hash = block::Hash([1; 32]);
    assert!(matches!(
        validate_header_range_links(genesis.hash(), &[Arc::new(bad_first)]),
        Err(HeaderSyncWireError::FirstHeaderDoesNotLink)
    ));

    let mut bad_second = *block2;
    bad_second.previous_block_hash = block::Hash([2; 32]);
    assert!(matches!(
        validate_header_range_links(genesis.hash(), &[block1, Arc::new(bad_second)]),
        Err(HeaderSyncWireError::NonContiguousHeaders)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_validation_rejects_bad_pow() {
    let mut bad_solution = *mainnet_header(&BLOCK_MAINNET_1_BYTES);
    bad_solution.nonce[0] ^= 1;
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(1, DEFAULT_HS_RANGE),
    };
    assert!(matches!(
        validate_headers_stateless(vec![Arc::new(bad_solution)], context).await,
        Err(HeaderSyncWireError::Equihash(_))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_block_stateless_validation_accepts_valid_mainnet_block() {
    validate_new_block_stateless(
        mainnet_block(&BLOCK_MAINNET_1_BYTES),
        &Network::Mainnet,
        Utc::now(),
        block::Height(1),
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn new_block_stateless_validation_rejects_wrong_solution_size_and_bad_pow() {
    let mut wrong_solution_size = (*mainnet_block(&BLOCK_MAINNET_1_BYTES)).clone();
    let mut header = *wrong_solution_size.header;
    header.solution = Solution::Regtest([0; 36]);
    wrong_solution_size.header = Arc::new(header);

    assert!(matches!(
        validate_new_block_stateless(
            Arc::new(wrong_solution_size),
            &Network::Mainnet,
            Utc::now(),
            block::Height(1),
        )
        .await,
        Err(HeaderSyncWireError::WrongEquihashSolutionSize)
    ));

    let mut bad_pow = (*mainnet_block(&BLOCK_MAINNET_1_BYTES)).clone();
    let mut header = *bad_pow.header;
    header.nonce[0] ^= 1;
    bad_pow.header = Arc::new(header);

    assert!(matches!(
        validate_new_block_stateless(
            Arc::new(bad_pow),
            &Network::Mainnet,
            Utc::now(),
            block::Height(1),
        )
        .await,
        Err(HeaderSyncWireError::Equihash(_))
    ));
}

#[test]
fn difficulty_filter_rejects_hash_above_threshold() {
    let threshold =
        CompactDifficulty::from_bytes_in_display_order(&[0x01, 0x01, 0x00, 0x00]).unwrap();

    assert!(matches!(
        validate_difficulty_filter(block::Hash([0xff; 32]), threshold),
        Err(HeaderSyncWireError::DifficultyFilter { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_header_validation_surfaces_difficulty_filter_after_equihash_acceptance() {
    let mut header = *mainnet_header(&BLOCK_MAINNET_1_BYTES);
    header.difficulty_threshold =
        CompactDifficulty::from_bytes_in_display_order(&[0x01, 0x01, 0x00, 0x00]).unwrap();
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(1, DEFAULT_HS_RANGE),
    };

    assert!(matches!(
        validate_headers_stateless_after_equihash_acceptance(vec![Arc::new(header)], context).await,
        Err(HeaderSyncWireError::DifficultyFilter { .. })
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stateless_validation_rejects_wrong_solution_size_for_network() {
    let mut regtest_sized = *mainnet_header(&BLOCK_MAINNET_1_BYTES);
    regtest_sized.solution = Solution::Regtest([0; 36]);
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(1, DEFAULT_HS_RANGE),
    };

    assert!(matches!(
        validate_headers_stateless(vec![Arc::new(regtest_sized)], context).await,
        Err(HeaderSyncWireError::WrongEquihashSolutionSize)
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn pow_validation_does_not_monopolize_the_runtime_thread() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let headers = vec![
        mainnet_header(&BLOCK_MAINNET_1_BYTES),
        mainnet_header(&BLOCK_MAINNET_2_BYTES),
        mainnet_header(&BLOCK_MAINNET_3_BYTES),
        mainnet_header(&BLOCK_MAINNET_4_BYTES),
    ];
    let context = HeaderSyncValidationContext {
        network: &Network::Mainnet,
        now: Utc::now(),
        start_height: block::Height(1),
        decode_context: headers_context(4, DEFAULT_HS_RANGE),
    };

    let ticks = Arc::new(AtomicUsize::new(0));
    let ticker_ticks = ticks.clone();
    let ticker = tokio::spawn(async move {
        loop {
            ticker_ticks.fetch_add(1, Ordering::SeqCst);
            tokio::task::yield_now().await;
        }
    });

    validate_headers_stateless(headers, context).await.unwrap();
    let progressed = ticks.load(Ordering::SeqCst);
    ticker.abort();

    assert!(
        progressed > 0,
        "reactor thread was blocked during PoW validation"
    );
}

#[test]
fn hostile_vectors_are_rejected_for_allocation_and_unsolicited_headers() {
    let mut encoded = vec![MSG_HS_HEADERS];
    encoded.write_u32::<LittleEndian>(u32::MAX).unwrap();
    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, headers_context(MAX_HS_RANGE, MAX_HS_RANGE)),
        Err(HeaderSyncWireError::HeaderCountLimit { .. })
    ));

    let mut encoded = vec![MSG_HS_HEADERS];
    encoded.write_u32::<LittleEndian>(1).unwrap();
    assert!(matches!(
        HeaderSyncMessage::decode(&encoded, HeaderSyncDecodeContext::control()),
        Err(HeaderSyncWireError::UnsolicitedHeaders)
    ));
}
