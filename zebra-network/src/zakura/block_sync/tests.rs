use std::{collections::HashMap, future};

use super::*;
use super::{
    config::{
        BS_PER_BLOCK_WORST_CASE_BYTES, DEFAULT_BS_FANOUT, DEFAULT_BS_MAX_SUBMITTED_BLOCK_APPLIES,
        DEFAULT_BS_REQUEST_TIMEOUT, MAX_BS_INFLIGHT_REQUESTS, MAX_BS_RESPONSE_BYTES,
    },
    reactor::node_id_from_block_peer_id,
    reorder::*,
    scheduler::*,
    state::*,
};
use crate::zakura::{
    framed_channel, ChainFrontier, FramedRecv, FramedSend, Frontier, FrontierChange,
    FrontierUpdate, Peer, PeerStreamSession, Service, ServicePeerSnapshot, ServiceRegistry,
    StreamMode, ZakuraBlockSyncCandidateState, ZakuraSyncExchange,
};
use zebra_chain::{
    fmt::HexDebug,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::Transaction,
    transparent,
};
use zebra_test::vectors::{BLOCK_MAINNET_1_BYTES, BLOCK_MAINNET_2_BYTES, BLOCK_MAINNET_3_BYTES};

fn peer(byte: u8) -> ZakuraPeerId {
    ZakuraPeerId::new(vec![byte; 32]).expect("test peer id is within bounds")
}

fn mainnet_block(bytes: &[u8]) -> Arc<block::Block> {
    Arc::new(bytes.zcash_deserialize_into().expect("block vector parses"))
}

fn mainnet_blocks_1_to_3() -> Vec<Arc<block::Block>> {
    vec![
        mainnet_block(&BLOCK_MAINNET_1_BYTES),
        mainnet_block(&BLOCK_MAINNET_2_BYTES),
        mainnet_block(&BLOCK_MAINNET_3_BYTES),
    ]
}

fn forked_block(block: &Arc<block::Block>, nonce_tag: u8) -> Arc<block::Block> {
    let mut fork = block.as_ref().clone();
    let mut header = *fork.header;
    header.nonce = HexDebug([nonce_tag; 32]);
    fork.header = Arc::new(header);
    Arc::new(fork)
}

fn block_with_bad_merkle_root(
    block: &Arc<block::Block>,
    extra_tx: &Arc<block::Block>,
) -> Arc<block::Block> {
    let mut bad_block = block.as_ref().clone();
    bad_block
        .transactions
        .push(extra_tx.transactions[0].clone());

    assert_eq!(bad_block.hash(), block.hash());
    assert_eq!(bad_block.coinbase_height(), block.coinbase_height());
    assert_ne!(
        bad_block
            .transactions
            .iter()
            .collect::<block::merkle::Root>(),
        bad_block.header.merkle_root
    );

    Arc::new(bad_block)
}

/// Build `count` internally-consistent blocks at the sequential heights
/// `1..=count`.
///
/// Each block is mainnet block 1 with its coinbase height rewritten and its
/// header merkle root recomputed, so it has a distinct hash and a header that
/// commits to its transactions. The real test vectors
/// only cover a handful of contiguous heights, which is too few to flood the
/// per-peer wire queue, so the body-flood test synthesizes its own chain.
fn fake_sequential_blocks(count: u32) -> Vec<Arc<block::Block>> {
    let template = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    (1..=count)
        .map(|height| fake_block_at_height(&template, block::Height(height)))
        .collect()
}

fn fake_blocks_in_range(start: u32, end: u32) -> Vec<Arc<block::Block>> {
    let template = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    (start..=end)
        .map(|height| fake_block_at_height(&template, block::Height(height)))
        .collect()
}

fn fake_block_at_height(template: &Arc<block::Block>, height: block::Height) -> Arc<block::Block> {
    let mut block = template.as_ref().clone();

    let mut coinbase = block.transactions[0].clone();
    let input = match Arc::make_mut(&mut coinbase) {
        Transaction::V1 { inputs, .. }
        | Transaction::V2 { inputs, .. }
        | Transaction::V3 { inputs, .. }
        | Transaction::V4 { inputs, .. }
        | Transaction::V5 { inputs, .. } => &mut inputs[0],
    };
    match input {
        transparent::Input::Coinbase {
            height: coinbase_height,
            ..
        } => *coinbase_height = height,
        _ => panic!("template block must start with a coinbase input"),
    }
    block.transactions[0] = coinbase;

    // Rewriting the coinbase changes the merkle root, so recompute it to keep the
    // synthesized block internally consistent (its header commits to its txs).
    let merkle_root = block.transactions.iter().collect::<block::merkle::Root>();
    let mut header = *block.header;
    header.merkle_root = merkle_root;
    block.header = Arc::new(header);

    Arc::new(block)
}

fn block_size(block: &block::Block) -> u32 {
    u32::try_from(
        block
            .zcash_serialize_to_vec()
            .expect("test block serializes")
            .len(),
    )
    .expect("test block size fits u32")
}

fn status() -> BlockSyncStatus {
    BlockSyncStatus {
        servable_low: block::Height(1),
        servable_high: block::Height(42),
        tip_hash: block::Hash([7; 32]),
        max_blocks_per_response: 16,
        max_inflight_requests: 4,
        max_response_bytes: MAX_BS_RESPONSE_BYTES,
    }
}

fn immediate_body_download_config() -> ZakuraBlockSyncConfig {
    ZakuraBlockSyncConfig {
        max_blocks_per_response: MAX_BS_BLOCKS_PER_REQUEST,
        near_tip_body_download_pause_blocks: 0,
        ..ZakuraBlockSyncConfig::default()
    }
}

fn test_frontier(height: u32) -> Frontier {
    let hash_byte = u8::try_from(height % 251).expect("height modulo 251 fits in u8");
    Frontier::new(block::Height(height), block::Hash([hash_byte; 32]))
}

fn test_frontier_update(
    finalized: u32,
    verified_body: u32,
    best_header: u32,
    change: FrontierChange,
) -> FrontierUpdate {
    FrontierUpdate {
        frontier: ChainFrontier {
            finalized: test_frontier(finalized),
            verified_body: test_frontier(verified_body),
            best_header: test_frontier(best_header),
        },
        change,
    }
}

fn exchange_block_sync_startup(
    initial: FrontierUpdate,
    config: ZakuraBlockSyncConfig,
) -> (ZakuraSyncExchange, BlockSyncStartup) {
    let exchange = ZakuraSyncExchange::new(initial, ZakuraTrace::noop());
    let frontier = initial.frontier;
    let startup = BlockSyncStartup::new_with_exchange(
        BlockSyncFrontiers {
            finalized_height: frontier.finalized.height,
            verified_block_tip: frontier.verified_body.height,
            verified_block_hash: frontier.verified_body.hash,
        },
        (frontier.best_header.height, frontier.best_header.hash),
        exchange.subscribe_frontier(),
        config,
    );

    (exchange, startup)
}

fn round_trip(message: BlockSyncMessage) {
    let encoded = message.encode().expect("message encodes");
    let decoded = BlockSyncMessage::decode(&encoded).expect("message decodes");

    assert_eq!(decoded, message);
}

async fn next_event(events: &mut mpsc::Receiver<BlockSyncEvent>) -> BlockSyncEvent {
    tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .expect("block-sync event should arrive")
        .expect("block-sync event channel should stay open")
}

async fn next_action(actions: &mut mpsc::Receiver<BlockSyncAction>) -> BlockSyncAction {
    tokio::time::timeout(Duration::from_secs(1), actions.recv())
        .await
        .expect("block-sync action should arrive")
        .expect("block-sync action channel should stay open")
}

async fn wait_for_query_needed_blocks(
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    verified_block_tip: block::Height,
    best_header_tip: block::Height,
) {
    loop {
        match next_action(actions).await {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: actual_verified,
                best_header_tip: actual_best,
            } if actual_verified == verified_block_tip && actual_best == best_header_tip => return,
            BlockSyncAction::QueryNeededBlocks { .. } | BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before target QueryNeededBlocks: {action:?}"),
        }
    }
}

async fn wait_for_getblocks(
    actions: &mut mpsc::Receiver<BlockSyncAction>,
) -> (ZakuraPeerId, block::Height, u32) {
    loop {
        match next_action(actions).await {
            BlockSyncAction::SendMessage {
                peer,
                msg:
                    BlockSyncMessage::GetBlocks {
                        start_height,
                        count,
                    },
            } => return (peer, start_height, count),
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::QueryNeededBlocks { .. } => {}
            BlockSyncAction::Misbehavior {
                reason: BlockSyncMisbehavior::UnsolicitedBlock,
                ..
            } => {}
            action => panic!("unexpected action before GetBlocks: {action:?}"),
        }
    }
}

async fn wait_for_connect_status(actions: &mut mpsc::Receiver<BlockSyncAction>) -> ZakuraPeerId {
    loop {
        match next_action(actions).await {
            BlockSyncAction::SendMessage {
                peer,
                msg: BlockSyncMessage::Status(_),
            } => return peer,
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::QueryNeededBlocks { .. } => {}
            BlockSyncAction::Misbehavior { .. } => {}
            action => panic!("unexpected action before connect status: {action:?}"),
        }
    }
}

async fn next_outbound_message(outbound: &mut FramedRecv) -> BlockSyncMessage {
    let frame = tokio::time::timeout(Duration::from_secs(1), outbound.recv())
        .await
        .expect("outbound frame arrives")
        .expect("outbound channel is live");
    BlockSyncMessage::decode_frame(frame).expect("outbound frame decodes")
}

async fn wait_for_outbound_block(outbound: &mut FramedRecv) -> Arc<block::Block> {
    loop {
        match next_outbound_message(outbound).await {
            BlockSyncMessage::Block(block) => return block,
            BlockSyncMessage::Status(_) | BlockSyncMessage::GetBlocks { .. } => {}
            msg => panic!("unexpected outbound message before block: {msg:?}"),
        }
    }
}

async fn wait_for_outbound_blocks_done(outbound: &mut FramedRecv) -> (block::Height, u32) {
    loop {
        match next_outbound_message(outbound).await {
            BlockSyncMessage::BlocksDone {
                start_height,
                returned,
            } => return (start_height, returned),
            BlockSyncMessage::Status(_) | BlockSyncMessage::GetBlocks { .. } => {}
            msg => panic!("unexpected outbound message before BlocksDone: {msg:?}"),
        }
    }
}

async fn wait_for_outbound_range_unavailable(outbound: &mut FramedRecv) -> (block::Height, u32) {
    loop {
        match next_outbound_message(outbound).await {
            BlockSyncMessage::RangeUnavailable {
                start_height,
                count,
            } => return (start_height, count),
            BlockSyncMessage::Status(_) | BlockSyncMessage::GetBlocks { .. } => {}
            msg => panic!("unexpected outbound message before RangeUnavailable: {msg:?}"),
        }
    }
}

async fn drain_parent_first_actions(
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    verified_tip: &mut block::Height,
    expected_new_fork: Option<&[Arc<block::Block>]>,
) {
    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(25), actions.recv()).await
    {
        match action {
            BlockSyncAction::SubmitBlock { block, .. } => {
                let height = block
                    .coinbase_height()
                    .expect("submitted test block has height");
                assert_eq!(
                    Some(height),
                    next_height(*verified_tip),
                    "block sync must submit only the contiguous parent-first prefix"
                );
                if let Some(new_fork) = expected_new_fork {
                    let expected_hash = match height.0 {
                        2 => new_fork[1].hash(),
                        3 => new_fork[2].hash(),
                        _ => panic!("unexpected post-reset submitted height: {height:?}"),
                    };
                    assert_eq!(
                        block.hash(),
                        expected_hash,
                        "post-reset submissions must follow the re-derived fork"
                    );
                }
                *verified_tip = height;
            }
            BlockSyncAction::Misbehavior {
                reason: BlockSyncMisbehavior::InvalidBlock | BlockSyncMisbehavior::UnsolicitedBlock,
                ..
            } => {}
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action while draining body responses: {action:?}"),
        }
    }
}

fn peer_state(byte: u8) -> (ZakuraPeerId, PeerBlockState) {
    let peer = peer(byte);
    let (_inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let session = PeerStreamSession::new(
        peer.clone(),
        ZAKURA_STREAM_BLOCK_SYNC,
        inbound_rx,
        outbound_tx,
        CancellationToken::new(),
    );
    let mut state = PeerBlockState::new(
        BlockSyncPeerSession::new(&session, ServicePeerDirection::Outbound),
        &ZakuraBlockSyncConfig::default(),
    );
    state.received_status = true;
    state.servable_high = block::Height(100);
    state.max_blocks_per_response = MAX_BS_BLOCKS_PER_REQUEST;
    (peer, state)
}

#[test]
fn peer_outbound_request_window_halves_on_timeout_and_grows_on_success() {
    let (_peer, mut state) = peer_state(44);
    state.max_inflight_requests = MAX_BS_INFLIGHT_REQUESTS;
    state.outbound_request_window = usize::from(MAX_BS_INFLIGHT_REQUESTS);
    state.outstanding.push(OutstandingBlockRange {
        request: BlockRangeRequest {
            start_height: block::Height(1),
            count: 1,
            anchor_hash: block::Hash([1; 32]),
            estimated_bytes: 1,
            expected_hashes: vec![(block::Height(1), block::Hash([1; 32]))],
            expected_bytes: vec![(block::Height(1), 1)],
        },
        deadline: Instant::now(),
        received: HashSet::new(),
    });
    assert_eq!(
        state.available_slots(),
        usize::from(MAX_BS_INFLIGHT_REQUESTS) - 1
    );

    state.reduce_outbound_window_after_timeout();
    assert_eq!(
        state.outbound_request_window,
        usize::from(MAX_BS_INFLIGHT_REQUESTS) / 2
    );

    for _ in 0..16 {
        state.reduce_outbound_window_after_timeout();
    }
    assert_eq!(state.outbound_request_window, 1);
    assert_eq!(state.available_slots(), state.timeout_recovery_slots);

    state.outstanding.clear();
    state.timeout_recovery_slots = 0;
    state.increase_outbound_window_after_success();
    assert_eq!(state.outbound_request_window, 2);

    state.outbound_request_window = usize::from(MAX_BS_INFLIGHT_REQUESTS);
    state.increase_outbound_window_after_success();
    assert_eq!(
        state.outbound_request_window,
        usize::from(MAX_BS_INFLIGHT_REQUESTS)
    );
}

#[test]
fn peer_timeout_recovery_slot_replaces_timed_out_request_above_reduced_window() {
    let (_peer, mut state) = peer_state(45);
    state.max_inflight_requests = 8;
    state.outbound_request_window = 8;

    for height in 1u32..=8 {
        let byte = u8::try_from(height).expect("test heights fit in u8");
        state.outstanding.push(OutstandingBlockRange {
            request: BlockRangeRequest {
                start_height: block::Height(height),
                count: 1,
                anchor_hash: block::Hash([byte; 32]),
                estimated_bytes: 1,
                expected_hashes: vec![(block::Height(height), block::Hash([byte; 32]))],
                expected_bytes: vec![(block::Height(height), 1)],
            },
            deadline: Instant::now(),
            received: HashSet::new(),
        });
    }

    assert_eq!(state.available_slots(), 0);

    state.reduce_outbound_window_after_timeout();
    assert_eq!(state.outbound_request_window, 4);
    assert_eq!(state.timeout_recovery_slots, 1);
    assert_eq!(
        state.available_slots(),
        0,
        "the advertised hard cap is still full until the timed-out request is removed"
    );

    state.outstanding.remove(0);
    assert_eq!(
        state.available_slots(),
        1,
        "a timeout recovery slot lets the retry replace the timed-out request"
    );

    state.record_outbound_request_scheduled();
    assert_eq!(state.timeout_recovery_slots, 0);
    state.outstanding.push(OutstandingBlockRange {
        request: BlockRangeRequest {
            start_height: block::Height(9),
            count: 1,
            anchor_hash: block::Hash([9; 32]),
            estimated_bytes: 1,
            expected_hashes: vec![(block::Height(9), block::Hash([9; 32]))],
            expected_bytes: vec![(block::Height(9), 1)],
        },
        deadline: Instant::now(),
        received: HashSet::new(),
    });
    assert_eq!(
        state.available_slots(),
        0,
        "regular scheduling remains held below the reduced adaptive window"
    );
}

#[test]
fn state_expires_due_timeouts_without_waiting_for_tick() {
    let startup = BlockSyncStartup::inert(ZakuraBlockSyncConfig::default());
    let mut state = BlockSyncState::new(&startup);
    let now = Instant::now();
    let request = BlockRangeRequest {
        start_height: block::Height(1),
        count: 1,
        anchor_hash: block::Hash([1; 32]),
        estimated_bytes: 100,
        expected_hashes: vec![(block::Height(1), block::Hash([1; 32]))],
        expected_bytes: vec![(block::Height(1), 100)],
    };

    let (timed_out_peer, mut timed_out_state) = peer_state(47);
    timed_out_state.max_blocks_per_response = 1;
    timed_out_state.max_inflight_requests = 512;
    timed_out_state.outstanding.push(OutstandingBlockRange {
        request: request.clone(),
        deadline: now - Duration::from_millis(1),
        received: HashSet::new(),
    });

    let (retry_peer, mut retry_state) = peer_state(48);
    retry_state.max_blocks_per_response = 1;
    retry_state.max_inflight_requests = 512;
    retry_state.servable_low = block::Height(1);
    retry_state.servable_high = block::Height(1);

    assert!(state.budget.try_reserve(request.estimated_bytes));
    state.peers.insert(timed_out_peer.clone(), timed_out_state);
    state.peers.insert(retry_peer.clone(), retry_state);

    assert!(
        state.expire_due_timeouts(now),
        "due request timeouts should be drained synchronously from hot paths",
    );
    assert_eq!(state.budget.reserved(), 0);
    assert!(state
        .peers
        .get(&timed_out_peer)
        .expect("timed-out peer remains connected")
        .outstanding
        .is_empty());

    let retry_peer_state = state
        .peers
        .get(&retry_peer)
        .expect("retry peer exists")
        .clone();
    let retry = state
        .schedule
        .next_for_peer(
            &retry_peer,
            &retry_peer_state,
            &mut state.budget,
            u64::MAX,
            1,
        )
        .expect("expired floor request should be immediately requestable");
    assert_eq!(retry.start_height, block::Height(1));
    assert_eq!(retry.count, 1);
}

#[test]
fn scheduler_retry_after_timeout_prefers_a_different_peer() {
    // Two separate needed ranges: the contiguous floor (h1) and unrelated work
    // (h5). Single-block requests keep each range one height.
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(100, 1);
    scheduler.refresh_needed(vec![
        needed(1, BlockSizeEstimate::Advertised(100)),
        needed(5, BlockSizeEstimate::Advertised(100)),
    ]);
    let (slow_peer, mut slow_state) = peer_state(47);
    slow_state.max_blocks_per_response = 1;
    let (healthy_peer, mut healthy_state) = peer_state(48);
    healthy_state.max_blocks_per_response = 1;
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 10);

    // The slow peer first downloads the floor (h1)...
    let floor = scheduler
        .next_for_peer(&slow_peer, &slow_state, &mut budget, u64::MAX, 1)
        .expect("floor range is assignable");
    assert_eq!(floor.start_height, block::Height(1));

    // ...then that request times out on it.
    budget.release(floor.estimated_bytes);
    scheduler.retry_after_timeout(floor, slow_peer.clone());

    // Asked again, the slow peer takes its *other* servable work (h5) rather
    // than immediately re-grabbing the floor it just timed out.
    let slow_next = scheduler
        .next_for_peer(&slow_peer, &slow_state, &mut budget, u64::MAX, 1)
        .expect("slow peer still has other assignable work");
    assert_eq!(
        slow_next.start_height,
        block::Height(5),
        "the slow peer must avoid re-grabbing the floor it just timed out",
    );

    // The floor is left for a different, healthy peer to pick up promptly.
    let healthy_next = scheduler
        .next_for_peer(&healthy_peer, &healthy_state, &mut budget, u64::MAX, 1)
        .expect("healthy peer can take the floor");
    assert_eq!(healthy_next.start_height, block::Height(1));
}

#[test]
fn scheduler_retry_after_timeout_falls_back_to_sole_servable_peer() {
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(100, 1);
    scheduler.refresh_needed(vec![needed(1, BlockSizeEstimate::Advertised(100))]);
    let (slow_peer, mut slow_state) = peer_state(47);
    slow_state.max_blocks_per_response = 1;
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 10);

    let floor = scheduler
        .next_for_peer(&slow_peer, &slow_state, &mut budget, u64::MAX, 1)
        .expect("floor range is assignable");
    budget.release(floor.estimated_bytes);
    scheduler.retry_after_timeout(floor, slow_peer.clone());

    // With no other servable peer and no other work, the slow peer must still
    // retry the floor — the bias is a preference, not a hard exclusion, so a
    // single servable peer never stalls.
    let retry = scheduler
        .next_for_peer(&slow_peer, &slow_state, &mut budget, u64::MAX, 1)
        .expect("sole servable peer falls back to the avoided floor");
    assert_eq!(retry.start_height, block::Height(1));
}

async fn connect_peer_with_status(
    service: &BlockSyncService,
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    byte: u8,
    servable_high: block::Height,
    tip_hash: block::Hash,
    max_inflight_requests: u16,
    max_response_bytes: u32,
) -> (ZakuraPeerId, FramedSend, FramedRecv) {
    connect_peer_with_status_message(
        service,
        actions,
        byte,
        BlockSyncStatus {
            servable_low: block::Height(1),
            servable_high,
            tip_hash,
            max_blocks_per_response: 16,
            max_inflight_requests,
            max_response_bytes,
        },
    )
    .await
}

async fn connect_peer_with_status_message(
    service: &BlockSyncService,
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    byte: u8,
    status: BlockSyncStatus,
) -> (ZakuraPeerId, FramedSend, FramedRecv) {
    let peer = peer(byte);
    let (inbound_tx, inbound_rx) = framed_channel(16);
    let (outbound_tx, outbound_rx) = framed_channel(16);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(actions).await, peer);
    inbound_tx
        .send(
            BlockSyncMessage::Status(status)
                .encode_frame()
                .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    (peer, inbound_tx, outbound_rx)
}

fn needed(height: u32, size: BlockSizeEstimate) -> NeededBlock {
    NeededBlock {
        height: block::Height(height),
        hash: block::Hash([height as u8; 32]),
        size,
    }
}

fn block_meta(block: &Arc<block::Block>) -> BlockSyncBlockMeta {
    BlockSyncBlockMeta {
        height: block.coinbase_height().expect("test block has height"),
        hash: block.hash(),
        size: BlockSizeEstimate::Advertised(block_size(block)),
    }
}

#[test]
fn block_sync_near_tip_pause_config_defaults_and_round_trips() {
    let default = ZakuraBlockSyncConfig::default();
    assert_eq!(default.max_blocks_per_response, 1);
    assert_eq!(default.max_inflight_requests, 512);
    assert_eq!(default.near_tip_body_download_pause_blocks, 2);
    assert_eq!(
        default.max_submitted_block_applies,
        DEFAULT_BS_MAX_SUBMITTED_BLOCK_APPLIES
    );
    assert_eq!(default.request_timeout, DEFAULT_BS_REQUEST_TIMEOUT);
    assert_eq!(default.fanout, DEFAULT_BS_FANOUT);

    let encoded = toml::to_string(&default).expect("block-sync config serializes");
    let decoded: ZakuraBlockSyncConfig =
        toml::from_str(&encoded).expect("block-sync config deserializes");
    assert_eq!(decoded, default);

    let config: crate::Config = toml::from_str(
        r#"
        [zakura.block_sync]
        near_tip_body_download_pause_blocks = 7
        max_submitted_block_applies = 9
        "#,
    )
    .expect("nested Zakura block-sync config deserializes");
    assert_eq!(
        config.zakura.block_sync.near_tip_body_download_pause_blocks,
        7
    );
    assert_eq!(config.zakura.block_sync.max_submitted_block_applies, 9);
}

#[test]
fn codec_round_trips_every_message_variant() {
    round_trip(BlockSyncMessage::Status(status()));
    round_trip(BlockSyncMessage::GetBlocks {
        start_height: block::Height(10),
        count: 3,
    });
    round_trip(BlockSyncMessage::Block(mainnet_block(
        &BLOCK_MAINNET_1_BYTES,
    )));
    round_trip(BlockSyncMessage::BlocksDone {
        start_height: block::Height(10),
        returned: 3,
    });
    round_trip(BlockSyncMessage::RangeUnavailable {
        start_height: block::Height(10),
        count: 3,
    });
}

#[test]
fn codec_round_trips_block_near_max_block_bytes() {
    let block = Arc::new(zebra_chain::block::tests::generate::large_multi_transaction_block());
    let serialized_len = block
        .zcash_serialize_to_vec()
        .expect("large test block serializes")
        .len();
    let max_block_bytes =
        usize::try_from(block::MAX_BLOCK_BYTES).expect("max block size fits in usize");

    assert!(
        serialized_len <= max_block_bytes && serialized_len > max_block_bytes - 1000,
        "test block should be close to the consensus cap, got {serialized_len}"
    );
    round_trip(BlockSyncMessage::Block(block));
}

#[test]
fn codec_rejects_malformed_discriminator_and_truncated_payload() {
    assert!(matches!(
        BlockSyncMessage::decode(&[99]),
        Err(BlockSyncWireError::UnknownMessageType(99))
    ));

    assert!(matches!(
        BlockSyncMessage::decode(&[MSG_BS_GET_BLOCKS, 1, 0]),
        Err(BlockSyncWireError::Io(_))
    ));
}

#[test]
fn codec_classifies_payloads_above_old_raw_stream6_cap() {
    let old_max_bs_message_bytes =
        usize::try_from(block::MAX_BLOCK_BYTES).expect("max block bytes fits in usize") + 1;
    let payload = vec![99; old_max_bs_message_bytes + 1];

    assert!(payload.len() <= MAX_BS_MESSAGE_BYTES);
    assert!(matches!(
        BlockSyncMessage::decode(&payload),
        Err(BlockSyncWireError::UnknownMessageType(99))
    ));
}

#[test]
fn codec_rejects_oversized_frame_and_oversized_block() {
    let oversized_payload = vec![0; MAX_BS_MESSAGE_BYTES + 1];
    assert!(matches!(
        BlockSyncMessage::decode(&oversized_payload),
        Err(BlockSyncWireError::OversizedPayload { .. })
    ));

    let oversized_block =
        Arc::new(zebra_chain::block::tests::generate::oversized_multi_transaction_block());
    assert!(matches!(
        BlockSyncMessage::Block(oversized_block).encode(),
        Err(BlockSyncWireError::OversizedBlock { .. })
            | Err(BlockSyncWireError::OversizedPayload { .. })
    ));
}

#[test]
fn codec_rejects_count_and_returned_over_cap() {
    let over_cap = MAX_BS_BLOCKS_PER_REQUEST + 1;

    assert!(matches!(
        BlockSyncMessage::BlocksDone {
            start_height: block::Height(1),
            returned: 0,
        }
        .encode(),
        Err(BlockSyncWireError::ZeroBlockCount)
    ));

    let mut zero_count_get_blocks = vec![MSG_BS_GET_BLOCKS];
    zero_count_get_blocks.extend_from_slice(&1u32.to_le_bytes());
    zero_count_get_blocks.extend_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        BlockSyncMessage::decode(&zero_count_get_blocks),
        Err(BlockSyncWireError::ZeroBlockCount)
    ));

    let mut zero_count_range_unavailable = vec![MSG_BS_RANGE_UNAVAILABLE];
    zero_count_range_unavailable.extend_from_slice(&1u32.to_le_bytes());
    zero_count_range_unavailable.extend_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        BlockSyncMessage::decode(&zero_count_range_unavailable),
        Err(BlockSyncWireError::ZeroBlockCount)
    ));

    assert!(matches!(
        BlockSyncMessage::GetBlocks {
            start_height: block::Height(1),
            count: over_cap,
        }
        .encode(),
        Err(BlockSyncWireError::BlockCountLimit { .. })
    ));

    assert!(matches!(
        BlockSyncMessage::BlocksDone {
            start_height: block::Height(1),
            returned: over_cap,
        }
        .encode(),
        Err(BlockSyncWireError::BlockCountLimit { .. })
    ));

    assert!(matches!(
        BlockSyncMessage::RangeUnavailable {
            start_height: block::Height(1),
            count: over_cap,
        }
        .encode(),
        Err(BlockSyncWireError::BlockCountLimit { .. })
    ));
}

#[test]
fn frame_decode_rejects_mismatched_unknown_flags_and_trailing_payload() {
    let frame = Frame {
        message_type: u16::from(MSG_BS_GET_BLOCKS),
        flags: 1,
        payload: BlockSyncMessage::GetBlocks {
            start_height: block::Height(1),
            count: 1,
        }
        .encode()
        .expect("message encodes"),
    };
    assert!(matches!(
        BlockSyncMessage::decode_frame(frame),
        Err(BlockSyncWireError::UnsupportedFlags(1))
    ));

    let mut payload = BlockSyncMessage::Status(status())
        .encode()
        .expect("message encodes");
    payload.push(0);
    assert!(matches!(
        BlockSyncMessage::decode(&payload),
        Err(BlockSyncWireError::TrailingBytes)
    ));

    let frame = Frame {
        message_type: u16::from(MSG_BS_BLOCK),
        flags: 0,
        payload: BlockSyncMessage::Status(status())
            .encode()
            .expect("message encodes"),
    };
    assert!(matches!(
        BlockSyncMessage::decode_frame(frame),
        Err(BlockSyncWireError::MismatchedFrameMessageType { .. })
    ));
}

#[test]
fn status_decode_clamps_peer_capacity_advertisements() {
    let mut payload = Vec::new();
    payload.push(MSG_BS_STATUS);
    payload.extend_from_slice(&block::Height(1).0.to_le_bytes());
    payload.extend_from_slice(&block::Height(2).0.to_le_bytes());
    block::Hash([9; 32])
        .zcash_serialize(&mut payload)
        .expect("hash serializes");
    payload.extend_from_slice(&u32::MAX.to_le_bytes());
    payload.extend_from_slice(&u16::MAX.to_le_bytes());
    payload.extend_from_slice(&u32::MAX.to_le_bytes());

    let BlockSyncMessage::Status(status) =
        BlockSyncMessage::decode(&payload).expect("status decodes")
    else {
        panic!("expected status message");
    };

    assert_eq!(status.max_blocks_per_response, MAX_BS_BLOCKS_PER_REQUEST);
    assert_eq!(status.max_inflight_requests, MAX_BS_INFLIGHT_REQUESTS);
    assert_eq!(status.max_response_bytes, MAX_BS_RESPONSE_BYTES);
}

#[test]
fn aggregate_response_cap_is_not_the_per_frame_cap() {
    assert!(
        MAX_BS_RESPONSE_BYTES > u32::try_from(MAX_BS_MESSAGE_BYTES).expect("frame cap fits u32"),
        "range responses are multiple independently-capped block frames"
    );
    assert_eq!(
        ZakuraBlockSyncConfig::default().advertised_max_response_bytes(),
        MAX_BS_RESPONSE_BYTES
    );
}

#[test]
fn scheduler_assigns_needed_ranges_with_fanout_slots_and_dedup() {
    let mut scheduler = BlockRangeScheduler::new(2);
    scheduler.refresh_needed(vec![
        needed(1, BlockSizeEstimate::Advertised(10_000)),
        needed(2, BlockSizeEstimate::Advertised(10_000)),
        needed(3, BlockSizeEstimate::Advertised(10_000)),
    ]);
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);
    let (peer1, state1) = peer_state(31);
    let (peer2, state2) = peer_state(32);
    let (peer3, state3) = peer_state(33);

    let first = scheduler
        .next_for_peer(
            &peer1,
            &state1,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("first peer gets the range");
    assert_eq!(first.start_height, block::Height(1));
    assert_eq!(first.count, 3);

    let second = scheduler
        .next_for_peer(
            &peer2,
            &state2,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("fanout allows a second peer");
    assert_eq!(second.start_height, block::Height(1));
    assert_eq!(second.count, 3);

    assert!(
        scheduler
            .next_for_peer(
                &peer1,
                &state1,
                &mut budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "same peer must not receive overlapping duplicate assignment"
    );
    assert!(
        scheduler
            .next_for_peer(
                &peer3,
                &state3,
                &mut budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "fanout cap must deduplicate the covered range"
    );
}

#[test]
fn scheduler_byte_budget_sizing_shrinks_or_defers_requests() {
    // Block sync reserves worst-case bytes per requested block, so request sizing
    // is in units of `BS_PER_BLOCK_WORST_CASE_BYTES`, not advertised size hints.
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer, mut state) = peer_state(34);
    // A response cap between one and two worst-case shares admits exactly one.
    state.max_response_bytes = u32::try_from(worst + worst / 2).expect("fits in u32");
    state.max_blocks_per_response = 10;
    let mut budget = ByteBudget::new(worst * 10);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("one block fits the peer response-byte cap");
    assert_eq!(request.start_height, block::Height(10));
    assert_eq!(request.count, 1);
    assert_eq!(budget.reserved(), worst);

    scheduler.complete(&request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    // A budget below one worst-case share cannot fit even the first block.
    let mut small_budget = ByteBudget::new(worst - 1);
    assert!(
        scheduler
            .next_for_peer(
                &peer,
                &state,
                &mut small_budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "a first block that does not fit is deferred"
    );
    assert_eq!(small_budget.reserved(), 0);
}

#[test]
fn scheduler_per_peer_byte_cap_limits_request_below_global_budget() {
    // Per-peer byte caps bind in worst-case units: each requested block reserves
    // `BS_PER_BLOCK_WORST_CASE_BYTES` regardless of its advertised size hint.
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(1, BlockSizeEstimate::Advertised(100)),
        needed(2, BlockSizeEstimate::Advertised(100)),
        needed(3, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer, mut state) = peer_state(50);
    state.max_response_bytes = u32::try_from(worst * 10).unwrap_or(u32::MAX); // not binding
    state.max_blocks_per_response = 10; // count cap not binding
    let mut budget = ByteBudget::new(worst * 100); // global budget ample

    // A per-peer cap between one and two worst-case shares admits only the first
    // block, even though the count cap, peer response cap, and global budget all
    // have ample room.
    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            worst + worst / 2,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("first block fits the per-peer byte cap");
    assert_eq!(request.count, 1);
    assert_eq!(request.estimated_bytes, worst);
    assert_eq!(budget.reserved(), worst);

    // A fresh peer with a cap covering three worst-case shares batches the whole
    // three-block range.
    let mut scheduler2 = BlockRangeScheduler::new(1);
    scheduler2.set_estimator_for_tests(750, 1);
    scheduler2.refresh_needed(vec![
        needed(1, BlockSizeEstimate::Advertised(100)),
        needed(2, BlockSizeEstimate::Advertised(100)),
        needed(3, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer2, mut state2) = peer_state(51);
    state2.max_response_bytes = u32::try_from(worst * 10).unwrap_or(u32::MAX);
    state2.max_blocks_per_response = 10;
    let mut budget2 = ByteBudget::new(worst * 100);
    let request2 = scheduler2
        .next_for_peer(
            &peer2,
            &state2,
            &mut budget2,
            worst * 3,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("higher per-peer cap admits the whole range");
    assert_eq!(request2.count, 3);
    assert_eq!(request2.estimated_bytes, worst * 3);
}

#[test]
fn block_sync_per_peer_byte_cap_shares_budget_and_floors_at_one_response() {
    // Ample budget: each of `expected_peers` gets an even share of the budget.
    let config = immediate_body_download_config();
    assert_eq!(
        config.per_peer_byte_cap(),
        config.max_inflight_block_bytes / config.expected_peers as u64,
    );

    // Tiny budget: the per-peer share would starve peers, so the cap floors at
    // one advertised response so a peer can always make progress.
    let tiny = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: 4_096,
        ..ZakuraBlockSyncConfig::default()
    };
    assert_eq!(
        tiny.per_peer_byte_cap(),
        u64::from(tiny.advertised_max_response_bytes()),
    );

    // `expected_peers == 0` disables per-peer byte fairness entirely.
    let disabled = ZakuraBlockSyncConfig {
        expected_peers: 0,
        ..ZakuraBlockSyncConfig::default()
    };
    assert_eq!(disabled.per_peer_byte_cap(), u64::MAX);
}

#[tokio::test]
async fn reactor_fill_loop_saturates_multiple_slots_in_one_pass() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Peer serves heights 1..=4 and accepts four concurrent single-block requests.
    let (peer_id, _inbound, _outbound) = connect_peer_with_status_message(
        &service,
        &mut actions,
        41,
        BlockSyncStatus {
            servable_low: block::Height(1),
            servable_high: block::Height(4),
            tip_hash: block::Hash([4; 32]),
            max_blocks_per_response: 1,
            max_inflight_requests: 4,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        },
    )
    .await;

    tip_tx
        .send((block::Height(4), block::Hash([4; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block::Hash([1; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block::Hash([2; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(3),
                hash: block::Hash([3; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(4),
                hash: block::Hash([4; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
        ]))
        .await
        .expect("needed metadata queues");

    // The fill-loop opens all four slots from the single NeededBlocks event.
    // Pre-fill-loop scheduling issued only one GetBlocks per scheduling event,
    // so this would time out on the second request.
    let mut heights = Vec::new();
    for _ in 0..4 {
        let (peer, start_height, count) = wait_for_getblocks(&mut actions).await;
        assert_eq!(peer, peer_id);
        assert_eq!(count, 1);
        heights.push(start_height.0);
    }
    heights.sort_unstable();
    assert_eq!(heights, vec![1, 2, 3, 4]);

    reactor_task.abort();
}

/// Every connected peer that advertises an in-flight window must have that
/// window filled in one scheduling pass when there is enough needed work to go
/// around — not just the first peer.
///
/// This is the multi-peer complement to
/// `reactor_fill_loop_saturates_multiple_slots_in_one_pass`: with three peers
/// each advertising four concurrent single-block slots and twelve needed
/// heights, the fill loop must fan out four requests to *each* peer rather than
/// pouring all twelve into the lowest-id peer (or stopping after one peer's
/// window). A regression here is what makes download budget collect on a single
/// peer while the rest sit idle with free slots.
#[tokio::test]
async fn reactor_fill_loop_saturates_every_peer_window_not_just_one() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Three peers, each willing to serve heights 1..=12 and accept four
    // concurrent single-block requests. The budget is ample, so the fill order
    // (rotated per pass) does not matter here: every peer's window is saturated
    // in the single pass regardless of which peer the rotation starts at.
    let mut peer_ids = Vec::new();
    // Keep every peer's stream handles alive for the whole test: dropping them
    // closes the channels and tears the peer down before it can serve.
    let mut peer_streams = Vec::new();
    for byte in [41u8, 42, 43] {
        let (peer_id, inbound, outbound) = connect_peer_with_status_message(
            &service,
            &mut actions,
            byte,
            BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(12),
                tip_hash: block::Hash([12; 32]),
                max_blocks_per_response: 1,
                max_inflight_requests: 4,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            },
        )
        .await;
        peer_ids.push(peer_id);
        peer_streams.push((inbound, outbound));
    }

    tip_tx
        .send((block::Height(12), block::Hash([12; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(
            (1..=12)
                .map(|height| BlockSyncBlockMeta {
                    height: block::Height(height),
                    hash: block::Hash([height as u8; 32]),
                    size: BlockSizeEstimate::Advertised(1_000),
                })
                .collect(),
        ))
        .await
        .expect("needed metadata queues");

    // Collect all twelve requests and tally them per peer. With fanout=1 each
    // height is assignable to exactly one peer, so a correct fill loop hands
    // four distinct heights to each of the three peers.
    let mut per_peer: HashMap<ZakuraPeerId, Vec<u32>> = HashMap::new();
    for _ in 0..12 {
        let (peer, start_height, count) = wait_for_getblocks(&mut actions).await;
        assert_eq!(count, 1);
        per_peer.entry(peer).or_default().push(start_height.0);
    }

    assert_eq!(
        per_peer.len(),
        3,
        "all three peers must receive requests, not just the first; got {per_peer:?}"
    );
    for peer_id in &peer_ids {
        let issued = per_peer
            .get(peer_id)
            .unwrap_or_else(|| panic!("peer {peer_id:?} received no requests: {per_peer:?}"));
        assert_eq!(
            issued.len(),
            4,
            "peer {peer_id:?} window of 4 was not saturated: {per_peer:?}"
        );
    }

    let mut all_heights: Vec<u32> = per_peer.values().flatten().copied().collect();
    all_heights.sort_unstable();
    assert_eq!(
        all_heights,
        (1..=12).collect::<Vec<_>>(),
        "every needed height must be requested exactly once across peers"
    );

    reactor_task.abort();
}

/// Under a budget that only covers one in-flight request at a time, issuance
/// must rotate across the status-ready peers rather than always pouring the
/// single budgeted request into the lowest-node-id peer.
///
/// Before peer-scoped issuance the full pass iterated peers in static sorted
/// (node-id) order, so a budget-constrained pass always handed its one request
/// to the lowest-id peer; the higher-id peers were perpetually starved. The
/// rotating full-pass cursor makes the starting peer advance once per pass, so
/// across several budget-limited passes the served peer is distributed instead
/// of monopolized. This is an invariant assertion (issuance is not pinned to one
/// peer), not an exact-order assertion, since the per-peer order is intentionally
/// nondeterministic relative to node id.
#[tokio::test]
async fn reactor_budget_constrained_issuance_rotates_across_peers() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Three peers, each able to serve height 1 with a single in-flight slot.
    // Distinct ascending id bytes give the old sorted order a fixed lowest peer
    // (0x41) that a regression would always pick first.
    let mut peer_inbounds = HashMap::new();
    // Keep every peer's outbound handle alive too: dropping it closes the channel
    // and tears the peer down before it can be offered work.
    let mut peer_outbounds = Vec::new();
    for byte in [0x41u8, 0x42, 0x43] {
        let (peer_id, inbound, outbound) = connect_peer_with_status_message(
            &service,
            &mut actions,
            byte,
            BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block::Hash([1; 32]),
                max_blocks_per_response: 1,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            },
        )
        .await;
        peer_inbounds.insert(peer_id, inbound);
        peer_outbounds.push(outbound);
    }

    tip_tx
        .send((block::Height(1), block::Hash([1; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block::Hash([1; 32]),
            size: BlockSizeEstimate::Advertised(1_000),
        }]))
        .await
        .expect("needed metadata queues");

    // A single contested height with fanout=1 can be assigned to exactly one
    // peer at a time. Each pass offers it to the rotation-start peer; that peer
    // answers `RangeUnavailable`, which re-queues the range and runs another full
    // pass. The rotating cursor advances once per pass, so over several passes
    // the offered peer rotates instead of pinning to the lowest-id peer (which is
    // what the old static sorted order did).
    let range_unavailable = BlockSyncMessage::RangeUnavailable {
        start_height: block::Height(1),
        count: 1,
    }
    .encode_frame()
    .expect("RangeUnavailable frame encodes");

    let mut served_peers = Vec::new();
    for _ in 0..6 {
        let (peer, start_height, count) = wait_for_getblocks(&mut actions).await;
        assert_eq!(start_height, block::Height(1));
        assert_eq!(count, 1, "fanout=1 yields single-block requests");
        served_peers.push(peer.clone());
        peer_inbounds
            .get(&peer)
            .expect("served peer is one of the connected peers")
            .send(range_unavailable.clone())
            .await
            .expect("RangeUnavailable frame queues");
    }

    let distinct: HashSet<_> = served_peers.iter().cloned().collect();
    assert!(
        distinct.len() >= 2,
        "budget-constrained issuance must rotate across peers, not pin to the \
         lowest-id peer; offered sequence was {served_peers:?}"
    );

    reactor_task.abort();
}

/// One peer whose request times out backs off (its outbound window halves) and
/// must not block the other peers from being filled out of the same shared work.
///
/// This is the timeout-locality invariant: a slow peer's recovery is local to
/// that peer. The retry path re-queues the timed-out range to a *different*
/// servable peer, so the healthy peer keeps making progress while the slow peer
/// is in recovery rather than the whole download stalling behind one straggler.
#[tokio::test]
async fn reactor_timeout_backoff_is_local_and_healthy_peer_keeps_filling() {
    let mut config = immediate_body_download_config();
    config.fanout = 1;
    // A request timeout long enough that the opening pass fans both heights out
    // before anything expires, but short enough that the slow peer's unanswered
    // request still times out within the test window.
    config.request_timeout = Duration::from_millis(400);
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES * 64;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Two peers, both able to serve heights 1..=2 with a single in-flight slot.
    // One will be left holding an unanswered request (the slow peer); the other
    // answers and must keep being filled from the same shared work — including
    // the slow peer's range once it times out and re-queues.
    let status = || BlockSyncStatus {
        servable_low: block::Height(1),
        servable_high: block::Height(2),
        tip_hash: block::Hash([2; 32]),
        max_blocks_per_response: 1,
        max_inflight_requests: 1,
        max_response_bytes: MAX_BS_RESPONSE_BYTES,
    };
    let (peer_a, _a_in, _a_out) =
        connect_peer_with_status_message(&service, &mut actions, 0x41, status()).await;
    let (peer_b, _b_in, _b_out) =
        connect_peer_with_status_message(&service, &mut actions, 0x42, status()).await;
    let mut inbounds = HashMap::from([(peer_a.clone(), _a_in), (peer_b.clone(), _b_in)]);

    tip_tx
        .send((block::Height(2), block::Hash([2; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block::Hash([1; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block::Hash([2; 32]),
                size: BlockSizeEstimate::Advertised(1_000),
            },
        ]))
        .await
        .expect("needed metadata queues");

    // Opening pass: with fanout=1 each peer is offered one of the two heights.
    let first = wait_for_getblocks(&mut actions).await;
    let second = wait_for_getblocks(&mut actions).await;
    let mut offered: HashMap<ZakuraPeerId, block::Height> = HashMap::new();
    offered.insert(first.0.clone(), first.1);
    offered.insert(second.0.clone(), second.1);
    assert_eq!(
        offered.len(),
        2,
        "both peers must be offered a height in the opening pass: {offered:?}"
    );

    // Pick one peer to be the straggler (it never answers) and the other to be
    // healthy. The healthy peer answers `RangeUnavailable` for its own range so
    // it frees its slot without committing anything; the straggler's range then
    // times out and re-queues. Because the timeout backoff is local to the
    // straggler, the healthy peer must keep being offered the re-queued shared
    // work rather than the whole download stalling behind the straggler.
    let healthy = peer_b.clone();
    let healthy_in = inbounds.remove(&healthy).expect("healthy peer inbound");

    let healthy_offers = tokio::time::timeout(Duration::from_secs(3), async {
        let mut count = 0usize;
        loop {
            // Whenever the healthy peer is offered a range, free its slot with a
            // `RangeUnavailable` so it can be offered the next range; the slow
            // peer never answers and stays in timeout recovery.
            let (peer, start_height, count_blocks) = wait_for_getblocks(&mut actions).await;
            if peer == healthy {
                count += 1;
                if count >= 2 {
                    break count;
                }
                healthy_in
                    .send(
                        BlockSyncMessage::RangeUnavailable {
                            start_height,
                            count: count_blocks,
                        }
                        .encode_frame()
                        .expect("RangeUnavailable frame encodes"),
                    )
                    .await
                    .expect("RangeUnavailable frame queues");
            }
        }
    })
    .await
    .expect(
        "the healthy peer must keep being offered shared work while the slow peer \
         is in timeout recovery; a stall here means a straggler wedged the pass",
    );
    assert!(
        healthy_offers >= 2,
        "the healthy peer was filled repeatedly despite the slow peer's timeout backoff"
    );

    reactor_task.abort();
}

#[test]
fn scheduler_partial_requests_clear_the_issued_assignment_key() {
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer, mut state) = peer_state(38);
    // A response cap between two and three worst-case shares admits two blocks.
    state.max_response_bytes =
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2 + BS_PER_BLOCK_WORST_CASE_BYTES / 2)
            .unwrap_or(u32::MAX);
    state.max_blocks_per_response = 10;
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("response-byte cap drains a prefix of the queued range");
    assert_eq!(request.start_height, block::Height(10));
    assert_eq!(request.count, 2);
    assert_eq!(scheduler.assigned_range_count(), 1);

    scheduler.complete(&request, &mut budget);
    assert_eq!(
        scheduler.assigned_range_count(),
        0,
        "completing a partial request must clear the same range key it assigned"
    );
}

#[test]
fn scheduler_drops_verified_prefix_from_queued_ranges() {
    let (peer1, state1) = peer_state(39);
    let (peer2, state2) = peer_state(40);
    let mut scheduler = BlockRangeScheduler::new(2);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
    ]);
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let first = scheduler
        .next_for_peer(
            &peer1,
            &state1,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("first fanout assignment queues");
    assert_eq!(first.start_height, block::Height(10));

    scheduler.drop_through(block::Height(10));
    assert!(
        scheduler
            .next_for_peer(
                &peer1,
                &state1,
                &mut budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "verified-prefix trimming must not let the same peer bypass its overlapping assignment",
    );

    let second = scheduler
        .next_for_peer(
            &peer2,
            &state2,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("queued suffix remains requestable by another fanout peer");
    assert_eq!(second.start_height, block::Height(11));
    assert_eq!(second.count, 2);
}

#[test]
fn scheduler_refresh_splits_around_assigned_and_queued_ranges() {
    let (peer, mut state) = peer_state(52);
    state.max_blocks_per_response = 2;
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(
        (1..=6)
            .map(|height| needed(height, BlockSizeEstimate::Advertised(100)))
            .collect(),
    );
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let first = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("first request assigns the range prefix");
    assert_eq!(first.start_height, block::Height(1));
    assert_eq!(first.count, 2);
    assert_eq!(scheduler.queued_block_count(), 4);

    scheduler.refresh_needed(
        (1..=10)
            .map(|height| needed(height, BlockSizeEstimate::Advertised(100)))
            .collect(),
    );

    assert_eq!(
        scheduler.queued_block_count(),
        8,
        "refresh must queue newly-needed suffix blocks instead of dropping the whole range"
    );
}

#[test]
fn scheduler_drops_covered_prefix_from_partially_queued_range() {
    let mut scheduler = BlockRangeScheduler::new(2);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer1, state1) = peer_state(40);
    let (peer2, state2) = peer_state(41);
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let first = scheduler
        .next_for_peer(
            &peer1,
            &state1,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("first fanout assignment leaves the queued range for another peer");
    assert_eq!(first.start_height, block::Height(10));
    assert_eq!(first.count, 3);

    scheduler.mark_height_covered(block::Height(10));

    assert!(
        scheduler
            .next_for_peer(
                &peer1,
                &state1,
                &mut budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "trimming a covered prefix must not let the same peer bypass its overlapping assignment",
    );

    let second = scheduler
        .next_for_peer(
            &peer2,
            &state2,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("uncovered suffix remains requestable");
    assert_eq!(
        second.start_height,
        block::Height(11),
        "covered prefix must not remain as the next request anchor",
    );
    assert_eq!(second.count, 2);
}

#[test]
fn scheduler_splits_queued_range_around_covered_heights() {
    let (peer, mut state) = peer_state(42);
    state.max_blocks_per_response = 10;
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
        needed(13, BlockSizeEstimate::Advertised(100)),
        needed(14, BlockSizeEstimate::Advertised(100)),
    ]);
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    scheduler.mark_height_covered(block::Height(12));

    let first = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("uncovered prefix remains requestable");
    assert_eq!(first.start_height, block::Height(10));
    assert_eq!(first.count, 2);

    let second = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("uncovered suffix remains requestable");
    assert_eq!(second.start_height, block::Height(13));
    assert_eq!(second.count, 2);
}

#[test]
fn scheduler_retries_only_uncovered_suffix() {
    let (peer, state) = peer_state(43);
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(20, BlockSizeEstimate::Advertised(100)),
        needed(21, BlockSizeEstimate::Advertised(100)),
        needed(22, BlockSizeEstimate::Advertised(100)),
    ]);
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("range fits");
    assert_eq!(request.start_height, block::Height(20));
    assert_eq!(request.count, 3);

    scheduler.mark_height_covered(block::Height(20));
    scheduler.timeout(request, &mut budget);

    let retry = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("uncovered retry suffix remains requestable");
    assert_eq!(
        retry.start_height,
        block::Height(21),
        "covered retry prefix must not be requested again",
    );
    assert_eq!(retry.count, 2);
}

#[test]
fn scheduler_keeps_queued_retries_and_missing_ranges_ordered_by_height() {
    let (peer, mut state) = peer_state(46);
    state.max_blocks_per_response = 10;
    let mut budget = ByteBudget::new(BS_PER_BLOCK_WORST_CASE_BYTES * 100);

    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![needed(20, BlockSizeEstimate::Advertised(100))]);
    scheduler.refresh_needed(vec![needed(10, BlockSizeEstimate::Advertised(100))]);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("lowest missing range is requestable");
    assert_eq!(
        request.start_height,
        block::Height(10),
        "new lower missing work must not sit behind later queued work",
    );

    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![needed(20, BlockSizeEstimate::Advertised(100))]);
    scheduler.retry(BlockRangeRequest {
        start_height: block::Height(10),
        count: 2,
        anchor_hash: block::Hash([10; 32]),
        estimated_bytes: 200,
        expected_hashes: vec![
            (block::Height(10), block::Hash([10; 32])),
            (block::Height(11), block::Hash([11; 32])),
        ],
        expected_bytes: vec![(block::Height(10), 100), (block::Height(11), 100)],
    });

    let retry = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("lowest retried range is requestable");
    assert_eq!(
        retry.start_height,
        block::Height(10),
        "timed-out lower work must not sit behind later queued work",
    );
}

#[test]
fn scheduler_releases_budget_on_completion_timeout_and_cancel() {
    // Each single-block request reserves one worst-case share regardless of the
    // advertised size hint; completion/timeout/cancel must release it exactly.
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let (peer, state) = peer_state(35);
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![needed(20, BlockSizeEstimate::Advertised(1_000))]);
    let mut budget = ByteBudget::new(worst * 10);
    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("range fits");
    assert_eq!(budget.reserved(), worst);
    scheduler.complete(&request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    scheduler.refresh_needed(vec![needed(21, BlockSizeEstimate::Advertised(2_000))]);
    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("range fits");
    scheduler.timeout(request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("retried range fits");
    assert_eq!(budget.reserved(), worst);
    assert_eq!(request.count, 1);
    scheduler.release_cancelled(&mut budget);
    assert_eq!(budget.reserved(), 0);
}

#[test]
fn scheduler_drops_queued_ranges_whose_anchor_left_current_header_spine() {
    let (peer, state) = peer_state(37);
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(40, BlockSizeEstimate::Advertised(1_000)),
        needed(41, BlockSizeEstimate::Advertised(1_000)),
    ]);

    let current = HashMap::from([
        (block::Height(40), block::Hash([90; 32])),
        (block::Height(41), block::Hash([91; 32])),
    ]);
    scheduler.retain_matching_needed(&current);

    let mut budget = ByteBudget::new(10_000);
    assert!(
        scheduler
            .next_for_peer(
                &peer,
                &state,
                &mut budget,
                u64::MAX,
                MAX_BS_BLOCKS_PER_REQUEST
            )
            .is_err(),
        "stale queued anchors must not survive a re-derived needed set"
    );
    assert_eq!(budget.reserved(), 0);
}

#[test]
fn scheduler_reserves_worst_case_regardless_of_size_hints() {
    // The byte reservation is worst-case per block; EWMA/Advertised/Confirmed size
    // hints no longer size the reservation (they still feed `expected_bytes` for
    // size-deviation scoring). A three-block range reserves three worst-case
    // shares regardless of how each block's size was hinted.
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let (peer, mut state) = peer_state(36);
    state.max_response_bytes = u32::try_from(worst * 10).unwrap_or(u32::MAX);
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1_000);
    scheduler.refresh_needed(vec![
        needed(30, BlockSizeEstimate::Unknown),
        needed(31, BlockSizeEstimate::Advertised(500)),
        needed(32, BlockSizeEstimate::Confirmed(50_000)),
    ]);
    let mut budget = ByteBudget::new(worst * 100);

    let request = scheduler
        .next_for_peer(
            &peer,
            &state,
            &mut budget,
            u64::MAX,
            MAX_BS_BLOCKS_PER_REQUEST,
        )
        .expect("range fits");
    assert_eq!(request.count, 3);
    assert_eq!(request.estimated_bytes, worst * 3);
}

#[test]
fn reorder_drains_only_contiguous_prefix_without_releasing_budget() {
    let mut reorder = ReorderBuffer::new();
    let mut budget = ByteBudget::new(10_000);
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);

    // The reorder buffer no longer touches the budget on insert: a received body
    // already owns its (shrunk) reservation, so the caller reserves and the buffer
    // only takes ownership. Model that by reserving the actual bytes here.
    assert!(budget.try_reserve(300));
    assert_eq!(
        reorder.insert(block::Height(3), block.clone(), 300, peer(0)),
        ReorderInsertResult::Inserted
    );
    assert!(reorder.drain_contiguous_prefix(block::Height(0)).is_empty());
    assert_eq!(reorder.buffered_bytes(), 300);
    assert_eq!(budget.reserved(), 300);

    assert!(budget.try_reserve(100));
    assert_eq!(
        reorder.insert(block::Height(1), block.clone(), 100, peer(0)),
        ReorderInsertResult::Inserted
    );
    let released = reorder.drain_contiguous_prefix(block::Height(0));
    assert_eq!(
        released
            .iter()
            .map(|(height, _, bytes, _)| (*height, *bytes))
            .collect::<Vec<_>>(),
        vec![(block::Height(1), 100)]
    );
    // Draining the contiguous prefix hands bytes to the apply stage; it does not
    // release the budget, which the apply finish releases later.
    assert_eq!(reorder.buffered_bytes(), 300);
    assert_eq!(budget.reserved(), 400);
    budget.release(100);

    assert!(budget.try_reserve(200));
    assert_eq!(
        reorder.insert(block::Height(2), block.clone(), 200, peer(0)),
        ReorderInsertResult::Inserted
    );
    let released = reorder.drain_contiguous_prefix(block::Height(1));
    assert_eq!(
        released
            .iter()
            .map(|(height, _, bytes, _)| (*height, *bytes))
            .collect::<Vec<_>>(),
        vec![(block::Height(2), 200), (block::Height(3), 300)]
    );
    assert_eq!(budget.reserved(), 500);
    budget.release(500);

    assert!(budget.try_reserve(200));
    assert_eq!(
        reorder.insert(block::Height(2), block.clone(), 200, peer(0)),
        ReorderInsertResult::Inserted
    );
    assert!(budget.try_reserve(300));
    assert_eq!(
        reorder.insert(block::Height(3), block, 300, peer(0)),
        ReorderInsertResult::Inserted
    );
    // `drop_from`/`drop_through`/`clear` still release the budget for bodies that
    // never reach the apply stage.
    reorder.drop_from(block::Height(3), &mut budget);
    assert_eq!(reorder.buffered_bytes(), 200);
    assert_eq!(budget.reserved(), 200);
    reorder.drop_through(block::Height(2), &mut budget);
    assert_eq!(reorder.buffered_bytes(), 0);
    assert_eq!(budget.reserved(), 0);
    assert!(budget.try_reserve(300));
    assert_eq!(
        reorder.insert(
            block::Height(3),
            mainnet_block(&BLOCK_MAINNET_1_BYTES),
            300,
            peer(0)
        ),
        ReorderInsertResult::Inserted
    );
    reorder.clear(&mut budget);
    assert_eq!(reorder.buffered_bytes(), 0);
    assert_eq!(budget.reserved(), 0);
}

#[test]
fn reorder_fuzzes_arrival_order_as_parent_first() {
    let orders = [
        [1, 2, 3, 4],
        [4, 3, 2, 1],
        [2, 4, 1, 3],
        [3, 1, 4, 2],
        [2, 1, 4, 3],
    ];

    for order in orders {
        let mut reorder = ReorderBuffer::new();
        let mut budget = ByteBudget::new(10_000);
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        let mut tip = block::Height(0);
        let mut released_all = Vec::new();

        for height in order {
            assert!(budget.try_reserve(100));
            assert_eq!(
                reorder.insert(block::Height(height), block.clone(), 100, peer(0)),
                ReorderInsertResult::Inserted
            );
            for (released, _, bytes, _) in reorder.drain_contiguous_prefix(tip) {
                assert_eq!(released, block::Height(tip.0 + 1));
                tip = released;
                released_all.push(released);
                budget.release(bytes);
            }
        }

        assert_eq!(
            released_all,
            vec![
                block::Height(1),
                block::Height(2),
                block::Height(3),
                block::Height(4)
            ]
        );
        assert_eq!(budget.reserved(), 0);
    }
}

/// Build an outstanding three-block range whose worst-case reservation is already
/// held against `budget`, mirroring what the scheduler does at send time.
fn outstanding_three_block_range(budget: &mut ByteBudget) -> OutstandingBlockRange {
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let request = BlockRangeRequest {
        start_height: block::Height(1),
        count: 3,
        anchor_hash: block::Hash([1; 32]),
        // Worst-case reservation: three blocks each reserve one worst-case share.
        estimated_bytes: worst * 3,
        expected_hashes: vec![
            (block::Height(1), block::Hash([1; 32])),
            (block::Height(2), block::Hash([2; 32])),
            (block::Height(3), block::Hash([3; 32])),
        ],
        // Size hints below the worst case; the reservation does not depend on them.
        expected_bytes: vec![
            (block::Height(1), 1_000),
            (block::Height(2), 1_000),
            (block::Height(3), 1_000),
        ],
    };
    assert!(budget.try_reserve(request.estimated_bytes));
    OutstandingBlockRange {
        request,
        deadline: Instant::now(),
        received: HashSet::new(),
    }
}

/// The global reservation must never exceed the budget and must monotonically
/// shrink over a block's lifetime across the download -> buffer -> apply -> commit
/// path, and across timeout/duplicate/short-response paths. This is the budget
/// half of the worst-case lossless scheme: a block reserves worst case at send,
/// only ever shrinks toward its actual serialized size, and is never re-reserved.
#[test]
fn budget_reservation_never_exceeds_max_and_only_shrinks_per_block() {
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    let max = worst * 3;

    // Happy path: download -> shrink-on-receipt -> buffer -> apply -> commit.
    {
        let mut budget = ByteBudget::new(max);
        let mut reorder = ReorderBuffer::new();
        let mut outstanding = outstanding_three_block_range(&mut budget);
        assert_eq!(budget.reserved(), max);
        assert!(budget.reserved() <= budget.max_bytes_for_test());

        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        // Receive each height: release `worst - actual`, keep `actual` reserved,
        // and hand `actual` to the reorder buffer without re-reserving.
        for (index, height) in [block::Height(1), block::Height(2), block::Height(3)]
            .into_iter()
            .enumerate()
        {
            let before = budget.reserved();
            let actual = 1_000u64 + index as u64; // < worst, varies per block
            budget.release(worst.saturating_sub(actual));
            outstanding.mark_received(height);
            assert_eq!(
                reorder.insert(height, block.clone(), actual, peer(0)),
                ReorderInsertResult::Inserted
            );
            // Per-block reservation only shrank (worst -> actual), never grew.
            assert!(budget.reserved() <= before);
            assert!(budget.reserved() <= budget.max_bytes_for_test());
        }
        assert!(outstanding.is_complete());
        assert_eq!(outstanding.reserved_bytes(), 0);
        assert_eq!(budget.reserved(), 1_000 + 1_001 + 1_002);

        // Commit: draining to apply carries the actual bytes; the apply finish
        // releases them.
        let mut floor = block::Height(0);
        let mut applied_bytes = 0;
        for (_height, _block, bytes, _peer) in reorder.drain_contiguous_prefix(floor) {
            applied_bytes += bytes;
            floor = block::Height(floor.0 + 1);
        }
        assert_eq!(applied_bytes, 1_000 + 1_001 + 1_002);
        budget.release(applied_bytes);
        assert_eq!(budget.reserved(), 0);
    }

    // Timeout / short-response path: heights that never buffer release exactly
    // their worst-case share, with no leak and no double-release.
    {
        let mut budget = ByteBudget::new(max);
        let mut outstanding = outstanding_three_block_range(&mut budget);
        assert_eq!(budget.reserved(), worst * 3);
        // A short response delivers only height 1; release its worst-case share.
        budget.release(worst.saturating_sub(1_000));
        outstanding.mark_received(block::Height(1));
        // The remaining two unreceived heights still reserve worst case each.
        assert_eq!(outstanding.reserved_bytes(), worst * 2);
        assert!(budget.reserved() <= budget.max_bytes_for_test());
        // On timeout the outstanding range releases its still-reserved worst case.
        budget.release(outstanding.reserved_bytes());
        // Plus the actual bytes held for the one received-but-not-buffered height.
        budget.release(1_000);
        assert_eq!(budget.reserved(), 0);
    }

    // Duplicate path: a duplicate body reserves nothing extra and releases its
    // actual bytes, so the buffer's reservation is unchanged.
    {
        let mut budget = ByteBudget::new(max);
        let mut reorder = ReorderBuffer::new();
        let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
        assert!(budget.try_reserve(1_000));
        assert_eq!(
            reorder.insert(block::Height(1), block.clone(), 1_000, peer(0)),
            ReorderInsertResult::Inserted
        );
        // A second body for the same height is a duplicate; reserve-then-release
        // leaves the reservation exactly where it was.
        assert!(budget.try_reserve(1_000));
        assert_eq!(
            reorder.insert(block::Height(1), block, 1_000, peer(0)),
            ReorderInsertResult::Duplicate
        );
        budget.release(1_000);
        assert_eq!(budget.reserved(), 1_000);
        assert!(budget.reserved() <= budget.max_bytes_for_test());
    }
}

/// A body whose actual serialized size exceeds its advertised size hint is still
/// accepted and buffered: worst case (not the hint) was reserved up front, so the
/// shrink-on-receipt cannot fail. Under the old release-then-reserve scheme this
/// path could re-reserve more than the released estimate and drop a valid body.
#[test]
fn underestimated_body_is_buffered_without_budget_drop() {
    let worst = BS_PER_BLOCK_WORST_CASE_BYTES;
    // Budget holds exactly one worst-case share, so a hint-sized re-reservation
    // would have had no headroom for an underestimated body.
    let mut budget = ByteBudget::new(worst);
    let mut reorder = ReorderBuffer::new();

    let hint = 1_000u64;
    let request = BlockRangeRequest {
        start_height: block::Height(1),
        count: 1,
        anchor_hash: block::Hash([1; 32]),
        estimated_bytes: worst,
        expected_hashes: vec![(block::Height(1), block::Hash([1; 32]))],
        expected_bytes: vec![(block::Height(1), hint)],
    };
    assert!(budget.try_reserve(request.estimated_bytes));
    let mut outstanding = OutstandingBlockRange {
        request,
        deadline: Instant::now(),
        received: HashSet::new(),
    };
    assert_eq!(budget.reserved(), worst);

    // The body's actual serialized size is far larger than the hint (but still
    // <= MAX_BLOCK_BYTES, the per-block worst case).
    let actual = hint * 50;
    assert!(actual < worst);
    assert!(actual > hint);

    // Receipt: shrink toward the actual size and hand it to the reorder buffer
    // without re-reserving. The shrink is non-negative because actual <= worst.
    budget.release(worst.saturating_sub(actual));
    outstanding.mark_received(block::Height(1));
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    assert_eq!(
        reorder.insert(block::Height(1), block, actual, peer(0)),
        ReorderInsertResult::Inserted,
        "an underestimated body must still buffer; worst case was reserved up front"
    );
    assert_eq!(reorder.buffered_bytes(), actual);
    assert_eq!(budget.reserved(), actual);
    assert!(budget.reserved() <= budget.max_bytes_for_test());
}

#[test]
fn block_sync_stream_declares_kind_capability_version_and_frame_cap() {
    let stream = block_sync_streams()
        .first()
        .copied()
        .expect("block sync declares one stream");

    assert_eq!(stream.kind, ZAKURA_STREAM_BLOCK_SYNC);
    assert_eq!(stream.version, ZAKURA_BLOCK_SYNC_STREAM_VERSION);
    assert_eq!(stream.capability, ZAKURA_CAP_BLOCK_SYNC);
    assert_eq!(stream.mode, StreamMode::Ordered);
    assert_eq!(stream.frame_cap, MAX_BS_FRAME_BYTES);
}

#[tokio::test]
async fn service_registry_routes_block_sync_by_exact_capability_and_version() {
    let service = Arc::new(BlockSyncService::new(ZakuraBlockSyncConfig::default()));
    let registry =
        ServiceRegistry::new(vec![service]).expect("block-sync service declares unique kind");
    let peer = peer(1);

    assert_eq!(
        registry.capability_for_stream(ZAKURA_STREAM_BLOCK_SYNC, ZAKURA_BLOCK_SYNC_STREAM_VERSION),
        Some(ZAKURA_CAP_BLOCK_SYNC)
    );
    assert!(registry
        .capability_for_stream(
            ZAKURA_STREAM_BLOCK_SYNC,
            ZAKURA_BLOCK_SYNC_STREAM_VERSION + 1
        )
        .is_none());
    assert_eq!(
        registry
            .ordered_streams_for_negotiated(ZAKURA_CAP_BLOCK_SYNC)
            .iter()
            .map(|stream| stream.kind)
            .collect::<Vec<_>>(),
        vec![ZAKURA_STREAM_BLOCK_SYNC]
    );
    assert!(registry.ordered_streams_for_negotiated(0).is_empty());
    assert!(registry.wants_ordered_stream(
        ZAKURA_STREAM_BLOCK_SYNC,
        ZAKURA_CAP_BLOCK_SYNC,
        &peer,
        ServicePeerDirection::Inbound,
    ));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn inert_reactor_parks_after_header_tip_watch_closes() {
    let _service = BlockSyncService::new(ZakuraBlockSyncConfig::default());

    let elapsed = tokio::time::timeout(Duration::from_secs(1), future::pending::<()>()).await;

    assert!(
        elapsed.is_err(),
        "paused-time timeout only elapses if the inert reactor has no always-ready branch"
    );
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "state-backed block sync must have exactly one frontier source")]
fn state_backed_reactor_panics_with_two_frontier_sources() {
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let (_frontier_tx, frontier_rx) =
        watch::channel(test_frontier_update(0, 0, 0, FrontierChange::Snapshot));
    let mut startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        ZakuraBlockSyncConfig::default(),
    );
    startup.frontier_updates = Some(frontier_rx);

    let (_handle, _actions, _task) = spawn_block_sync_reactor(startup);
}

#[tokio::test]
async fn add_peer_emits_events_and_round_trips_status_over_framed_path() {
    let (service, mut events) = BlockSyncService::new_for_test(ZakuraBlockSyncConfig::default());
    let peer = peer(2);
    let cancel_token = CancellationToken::new();
    let (inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, mut outbound_rx) = framed_channel(4);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        streams,
        cancel_token,
    ));

    let session = match next_event(&mut events).await {
        BlockSyncEvent::PeerConnected(session) => session,
        event => panic!("expected PeerConnected, got {event:?}"),
    };
    assert_eq!(session.peer_id(), &peer);
    assert_eq!(service.peer_count(), 1);

    service
        .send_action(BlockSyncAction::SendMessage {
            peer: peer.clone(),
            msg: BlockSyncMessage::Status(status()),
        })
        .await
        .expect("action queues to source");
    let frame = tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
        .await
        .expect("action status frame should be sent")
        .expect("outbound frame receiver stays open");
    assert_eq!(
        BlockSyncMessage::decode_frame(frame).expect("status frame decodes"),
        BlockSyncMessage::Status(status())
    );

    inbound_tx
        .send(
            BlockSyncMessage::Status(status())
                .encode_frame()
                .expect("status frame encodes"),
        )
        .await
        .expect("inbound status queues");
    assert!(matches!(
        next_event(&mut events).await,
        BlockSyncEvent::WireMessage {
            msg: BlockSyncMessage::Status(_),
            ..
        }
    ));

    service.remove_peer(&peer);
    assert_eq!(service.peer_count(), 0);
    assert!(session.cancel_token().is_cancelled());
}

#[tokio::test]
async fn stale_block_sync_teardown_keeps_replacement_session() {
    let (service, mut events) = BlockSyncService::new_for_test(ZakuraBlockSyncConfig::default());
    let peer = peer(92);

    let (old_inbound_tx, old_inbound_rx) = framed_channel(4);
    let (old_outbound_tx, _old_outbound_rx) = framed_channel(4);
    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (old_inbound_rx, old_outbound_tx))]),
        CancellationToken::new(),
    ));
    assert!(matches!(
        next_event(&mut events).await,
        BlockSyncEvent::PeerConnected(session) if session.peer_id() == &peer
    ));

    let (new_inbound_tx, new_inbound_rx) = framed_channel(4);
    let (new_outbound_tx, mut new_outbound_rx) = framed_channel(4);
    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (new_inbound_rx, new_outbound_tx))]),
        CancellationToken::new(),
    ));
    assert!(matches!(
        next_event(&mut events).await,
        BlockSyncEvent::PeerConnected(session) if session.peer_id() == &peer
    ));
    assert_eq!(service.peer_count(), 1);

    drop(old_inbound_tx);
    tokio::time::sleep(Duration::from_millis(50)).await;

    if let Ok(Some(BlockSyncEvent::PeerDisconnected(disconnected))) =
        tokio::time::timeout(Duration::from_millis(50), events.recv()).await
    {
        panic!("stale teardown disconnected replacement session for {disconnected:?}");
    }
    assert_eq!(service.peer_count(), 1);

    service
        .send_action(BlockSyncAction::SendMessage {
            peer: peer.clone(),
            msg: BlockSyncMessage::Status(status()),
        })
        .await
        .expect("replacement session remains installed");
    let frame = tokio::time::timeout(Duration::from_secs(1), new_outbound_rx.recv())
        .await
        .expect("replacement session sends")
        .expect("replacement outbound stream is live");
    assert_eq!(
        BlockSyncMessage::decode_frame(frame).expect("status frame decodes"),
        BlockSyncMessage::Status(status())
    );

    drop(new_inbound_tx);
}

#[tokio::test]
async fn lifecycle_events_bypass_full_bounded_wire_queue() {
    let mut config = ZakuraBlockSyncConfig::default();
    config.peer_limits.inbound_queue_depth = 1;
    let (events, _event_rx) = mpsc::channel(config.peer_limits.inbound_queue_depth);
    events
        .try_send(BlockSyncEvent::WireMessage {
            peer: peer(90),
            msg: BlockSyncMessage::Status(status()),
        })
        .expect("test fills bounded wire queue");
    let (lifecycle, mut lifecycle_rx) = mpsc::unbounded_channel();
    let (_peers_tx, peers) = watch::channel(ServicePeerSnapshot::new(0, 0, config.peer_limits));
    let (_status_tx, status) = watch::channel(config.initial_status());
    let (_candidates_tx, candidates) = watch::channel(ZakuraBlockSyncCandidateState::default());
    let handle = BlockSyncHandle {
        events,
        lifecycle,
        peers,
        status,
        candidates,
    };
    let service = BlockSyncService::new_with_handle_for_test(config, handle);
    let peer = peer(91);
    let (inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    let _inbound_tx = inbound_tx;

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), lifecycle_rx.recv())
            .await
            .expect("lifecycle event arrives")
            .expect("lifecycle channel stays open"),
        BlockSyncEvent::PeerConnected(session) if session.peer_id() == &peer
    ));

    service.remove_peer(&peer);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), lifecycle_rx.recv())
            .await
            .expect("lifecycle event arrives")
            .expect("lifecycle channel stays open"),
        BlockSyncEvent::PeerDisconnected(disconnected) if disconnected == peer
    ));
}

#[tokio::test]
async fn add_peer_decode_failure_emits_wire_decode_failed() {
    let (service, mut events) = BlockSyncService::new_for_test(ZakuraBlockSyncConfig::default());
    let peer = peer(3);
    let (inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    let cancel_token = CancellationToken::new();

    service.add_peer(Peer::new(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        streams,
        cancel_token.clone(),
    ));
    let _ = next_event(&mut events).await;

    inbound_tx
        .send(Frame {
            message_type: u16::from(MSG_BS_STATUS),
            flags: 0,
            payload: Vec::new(),
        })
        .await
        .expect("malformed inbound frame queues");

    assert!(matches!(
        next_event(&mut events).await,
        BlockSyncEvent::WireDecodeFailed { .. }
    ));
    assert!(cancel_token.is_cancelled());
}

#[tokio::test]
async fn registry_add_peer_requires_negotiated_block_sync_capability() {
    let (service, mut events) = BlockSyncService::new_for_test(ZakuraBlockSyncConfig::default());
    let registry = ServiceRegistry::new(vec![Arc::new(service)])
        .expect("block-sync service declares unique kind");
    let peer = peer(4);
    let (inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    registry.add_peer(Peer::new(peer, None, 0, streams, CancellationToken::new()));
    drop(inbound_tx);

    assert!(
        tokio::time::timeout(Duration::from_millis(100), events.recv())
            .await
            .is_err(),
        "without cap 1<<3 the registry must not deliver kind-6 streams"
    );
}

#[tokio::test]
async fn wants_peer_rejects_when_configured_slot_cap_is_reached() {
    let config = ZakuraBlockSyncConfig {
        peer_limits: ServicePeerLimits {
            max_inbound_peers: 0,
            max_outbound_peers: 2,
            ..ServicePeerLimits::default()
        },
        ..ZakuraBlockSyncConfig::default()
    };
    let (service, mut events) = BlockSyncService::new_for_test(config);
    let inbound_peer = peer(5);

    assert!(!service.wants_peer(
        &inbound_peer,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Inbound
    ));
    assert!(service.wants_peer(
        &inbound_peer,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound
    ));

    let mut inbound_senders = Vec::new();
    for byte in 6..=7 {
        let peer_id = peer(byte);
        let (inbound_tx, inbound_rx) = framed_channel(4);
        let (outbound_tx, _outbound_rx) = framed_channel(4);
        let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

        service.add_peer(Peer::new_with_direction(
            peer_id.clone(),
            None,
            ZAKURA_CAP_BLOCK_SYNC,
            ServicePeerDirection::Outbound,
            streams,
            CancellationToken::new(),
        ));

        assert!(matches!(
            next_event(&mut events).await,
            BlockSyncEvent::PeerConnected(session) if session.peer_id() == &peer_id
        ));
        inbound_senders.push(inbound_tx);
    }

    assert_eq!(service.peer_count(), 2);
    assert!(!service.wants_peer(
        &peer(8),
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound
    ));

    let (_inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer(8),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    assert_eq!(service.peer_count(), 2);
}

#[tokio::test]
async fn reactor_drives_tip_to_getblocks_to_submit_over_framed_path() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer = peer(40);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block::Hash([1; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let block_hash = block.hash();
    let block_size = u32::try_from(
        block
            .zcash_serialize_to_vec()
            .expect("block serializes")
            .len(),
    )
    .expect("test block size fits u32");
    tip_tx
        .send((block::Height(1), block_hash))
        .expect("tip watch is live");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                assert_eq!(verified_block_tip, block::Height(0));
                assert_eq!(best_header_tip, block::Height(1));
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before query: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block_hash,
            size: BlockSizeEstimate::Advertised(block_size),
        }]))
        .await
        .expect("needed metadata queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SendMessage {
                peer: action_peer,
                msg:
                    BlockSyncMessage::GetBlocks {
                        start_height,
                        count,
                    },
            } => {
                assert_eq!(action_peer, peer);
                assert_eq!(start_height, block::Height(1));
                assert_eq!(count, 1);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before getblocks: {action:?}"),
        }
    }

    loop {
        let frame = tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
            .await
            .expect("outbound frame arrives")
            .expect("outbound channel is live");
        if let BlockSyncMessage::GetBlocks {
            start_height,
            count,
        } = BlockSyncMessage::decode_frame(frame).expect("outbound frame decodes")
        {
            assert_eq!(start_height, block::Height(1));
            assert_eq!(count, 1);
            break;
        }
    }

    inbound_tx
        .send(
            BlockSyncMessage::Block(block.clone())
                .encode_frame()
                .expect("block frame encodes"),
        )
        .await
        .expect("block frame queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock {
                block: submitted, ..
            } => {
                assert_eq!(submitted.hash(), block_hash);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_keeps_submitted_body_budget_until_apply_finishes() {
    let blocks = mainnet_blocks_1_to_3();
    let block1_size = block_size(&blocks[0]);
    let mut config = immediate_body_download_config();
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(41);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(2),
                tip_hash: blocks[1].hash(),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(2), blocks[1].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: blocks[0].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
        ]))
        .await
        .expect("needed metadata queues");

    let (request_peer, start_height, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(request_peer, peer_id);
    assert_eq!(start_height, block::Height(1));
    assert_eq!(count, 1);
    while !matches!(
        BlockSyncMessage::decode_frame(
            tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
                .await
                .expect("outbound frame arrives")
                .expect("outbound channel is live")
        )
        .expect("frame decodes"),
        BlockSyncMessage::GetBlocks {
            start_height: block::Height(1),
            count: 1,
        }
    ) {}

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");

    let submit_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    };
    assert_eq!(handle.local_status().servable_high, block::Height(0));

    let quiet = tokio::time::timeout(Duration::from_millis(100), actions.recv()).await;
    assert!(
        quiet.is_err(),
        "submitted-but-not-applied block should keep body budget full",
    );

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: submit_token,
            height: block::Height(1),
            hash: blocks[0].hash(),
            result: BlockApplyResult::Committed,
            local_frontier: Some(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(1),
                verified_block_hash: blocks[0].hash(),
            }),
        })
        .await
        .expect("apply-finished event queues");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.local_status().servable_high == block::Height(1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("apply completion frontier advances advertised status");

    let (_request_peer, start_height, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(start_height, block::Height(2));
    assert_eq!(count, 1);

    reactor_task.abort();
}

/// A real body whose serialized size dwarfs its advertised size hint is still
/// accepted, buffered, and submitted. Worst case (not the hint) is reserved at
/// send time, so shrink-on-receipt always has room and never drops the body.
///
/// A/B: under the old release-then-reserve scheme the request reserved only the
/// hint-sized estimate, so a body larger than the hint re-reserved more than was
/// released and, against a tight budget, hit `BudgetFull` and dropped a valid
/// body. With worst-case reservation this path is unreachable.
#[tokio::test]
async fn reactor_buffers_body_larger_than_its_size_hint() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    // Budget holds exactly one worst-case share, so a hint-sized re-reservation
    // would have left no headroom for an underestimated body.
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        41,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    tip_tx
        .send((block::Height(1), blocks[0].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    // Advertise a 1-byte size hint, far below the real body size, with a
    // tolerance that admits the deviation without misbehavior scoring.
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: blocks[0].hash(),
            size: BlockSizeEstimate::Advertised(1),
        }]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(1), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("body queues");

    let submitted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match next_action(&mut actions).await {
                BlockSyncAction::SubmitBlock { block, .. } => break block.hash(),
                // The preserved size-deviation check reports the hint mismatch but
                // must not drop the body.
                BlockSyncAction::Misbehavior {
                    reason: BlockSyncMisbehavior::SizeMismatch,
                    ..
                }
                | BlockSyncAction::SendMessage { .. }
                | BlockSyncAction::QueryNeededBlocks { .. } => {}
                action => panic!("unexpected action before submit: {action:?}"),
            }
        }
    })
    .await
    .expect("underestimated body must still be buffered and submitted, not dropped");
    assert_eq!(submitted, blocks[0].hash());

    reactor_task.abort();
}

/// A stalled commit must not pace downloads. The refill low-water mark counts
/// only the download pipeline (`queued` + `outstanding`), never the commit
/// pipeline (`reorder` + `applying`), so a block stuck in `applying` (submitted
/// but never apply-finished) does not stop the reactor from re-querying and
/// downloading higher heights — downloads run ahead of commit, bounded only by
/// the in-flight byte budget.
///
/// A/B: before the decoupling, block 1 held in `applying` satisfies the
/// low-water mark (sized to one block here), so the reactor never issues the
/// second `QueryNeededBlocks` and this test times out waiting for it.
#[tokio::test]
async fn reactor_downloads_run_ahead_of_stalled_commit() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    // Low-water mark = 1 status peer * advertised inflight (1) * advertised
    // blocks-per-response (1) = 1 block, so a single block held in the commit
    // pipeline is enough to (before the fix) satisfy it. The default 4 GiB byte
    // budget stays far from binding for these tiny blocks.
    config.max_inflight_requests = 1;
    config.max_blocks_per_response = 1;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status_message(
        &service,
        &mut actions,
        41,
        BlockSyncStatus {
            servable_low: block::Height(1),
            servable_high: block::Height(3),
            tip_hash: blocks[2].hash(),
            max_blocks_per_response: 1,
            max_inflight_requests: 4,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        },
    )
    .await;

    // A header tip above the verified tip opens the download window.
    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(3)).await;

    // Supply only height 1, download it, and submit it — then leave it stuck in
    // `applying` by never sending BlockApplyFinished (a stalled commit).
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[0])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 1)
    );
    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");
    let _submit_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    };

    // The commit is now stalled: block 1 sits in `applying` (awaiting an
    // apply-finished that never comes), holding its byte reservation. The
    // download floor advanced to 1 on submit, but `queued` and `outstanding`
    // are both empty. A header-tip bump must still make the reactor re-query
    // and download height 2 — the download pipeline is empty even though the
    // commit pipeline is not.
    tip_tx
        .send((block::Height(4), block::Hash([4; 32])))
        .expect("tip watch is live");
    wait_for_query_needed_blocks(&mut actions, block::Height(1), block::Height(4)).await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[1])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(2), 1),
        "the reactor must download height 2 ahead of the stalled commit of height 1",
    );

    reactor_task.abort();
}

fn add_outbound_block_sync_peer(
    service: &BlockSyncService,
    byte: u8,
    held: &mut Vec<(FramedSend, FramedRecv)>,
) -> ZakuraPeerId {
    let peer_id = peer(byte);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    held.push((inbound_tx, outbound_rx));
    peer_id
}

/// A connection-symmetry collision resolves by the loser re-registering an
/// already-present peer (adopting the winner's incoming stream). That
/// replacement must succeed even when the per-direction cap is full — the peer
/// is already counted — while a genuinely new peer at the cap is still rejected.
#[tokio::test]
async fn block_sync_add_peer_replaces_same_peer_even_at_full_cap() {
    let mut config = immediate_body_download_config();
    config.peer_limits.max_outbound_peers = 1;
    config.peer_limits.max_inbound_peers = 0;
    let (service, mut events) = BlockSyncService::new_for_test(config);

    // Keep every stream handle alive so the per-peer pipes are not torn down.
    let mut held = Vec::new();

    // Peer A fills the only outbound slot.
    let peer_a = add_outbound_block_sync_peer(&service, 41, &mut held);
    match next_event(&mut events).await {
        BlockSyncEvent::PeerConnected(session) => assert_eq!(session.peer_id(), &peer_a),
        event => panic!("expected PeerConnected for peer A, got {event:?}"),
    }
    assert_eq!(service.peer_count(), 1);

    // A distinct, new peer at the full cap is rejected: no session is created.
    let _peer_b = add_outbound_block_sync_peer(&service, 42, &mut held);
    let quiet = tokio::time::timeout(Duration::from_millis(100), events.recv()).await;
    assert!(
        quiet.is_err(),
        "a new peer at a full per-direction cap must be rejected",
    );
    assert_eq!(service.peer_count(), 1);

    // Re-registering peer A (the collision adoption) replaces its session even
    // though the cap is full, because A is already counted. The stale-session
    // teardown keys on the session id, so only a fresh PeerConnected is emitted.
    let peer_a_again = add_outbound_block_sync_peer(&service, 41, &mut held);
    assert_eq!(peer_a_again, peer_a);
    match next_event(&mut events).await {
        BlockSyncEvent::PeerConnected(session) => assert_eq!(session.peer_id(), &peer_a),
        event => panic!("expected PeerConnected for replaced peer A, got {event:?}"),
    }
    assert_eq!(
        service.peer_count(),
        1,
        "the replacement must not leave two sessions for the same peer",
    );
}

#[tokio::test]
async fn reactor_keeps_applying_body_after_non_advancing_duplicate_result() {
    let blocks = mainnet_blocks_1_to_3();
    let block1_size = block_size(&blocks[0]);
    let mut config = immediate_body_download_config();
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES;

    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, _inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        42,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: blocks[0].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 1)
    );

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: peer_id,
            msg: BlockSyncMessage::Block(blocks[0].clone()),
        })
        .await
        .expect("body queues");
    let submit_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    };

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: submit_token,
            height: block::Height(1),
            hash: blocks[0].hash(),
            result: BlockApplyResult::Duplicate,
            local_frontier: Some(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            }),
        })
        .await
        .expect("non-advancing duplicate completion queues");

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: blocks[0].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
        ]))
        .await
        .expect("needed metadata after duplicate queues");

    let no_duplicate_request = tokio::time::timeout(Duration::from_millis(100), async {
        while let Some(action) = actions.recv().await {
            if let BlockSyncAction::SendMessage {
                msg:
                    BlockSyncMessage::GetBlocks {
                        start_height,
                        count,
                    },
                ..
            } = action
            {
                panic!("non-advancing duplicate result re-requested {start_height:?}/{count}");
            }
        }
    })
    .await;
    assert!(
        no_duplicate_request.is_err(),
        "reactor should keep waiting after a duplicate result that did not advance the frontier",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_keeps_active_response_when_needed_snapshot_omits_inflight_height() {
    let blocks = fake_sequential_blocks(3);
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(42);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(3),
                tip_hash: blocks[2].hash(),
                max_blocks_per_response: 16,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 3)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("first block queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } if block.hash() == blocks[0].hash() => break,
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before first submit: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[2])]))
        .await
        .expect("newer needed metadata queues");

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[1].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("second block queues");

    let mut submitted_second = false;
    while let Ok(Some(action)) = tokio::time::timeout(Duration::from_secs(1), actions.recv()).await
    {
        match action {
            BlockSyncAction::SubmitBlock { block, .. } if block.hash() == blocks[1].hash() => {
                submitted_second = true;
                break;
            }
            BlockSyncAction::Misbehavior {
                peer,
                reason: BlockSyncMisbehavior::UnsolicitedBlock,
            } if peer == peer_id => panic!("in-flight body was misclassified as unsolicited"),
            BlockSyncAction::SendMessage { .. }
            | BlockSyncAction::QueryNeededBlocks { .. }
            | BlockSyncAction::SubmitBlock { .. } => {}
            action => panic!("unexpected action after second block: {action:?}"),
        }
    }

    assert!(
        submitted_second,
        "second body from the original active response should remain correlated",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_unmatched_body_for_currently_needed_height() {
    let blocks = mainnet_blocks_1_to_3();
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(142);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(2),
                servable_high: block::Height(3),
                tip_hash: blocks[2].hash(),
                max_blocks_per_response: 16,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(1), blocks[0].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[0])]))
        .await
        .expect("needed metadata queues");

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("unmatched needed block queues");

    let quiet = tokio::time::timeout(Duration::from_millis(200), async {
        loop {
            if let BlockSyncAction::Misbehavior { reason, .. } = next_action(&mut actions).await {
                return reason;
            }
        }
    })
    .await;
    assert!(
        quiet.is_err(),
        "unmatched body for a currently needed height should not be hard misbehavior",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_accepts_unmatched_body_for_queued_height() {
    let blocks = mainnet_blocks_1_to_3();
    let config = immediate_body_download_config();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (_peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        143,
        block::Height(1),
        blocks[0].hash(),
        1,
        1,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[0])]))
        .await
        .expect("needed metadata queues");

    let no_getblocks = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            if let BlockSyncAction::SendMessage {
                msg: BlockSyncMessage::GetBlocks { .. },
                ..
            } = next_action(&mut actions).await
            {
                return;
            }
        }
    })
    .await;
    assert!(
        no_getblocks.is_err(),
        "test setup requires the body to remain queued without an outstanding request",
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("unmatched queued block queues");

    let submitted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let BlockSyncAction::SubmitBlock { block, .. } = next_action(&mut actions).await {
                return block.hash();
            }
        }
    })
    .await
    .expect("unmatched queued body is submitted");
    assert_eq!(submitted, blocks[0].hash());

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_accepts_queued_body_from_recently_disconnected_peer() {
    let blocks = mainnet_blocks_1_to_3();
    let config = immediate_body_download_config();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, _inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        144,
        block::Height(1),
        blocks[0].hash(),
        1,
        1,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[0])]))
        .await
        .expect("needed metadata queues");

    let no_getblocks = tokio::time::timeout(Duration::from_millis(100), async {
        loop {
            if let BlockSyncAction::SendMessage {
                msg: BlockSyncMessage::GetBlocks { .. },
                ..
            } = next_action(&mut actions).await
            {
                return;
            }
        }
    })
    .await;
    assert!(
        no_getblocks.is_err(),
        "test setup requires the body to remain queued without an outstanding request",
    );

    service.remove_peer(&peer_id);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.peer_snapshot().outbound_peers == 0 && service.peer_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect releases the block-sync peer slot");

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: peer_id,
            msg: BlockSyncMessage::Block(blocks[0].clone()),
        })
        .await
        .expect("late disconnected body queues");

    let submitted = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let BlockSyncAction::SubmitBlock { block, .. } = next_action(&mut actions).await {
                return block.hash();
            }
        }
    })
    .await
    .expect("late disconnected queued body is submitted");
    assert_eq!(submitted, blocks[0].hash());

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_queries_needed_blocks_above_submitted_floor() {
    let blocks = mainnet_blocks_1_to_3();
    let block1_size = block_size(&blocks[0]);
    let block2_size = block_size(&blocks[1]);
    let mut config = immediate_body_download_config();
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES * 2;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(43);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(3),
                tip_hash: blocks[2].hash(),
                max_blocks_per_response: 2,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: blocks[0].hash(),
                size: BlockSizeEstimate::Advertised(block1_size),
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(block2_size),
            },
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(1), 2)
    );

    for block in blocks.iter().take(2) {
        inbound_tx
            .send(
                BlockSyncMessage::Block(block.clone())
                    .encode_frame()
                    .expect("block encodes"),
            )
            .await
            .expect("block queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < 2 {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                submitted.push((
                    block.coinbase_height().expect("test block has height"),
                    token,
                ));
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before checkpoint submissions: {action:?}"),
        }
    }
    assert_eq!(
        submitted
            .iter()
            .map(|(height, _token)| *height)
            .collect::<Vec<_>>(),
        vec![block::Height(1), block::Height(2)]
    );

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: submitted[0].1,
            height: block::Height(1),
            hash: blocks[0].hash(),
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("apply-finished event queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                assert_eq!(
                    verified_block_tip,
                    block::Height(2),
                    "missing-body query must skip already submitted contiguous bodies",
                );
                assert_eq!(best_header_tip, block::Height(3));
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before needed-block query: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_retries_submitted_body_after_apply_rejection() {
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let block_bytes = block_size(&block);
    let mut config = immediate_body_download_config();
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES;

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(42);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block.hash(),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(1), block.hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block.hash(),
            size: BlockSizeEstimate::Advertised(block_bytes),
        }]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(block.clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");
    let submit_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock {
                token,
                block: submitted,
            } => {
                assert_eq!(submitted.hash(), block.hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    };

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: submit_token,
            height: block::Height(1),
            hash: block.hash(),
            result: BlockApplyResult::Rejected,
            local_frontier: None,
        })
        .await
        .expect("apply-finished event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block.hash(),
            size: BlockSizeEstimate::Advertised(block_bytes),
        }]))
        .await
        .expect("needed metadata queues after rejection");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(1), 1),
        "apply rejection must release capacity and clear submitted coverage"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_pauses_new_body_downloads_near_tip_by_default() {
    let config = ZakuraBlockSyncConfig::default();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(70);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);
    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(3),
                tip_hash: block::Hash([3; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status queues");

    for height in [block::Height(1), block::Height(2)] {
        let hash_byte = u8::try_from(height.0).expect("test height fits in u8");
        handle
            .send(BlockSyncEvent::HeaderTipChanged {
                height,
                hash: block::Hash([hash_byte; 32]),
            })
            .await
            .expect("header-tip event queues");
        let quiet = tokio::time::timeout(Duration::from_millis(100), actions.recv()).await;
        assert!(
            quiet.is_err(),
            "lag {height:?} is within the default pause window and must not query needed blocks",
        );

        handle
            .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
                height,
                hash: block::Hash([hash_byte; 32]),
                size: BlockSizeEstimate::Unknown,
            }]))
            .await
            .expect("stale needed-block event queues");
        let quiet = tokio::time::timeout(Duration::from_millis(100), actions.recv()).await;
        assert!(
            quiet.is_err(),
            "stale needed metadata inside the pause window must not schedule GetBlocks",
        );
    }

    handle
        .send(BlockSyncEvent::HeaderTipChanged {
            height: block::Height(3),
            hash: block::Hash([3; 32]),
        })
        .await
        .expect("header-tip event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(0),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block::Hash([1; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block::Hash([2; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(3),
                hash: block::Hash([3; 32]),
                size: BlockSizeEstimate::Unknown,
            },
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(1), 1)
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_zero_pause_threshold_preserves_lag_one_downloads() {
    let config = immediate_body_download_config();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config,
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    handle
        .send(BlockSyncEvent::HeaderTipChanged {
            height: block::Height(1),
            hash: block::Hash([1; 32]),
        })
        .await
        .expect("header-tip event queues");

    assert!(matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(0),
            best_header_tip: block::Height(1),
        }
    ));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_keeps_block_sync_peer_after_catch_up_and_reuses_later() {
    let mut config = immediate_body_download_config();
    config.peer_limits.max_outbound_peers = 1;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(4), block::Hash([4; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(4), block::Hash([4; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        71,
        block::Height(6),
        block::Hash([6; 32]),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::HeaderTipChanged {
            height: block::Height(3),
            hash: block::Hash([3; 32]),
        })
        .await
        .expect("header-tip event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block::Hash([1; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block::Hash([2; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(3),
                hash: block::Hash([3; 32]),
                size: BlockSizeEstimate::Unknown,
            },
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 3)
    );

    for height in [block::Height(1), block::Height(2)] {
        let hash_byte = u8::try_from(height.0).expect("test height fits in u8");
        handle
            .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: height,
                verified_block_hash: block::Hash([hash_byte; 32]),
            }))
            .await
            .expect("frontier event queues");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(handle.peer_snapshot().outbound_peers, 1);
        assert_eq!(service.peer_count(), 1);
    }

    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(1),
                returned: 3,
            }
            .encode_frame()
            .expect("BlocksDone encodes"),
        )
        .await
        .expect("BlocksDone queues");

    handle
        .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(3),
            verified_block_hash: block::Hash([3; 32]),
        }))
        .await
        .expect("caught-up frontier event queues");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        handle.peer_snapshot().outbound_peers,
        1,
        "caught-up nodes must keep block-sync peers so they can serve fresh nodes",
    );
    assert_eq!(service.peer_count(), 1);

    handle
        .send(BlockSyncEvent::HeaderTipChanged {
            height: block::Height(6),
            hash: block::Hash([6; 32]),
        })
        .await
        .expect("later header-tip event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(3),
            best_header_tip: block::Height(6),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(4),
                hash: block::Hash([4; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(5),
                hash: block::Hash([5; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(6),
                hash: block::Hash([6; 32]),
                size: BlockSizeEstimate::Unknown,
            },
        ]))
        .await
        .expect("new needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(4), 3),
        "the retained block-sync peer should be reused after later header growth",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_accepts_multi_block_range_and_submits_parent_first() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer = peer(43);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(3),
                tip_hash: blocks[2].hash(),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");
    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks
                .iter()
                .map(|block| BlockSyncBlockMeta {
                    height: block.coinbase_height().expect("test block has height"),
                    hash: block.hash(),
                    size: BlockSizeEstimate::Advertised(block_size(block)),
                })
                .collect(),
        ))
        .await
        .expect("needed metadata queues");

    let (action_peer, start_height, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(action_peer, peer);
    assert_eq!(start_height, block::Height(1));
    assert_eq!(count, 3);

    while !matches!(
        BlockSyncMessage::decode_frame(
            tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
                .await
                .expect("outbound frame arrives")
                .expect("outbound channel is live")
        )
        .expect("frame decodes"),
        BlockSyncMessage::GetBlocks {
            start_height: block::Height(1),
            count: 3,
        }
    ) {}

    for index in [1usize, 2, 0] {
        inbound_tx
            .send(
                BlockSyncMessage::Block(blocks[index].clone())
                    .encode_frame()
                    .expect("block encodes"),
            )
            .await
            .expect("block queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < 3 {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => submitted.push(
                block
                    .coinbase_height()
                    .expect("submitted test block has height"),
            ),
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::Misbehavior { reason, .. } => {
                panic!("honest multi-block response was misclassified: {reason:?}")
            }
            action => panic!("unexpected action before all submits: {action:?}"),
        }
    }
    assert_eq!(
        submitted,
        vec![block::Height(1), block::Height(2), block::Height(3)]
    );

    reactor_task.abort();
}

/// Every solicited body in a burst must reach the apply stage — the inbound
/// peer->reactor path must never silently drop a block body under load.
///
/// This is the regression guard for the production "drop-through" stall. The
/// per-peer wire queue is bounded; the old inbound pump forwarded decoded
/// messages with a non-blocking `try_send` and dropped solicited block bodies
/// once that queue filled during a body flood. A single dropped body wedges
/// `body_download_floor`, and because checkpoint-range commits wait
/// indefinitely for a contiguous range, the wedge never clears — every block
/// above it sits in `applying` forever and sync stalls at a checkpoint. The fix
/// makes the pump backpressure instead of dropping, so the flood always drains.
///
/// The harness makes a drop *fatal* rather than self-healing, which is what our
/// earlier tests missed: a one-slot wire queue forces the drop, a single
/// in-flight request stops the reactor from working around the gap, and a very
/// long request timeout means a dropped body is never re-requested within the
/// deadline. So a regression hangs here (outer timeout) instead of slowly
/// recovering and passing.
#[tokio::test]
async fn reactor_backpressures_inbound_body_flood_without_dropping_bodies() {
    const FLOOD: u32 = 64;
    let blocks = fake_sequential_blocks(FLOOD);
    let tip = blocks.last().expect("flood is non-empty").clone();
    let tip_height = tip.coinbase_height().expect("tip has height");

    let mut config = immediate_body_download_config();
    // One-slot wire queue: a pump that outruns the reactor by more than one
    // frame must backpressure, never drop.
    config.peer_limits.inbound_queue_depth = 1;
    // Hold the whole flood in flight at once so nothing pauses on the byte
    // budget; the inbound flood, not the budget, is what this test exercises.
    config.max_inflight_block_bytes = u64::MAX;
    // A dropped body must not be quietly re-requested and healed before the
    // deadline — that would hide the very regression this test guards.
    config.request_timeout = Duration::from_secs(300);

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer = peer(64);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: tip_height,
                tip_hash: tip.hash(),
                max_blocks_per_response: FLOOD,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");
    tip_tx
        .send((tip_height, tip.hash()))
        .expect("tip watch is live");

    // Feed solicited bodies from a dedicated task so the drive loop never blocks
    // on inbound backpressure while it still needs to drain reactor actions.
    let feed_blocks = blocks.clone();
    let (feed_tx, mut feed_rx) = mpsc::unbounded_channel::<u32>();
    let feeder = tokio::spawn(async move {
        while let Some(height) = feed_rx.recv().await {
            let frame = BlockSyncMessage::Block(feed_blocks[(height - 1) as usize].clone())
                .encode_frame()
                .expect("block frame encodes");
            if inbound_tx.send(frame).await.is_err() {
                break;
            }
        }
    });

    let metas: Vec<_> = blocks
        .iter()
        .map(|block| BlockSyncBlockMeta {
            height: block.coinbase_height().expect("test block has height"),
            hash: block.hash(),
            size: BlockSizeEstimate::Advertised(block_size(block)),
        })
        .collect();

    let drive = async {
        let mut submitted = std::collections::HashSet::new();
        while submitted.len() < FLOOD as usize {
            // Wait on the raw channel (no per-action timeout): if a regression
            // drops a body the reactor simply stops producing actions, and the
            // outer deadline below reports the failure.
            let Some(action) = actions.recv().await else {
                break;
            };
            match action {
                BlockSyncAction::QueryNeededBlocks { .. } => {
                    handle
                        .send(BlockSyncEvent::NeededBlocks(metas.clone()))
                        .await
                        .expect("needed metadata queues");
                }
                BlockSyncAction::SendMessage {
                    msg:
                        BlockSyncMessage::GetBlocks {
                            start_height,
                            count,
                        },
                    ..
                } => {
                    for height in start_height.0..start_height.0 + count {
                        feed_tx.send(height).expect("feeder task stays open");
                    }
                }
                BlockSyncAction::SubmitBlock { block, .. } => {
                    submitted.insert(block.coinbase_height().expect("submitted block has height"));
                }
                _ => {}
            }
        }
        submitted
    };

    let submitted = tokio::time::timeout(Duration::from_secs(20), drive)
        .await
        .expect(
        "flooded bodies must all reach the apply stage; a dropped inbound body wedges block sync",
    );

    let expected: std::collections::HashSet<_> = (1..=FLOOD).map(block::Height).collect();
    assert_eq!(
        submitted, expected,
        "every solicited body in the flood must be submitted exactly once, with no drops"
    );

    feeder.abort();
    reactor_task.abort();
}

#[tokio::test]
async fn reactor_restarted_at_genesis_queries_and_schedules_without_tip_change() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(3), blocks[2].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    match next_action(&mut actions).await {
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip,
            best_header_tip,
        } => {
            assert_eq!(verified_block_tip, block::Height(0));
            assert_eq!(best_header_tip, block::Height(3));
        }
        action => panic!("restart from genesis must query missing bodies, got {action:?}"),
    }

    let (peer, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        67,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer, block::Height(1), 3),
        "restart from genesis must schedule scratch body sync from height 1"
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[0].hash());
                assert_eq!(block.coinbase_height(), Some(block::Height(1)));
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before first scratch submit: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_accepts_blocks_done_after_completed_range() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        68,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    tip_tx
        .send((block::Height(2), blocks[1].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[0])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer.clone(), block::Height(1), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before submit: {action:?}"),
        }
    }

    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(1),
                returned: 1,
            }
            .encode_frame()
            .expect("BlocksDone encodes"),
        )
        .await
        .expect("BlocksDone queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(200), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior {
            peer: action_peer,
            reason,
        } = action
        {
            assert_ne!(
                (action_peer, reason),
                (peer.clone(), BlockSyncMisbehavior::UnsolicitedDone),
                "a valid terminator after a completed block response must not be scored"
            );
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_retries_missing_heights_after_partial_blocks_done() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        69,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer.clone(), block::Height(1), 3)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before first submit: {action:?}"),
        }
    }

    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(1),
                returned: 1,
            }
            .encode_frame()
            .expect("BlocksDone encodes"),
        )
        .await
        .expect("BlocksDone queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer, block::Height(2), 2),
        "partial responses must retry the contiguous missing suffix"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_does_not_retry_missing_height_already_in_flight() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.fanout = 2;
    config.expected_peers = 0;

    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(3), blocks[2].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (_peer_a, _inbound_a, _outbound_a) = connect_peer_with_status(
        &service,
        &mut actions,
        70,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (_peer_b, _inbound_b, _outbound_b) = connect_peer_with_status(
        &service,
        &mut actions,
        71,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");
    let first_request = wait_for_getblocks(&mut actions).await;
    let second_request = wait_for_getblocks(&mut actions).await;
    assert_eq!((first_request.1, first_request.2), (block::Height(1), 3));
    assert_eq!((second_request.1, second_request.2), (block::Height(1), 3));
    assert_ne!(
        first_request.0, second_request.0,
        "fanout should assign the same range to two peers",
    );

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: first_request.0.clone(),
            msg: BlockSyncMessage::Block(blocks[0].clone()),
        })
        .await
        .expect("first body queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before first submit: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: first_request.0.clone(),
            msg: BlockSyncMessage::BlocksDone {
                start_height: block::Height(1),
                returned: 1,
            },
        })
        .await
        .expect("BlocksDone queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(100), actions.recv()).await
    {
        match action {
            BlockSyncAction::SendMessage {
                msg: BlockSyncMessage::GetBlocks { start_height, count },
                ..
            } => panic!(
                "partial response retried {start_height:?}/{count} while another peer had it in flight"
            ),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action after partial BlocksDone: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn checkpoint_hole_disconnect_retries_first_missing_height_with_fresh_peer() {
    // Worst-case reservation caps a request at ~16 blocks (down from 128), so the
    // priming prefix is kept short enough to submit within the priming window
    // while still placing the hole behind several scheduled requests.
    const FIRST_NEEDED: u32 = 801;
    const PREFIX_END: u32 = 864;
    const HOLE_START: u32 = 865;
    const HOLE_END: u32 = 872;
    const LAST_METADATA: u32 = 933;
    const BEST_HEADER_TIP: u32 = 10_400;

    let blocks = fake_blocks_in_range(FIRST_NEEDED, LAST_METADATA);
    let block_at = |height: u32| -> Arc<block::Block> {
        let index =
            usize::try_from(height - FIRST_NEEDED).expect("test height is inside block vector");
        blocks[index].clone()
    };
    let metas: Vec<_> = blocks.iter().map(block_meta).collect();
    let prefix: std::collections::HashSet<_> =
        (FIRST_NEEDED..=PREFIX_END).map(block::Height).collect();
    let sparse_above_hole: std::collections::HashSet<_> = [873, 888, 905, 920]
        .into_iter()
        .map(block::Height)
        .collect();

    let mut config = immediate_body_download_config();
    config.fanout = 1;
    config.max_inflight_block_bytes = u64::MAX;
    config.request_timeout = Duration::from_secs(300);
    config.peer_limits.max_outbound_peers = 1;
    config.peer_limits.inbound_queue_depth = 128;
    // Worst-case reservation caps a request at ~16 blocks, so the prefix needs
    // more concurrent requests; keep the outbound queue wide enough that a fill
    // pass never overflows it and cancels the peer.
    config.peer_limits.outbound_queue_depth = 128;

    let (_tip_tx, tip_rx) = watch::channel((block::Height(BEST_HEADER_TIP), block::Hash([10; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(800),
            verified_block_tip: block::Height(800),
            verified_block_hash: block::Hash([8; 32]),
        },
        (block::Height(BEST_HEADER_TIP), block::Hash([10; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    let (old_peer, old_inbound, _old_outbound) = connect_peer_with_status_message(
        &service,
        &mut actions,
        70,
        BlockSyncStatus {
            servable_low: block::Height(1),
            servable_high: block::Height(LAST_METADATA),
            tip_hash: block_at(LAST_METADATA).hash(),
            max_blocks_per_response: MAX_BS_BLOCKS_PER_REQUEST,
            // Worst-case reservation caps a request at `max_response_bytes /
            // MAX_BLOCK_BYTES` (~16) blocks, so allow more concurrent requests to
            // cover the checkpoint prefix within the priming window.
            max_inflight_requests: 8,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        },
    )
    .await;
    handle
        .send(BlockSyncEvent::NeededBlocks(metas.clone()))
        .await
        .expect("checkpoint metadata queues after peer connection");

    let (feed_tx, mut feed_rx) = mpsc::unbounded_channel::<u32>();
    let blocks_for_feeder = blocks.clone();
    let feeder = tokio::spawn(async move {
        while let Some(height) = feed_rx.recv().await {
            let index = usize::try_from(height - FIRST_NEEDED)
                .expect("fed test height is inside block vector");
            let frame = BlockSyncMessage::Block(blocks_for_feeder[index].clone())
                .encode_frame()
                .expect("block frame encodes");
            if old_inbound.send(frame).await.is_err() {
                break;
            }
        }
    });

    let mut requests = Vec::new();
    let mut submitted = std::collections::HashSet::new();
    let primed = tokio::time::timeout(Duration::from_secs(40), async {
        while requests.len() < 4 || !prefix.is_subset(&submitted) {
            let action = actions
                .recv()
                .await
                .expect("block-sync action channel should stay open");
            match action {
                BlockSyncAction::QueryNeededBlocks {
                    verified_block_tip,
                    best_header_tip,
                } => {
                    assert_eq!(verified_block_tip, block::Height(800));
                    assert_eq!(best_header_tip, block::Height(BEST_HEADER_TIP));
                    handle
                        .send(BlockSyncEvent::NeededBlocks(metas.clone()))
                        .await
                        .expect("checkpoint metadata queues");
                }
                BlockSyncAction::SendMessage {
                    peer,
                    msg:
                        BlockSyncMessage::GetBlocks {
                            start_height,
                            count,
                        },
                } if peer == old_peer => {
                    requests.push((start_height, count));
                    let end_height = start_height
                        .0
                        .checked_add(count)
                        .expect("test request height range fits u32");
                    for height in start_height.0..end_height {
                        let height = block::Height(height);
                        if height.0 <= PREFIX_END || sparse_above_hole.contains(&height) {
                            feed_tx.send(height.0).expect("feeder task stays open");
                        }
                    }
                }
                BlockSyncAction::SubmitBlock { block, .. } => {
                    let height = block.coinbase_height().expect("submitted block has height");
                    assert!(
                        submitted.insert(height),
                        "height {height:?} was submitted more than once before the hole retry"
                    );
                }
                BlockSyncAction::SendMessage { .. } => {}
                action => panic!("unexpected action while priming checkpoint hole: {action:?}"),
            }
        }
    })
    .await;
    assert!(
        primed.is_ok(),
        "checkpoint-hole priming timed out: requests={requests:?}, submitted={} of {}",
        submitted.len(),
        prefix.len()
    );

    assert!(
        requests
            .iter()
            .any(|(start, count)| *start <= block::Height(HOLE_START)
                && start.0.saturating_add(*count) > HOLE_END),
        "the initial in-flight requests must cover the missing checkpoint hole"
    );

    service.remove_peer(&old_peer);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.peer_snapshot().outbound_peers == 0 && service.peer_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect releases the old block-sync peer slot");

    let (new_peer, _new_inbound, _new_outbound) = connect_peer_with_status_message(
        &service,
        &mut actions,
        71,
        BlockSyncStatus {
            servable_low: block::Height(1),
            servable_high: block::Height(LAST_METADATA),
            tip_hash: block_at(LAST_METADATA).hash(),
            max_blocks_per_response: MAX_BS_BLOCKS_PER_REQUEST,
            max_inflight_requests: 8,
            max_response_bytes: MAX_BS_RESPONSE_BYTES,
        },
    )
    .await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let action = actions
                .recv()
                .await
                .expect("block-sync action channel should stay open");
            match action {
                BlockSyncAction::QueryNeededBlocks { .. } => {
                    handle
                        .send(BlockSyncEvent::NeededBlocks(metas.clone()))
                        .await
                        .expect("post-disconnect metadata queues");
                }
                BlockSyncAction::SendMessage {
                    peer,
                    msg:
                        BlockSyncMessage::GetBlocks {
                            start_height,
                            count,
                        },
                } if peer == new_peer => {
                    assert_eq!(
                    start_height,
                    block::Height(HOLE_START),
                    "after a partial response disconnect, the first fresh request must fill the \
                     checkpoint hole instead of jumping above it or reusing a stale assignment"
                );
                    assert!(count >= 1);
                    break;
                }
                BlockSyncAction::SendMessage { .. } => {}
                BlockSyncAction::SubmitBlock { block, .. } => {
                    let height = block.coinbase_height().expect("submitted block has height");
                    assert!(
                        submitted.insert(height),
                        "height {height:?} was resubmitted before the checkpoint hole was retried"
                    );
                }
                action => {
                    panic!("unexpected action while waiting for checkpoint-hole retry: {action:?}")
                }
            }
        }
    })
    .await
    .expect("fresh peer should request the checkpoint hole after disconnect");

    feeder.abort();
    reactor_task.abort();
}

#[tokio::test]
async fn reactor_reset_mid_download_drops_stale_anchors_and_releases_budget() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        47,
        block::Height(3),
        blocks[2].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(10_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(3),
                hash: blocks[2].hash(),
                size: BlockSizeEstimate::Advertised(10_000),
            },
        ]))
        .await
        .expect("old-fork needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 2)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("out-of-order old-fork block queues");

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        }))
        .await
        .expect("reset event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(2),
            hash: block::Hash([92; 32]),
            size: BlockSizeEstimate::Advertised(20_000),
        }]))
        .await
        .expect("new-fork needed metadata queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SendMessage {
                peer,
                msg:
                    BlockSyncMessage::GetBlocks {
                        start_height,
                        count,
                    },
            } => {
                assert_eq!(
                    (peer, start_height, count),
                    (peer_id, block::Height(2), 1),
                    "a full-budget new fork request can only be scheduled if stale bytes were released"
                );
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::Misbehavior {
                reason: BlockSyncMisbehavior::UnsolicitedBlock,
                ..
            } => {}
            action => panic!("unexpected action before new fork request: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_forward_reset_preserves_submitted_successor_body() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        49,
        block::Height(3),
        blocks[2].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[1]),
            block_meta(&blocks[2]),
        ]))
        .await
        .expect("initial needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(2), 2)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[1].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("contiguous body queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[1].hash());
                break;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before first submit: {action:?}"),
        }
    }

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("successor body queues");
    let successor_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), blocks[2].hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before successor submit: {action:?}"),
        }
    };

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(2),
            verified_block_hash: blocks[1].hash(),
        }))
        .await
        .expect("forward reset event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(3),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: successor_token,
            height: block::Height(3),
            hash: blocks[2].hash(),
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("successor apply result queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(3),
            best_header_tip: block::Height(3),
        }
    ) {}

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_forward_reset_preserves_future_outstanding_body() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        72,
        block::Height(3),
        blocks[2].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[1]),
            block_meta(&blocks[2]),
        ]))
        .await
        .expect("initial needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 2)
    );

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(2),
            verified_block_hash: blocks[1].hash(),
        }))
        .await
        .expect("forward reset event queues");

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("successor body queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(block.hash(), blocks[2].hash());
                break;
            }
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_ne!(
                    (peer, reason),
                    (peer_id.clone(), BlockSyncMisbehavior::UnsolicitedBlock),
                    "forward reset must not drop a still-active successor request"
                );
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            BlockSyncAction::QueryBlocksByHeightRange { .. } => {}
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_forward_reset_preserves_buffered_successor_body() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        73,
        block::Height(3),
        blocks[2].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[1]),
            block_meta(&blocks[2]),
        ]))
        .await
        .expect("initial needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 2)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("successor body queues");
    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(2),
                returned: 2,
            }
            .encode_frame()
            .expect("BlocksDone encodes"),
        )
        .await
        .expect("BlocksDone queues");

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(2),
            verified_block_hash: blocks[1].hash(),
        }))
        .await
        .expect("forward reset event queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(
                    block.hash(),
                    blocks[2].hash(),
                    "buffered successor must submit without a duplicate download"
                );
                break;
            }
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_ne!(
                    (peer, reason),
                    (peer_id.clone(), BlockSyncMisbehavior::UnsolicitedBlock),
                    "forward reset must not drop a buffered successor body"
                );
            }
            BlockSyncAction::SendMessage { .. }
            | BlockSyncAction::QueryNeededBlocks { .. }
            | BlockSyncAction::QueryBlocksByHeightRange { .. } => {}
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_destructive_forward_reset_does_not_rerequest_same_hash_in_flight_apply() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        68,
        block::Height(2),
        blocks[1].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[0]),
            block_meta(&blocks[1]),
        ]))
        .await
        .expect("initial needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 2)
    );

    for block in blocks.iter().take(2) {
        inbound_tx
            .send(
                BlockSyncMessage::Block(block.clone())
                    .encode_frame()
                    .expect("block encodes"),
            )
            .await
            .expect("block queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < 2 {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => submitted.push((
                block.coinbase_height().expect("test block has height"),
                token,
            )),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before submitted bodies: {action:?}"),
        }
    }
    assert_eq!(
        submitted
            .iter()
            .map(|(height, _)| *height)
            .collect::<Vec<_>>(),
        vec![block::Height(1), block::Height(2)]
    );

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: block::Hash([99; 32]),
        }))
        .await
        .expect("destructive forward reset queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(2),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[1])]))
        .await
        .expect("same-hash needed metadata queues");
    let quiet = tokio::time::timeout(Duration::from_millis(100), actions.recv()).await;
    assert!(
        quiet.is_err(),
        "same-hash in-flight apply released by reset must not be re-requested",
    );

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(2),
            hash: block::Hash([42; 32]),
            size: BlockSizeEstimate::Advertised(block_size(&blocks[1])),
        }]))
        .await
        .expect("new-fork needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(2), 1),
        "different hash at the same height must still schedule after a fork reset"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_stale_apply_completion_after_resubmit() {
    let config = immediate_body_download_config();
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    let block_hash = block.hash();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(61);
    let (inbound_tx, inbound_rx) = framed_channel(16);
    let (outbound_tx, _outbound_rx) = framed_channel(16);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block_hash,
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(1), block_hash))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block_hash,
            size: BlockSizeEstimate::Advertised(block_size(&block)),
        }]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(block.clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("first body frame queues");
    let stale_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), block_hash);
                break token;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before first submit: {action:?}"),
        }
    };

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        }))
        .await
        .expect("reset event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block_hash,
            size: BlockSizeEstimate::Advertised(block_size(&block)),
        }]))
        .await
        .expect("needed metadata queues after reset");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(1), 1)
    );
    inbound_tx
        .send(
            BlockSyncMessage::Block(block.clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("second body frame queues");
    let current_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), block_hash);
                break token;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before second submit: {action:?}"),
        }
    };
    assert_ne!(stale_token, current_token);

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: stale_token,
            height: block::Height(1),
            hash: block_hash,
            result: BlockApplyResult::Duplicate,
            local_frontier: None,
        })
        .await
        .expect("stale apply-finished event queues");
    let no_query_from_stale_completion = tokio::time::timeout(Duration::from_millis(100), async {
        while let Some(action) = actions.recv().await {
            if matches!(action, BlockSyncAction::QueryNeededBlocks { .. }) {
                panic!("stale apply completion released the current submission");
            }
        }
    })
    .await;
    assert!(
        no_query_from_stale_completion.is_err(),
        "reactor should keep the current submission after a stale completion"
    );

    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: current_token,
            height: block::Height(1),
            hash: block_hash,
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("current apply-finished event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_fast_forward_reset_clears_buffered_bodies_and_releases_budget() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        50,
        block::Height(4),
        block::Hash([4; 32]),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 2).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(3),
        }
    ) {}

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: blocks[1].hash(),
                size: BlockSizeEstimate::Advertised(10_000),
            },
            BlockSyncBlockMeta {
                height: block::Height(3),
                hash: blocks[2].hash(),
                size: BlockSizeEstimate::Advertised(10_000),
            },
        ]))
        .await
        .expect("initial needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 2)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("out-of-order body queues");

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(3),
            verified_block_hash: blocks[2].hash(),
        }))
        .await
        .expect("fast-forward reset event queues");

    tip_tx
        .send((block::Height(4), block::Hash([4; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(3),
            best_header_tip: block::Height(4),
        }
    ) {}
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        handle.peer_snapshot().outbound_peers,
        1,
        "caught-up reset keeps the previous block-sync peer available for serving",
    );
    assert_eq!(service.peer_count(), 1);

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(4),
            hash: block::Hash([4; 32]),
            size: BlockSizeEstimate::Advertised(20_000),
        }]))
        .await
        .expect("post-reset needed metadata queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(4), 1),
        "a full-budget request after fast-forward Reset requires releasing buffered bytes"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_fuzzes_arrival_order_across_fork_parent_first() {
    #[derive(Copy, Clone)]
    enum ForkBody {
        Old(usize),
        New(usize),
    }

    let cases = vec![
        (
            "stale-high-before-reset",
            vec![3],
            vec![],
            vec![ForkBody::Old(2), ForkBody::New(3), ForkBody::New(2)],
        ),
        (
            "old-prefix-before-reset",
            vec![2, 1],
            vec![],
            vec![ForkBody::Old(3), ForkBody::New(2), ForkBody::New(3)],
        ),
        (
            "stale-before-new-needed",
            vec![2],
            vec![3],
            vec![ForkBody::New(2), ForkBody::Old(2), ForkBody::New(3)],
        ),
        (
            "new-out-of-order-with-stale-tail",
            vec![],
            vec![2],
            vec![ForkBody::New(3), ForkBody::Old(3), ForkBody::New(2)],
        ),
    ];

    for (case, old_before_reset, old_before_new_needed, after_new_needed) in cases {
        let mut config = ZakuraBlockSyncConfig {
            max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 3,
            ..immediate_body_download_config()
        };
        config.peer_limits.outbound_queue_depth = 16;
        let old_blocks = mainnet_blocks_1_to_3();
        let new_blocks = vec![
            forked_block(&old_blocks[0], 101),
            forked_block(&old_blocks[1], 102),
            forked_block(&old_blocks[2], 103),
        ];
        let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
        let startup = BlockSyncStartup::new(
            BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(0),
                verified_block_hash: block::Hash([0; 32]),
            },
            (block::Height(0), block::Hash([0; 32])),
            tip_rx,
            config.clone(),
        );
        let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
        let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
        let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
            &service,
            &mut actions,
            51,
            block::Height(4),
            old_blocks[2].hash(),
            1,
            u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES * 3).unwrap_or(u32::MAX),
        )
        .await;

        tip_tx
            .send((block::Height(3), old_blocks[2].hash()))
            .expect("tip watch is live");
        while !matches!(
            next_action(&mut actions).await,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(0),
                best_header_tip: block::Height(3),
            }
        ) {}
        handle
            .send(BlockSyncEvent::NeededBlocks(
                old_blocks
                    .iter()
                    .map(|block| BlockSyncBlockMeta {
                        height: block.coinbase_height().expect("test block has height"),
                        hash: block.hash(),
                        size: BlockSizeEstimate::Advertised(block_size(block)),
                    })
                    .collect(),
            ))
            .await
            .expect("old-fork needed metadata queues");
        assert_eq!(
            wait_for_getblocks(&mut actions).await,
            (peer_id.clone(), block::Height(1), 3),
            "{case}: old fork request schedules"
        );

        let mut submitted_tip = block::Height(0);
        for height in old_before_reset {
            inbound_tx
                .send(
                    BlockSyncMessage::Block(old_blocks[height - 1].clone())
                        .encode_frame()
                        .expect("old-fork block encodes"),
                )
                .await
                .expect("old-fork block queues");
            drain_parent_first_actions(&mut actions, &mut submitted_tip, None).await;
        }

        handle
            .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(1),
                verified_block_hash: new_blocks[0].hash(),
            }))
            .await
            .expect("reset event queues");
        while !matches!(
            next_action(&mut actions).await,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(1),
                best_header_tip: block::Height(3),
            }
        ) {}
        submitted_tip = block::Height(1);

        tip_tx
            .send((block::Height(3), new_blocks[2].hash()))
            .expect("tip watch is live");
        while !matches!(
            next_action(&mut actions).await,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(1),
                best_header_tip: block::Height(3),
            }
        ) {}

        for height in old_before_new_needed {
            inbound_tx
                .send(
                    BlockSyncMessage::Block(old_blocks[height - 1].clone())
                        .encode_frame()
                        .expect("stale old-fork block encodes"),
                )
                .await
                .expect("stale old-fork block queues");
            drain_parent_first_actions(&mut actions, &mut submitted_tip, Some(&new_blocks)).await;
        }

        handle
            .send(BlockSyncEvent::NeededBlocks(vec![
                BlockSyncBlockMeta {
                    height: block::Height(2),
                    hash: new_blocks[1].hash(),
                    size: BlockSizeEstimate::Advertised(block_size(&new_blocks[1])),
                },
                BlockSyncBlockMeta {
                    height: block::Height(3),
                    hash: new_blocks[2].hash(),
                    size: BlockSizeEstimate::Advertised(block_size(&new_blocks[2])),
                },
            ]))
            .await
            .expect("new-fork needed metadata queues");
        assert_eq!(
            wait_for_getblocks(&mut actions).await,
            (peer_id.clone(), block::Height(2), 2),
            "{case}: new fork request schedules after reset"
        );

        for body in after_new_needed {
            let block = match body {
                ForkBody::Old(height) => old_blocks[height - 1].clone(),
                ForkBody::New(height) => new_blocks[height - 1].clone(),
            };
            inbound_tx
                .send(
                    BlockSyncMessage::Block(block)
                        .encode_frame()
                        .expect("fork body encodes"),
                )
                .await
                .expect("fork body queues");
            drain_parent_first_actions(&mut actions, &mut submitted_tip, Some(&new_blocks)).await;
        }
        assert_eq!(
            submitted_tip,
            block::Height(3),
            "{case}: new fork bodies submit parent-first through height 3"
        );

        handle
            .send(BlockSyncEvent::ChainTipGrow(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(3),
                verified_block_hash: new_blocks[2].hash(),
            }))
            .await
            .expect("post-submit grow event queues");
        tip_tx
            .send((block::Height(4), block::Hash([4; 32])))
            .expect("tip watch is live");
        while !matches!(
            next_action(&mut actions).await,
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: block::Height(3),
                best_header_tip: block::Height(4),
            }
        ) {}
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            handle.peer_snapshot().outbound_peers,
            1,
            "{case}: caught-up fork handling keeps the old block-sync peer available for serving",
        );
        assert_eq!(service.peer_count(), 1);
        handle
            .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
                height: block::Height(4),
                hash: block::Hash([4; 32]),
                size: BlockSizeEstimate::Advertised(60_000),
            }]))
            .await
            .expect("post-fuzz needed metadata queues");
        assert_eq!(
            wait_for_getblocks(&mut actions).await,
            (peer_id, block::Height(4), 1),
            "{case}: byte budget returns to baseline after reset and submissions"
        );

        reactor_task.abort();
    }
}

#[tokio::test]
async fn reactor_competing_fork_download_switches_to_current_header_hashes() {
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        immediate_body_download_config(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(
        immediate_body_download_config(),
        handle.clone(),
    );
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        48,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    tip_tx
        .send((block::Height(3), blocks[2].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[1]),
            block_meta(&blocks[2]),
        ]))
        .await
        .expect("old fork metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 2)
    );

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        }))
        .await
        .expect("reset event queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(2),
            hash: block::Hash([222; 32]),
            size: BlockSizeEstimate::Advertised(block_size(&blocks[1])),
        }]))
        .await
        .expect("new fork metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[1].clone())
                .encode_frame()
                .expect("old-fork body encodes"),
        )
        .await
        .expect("old-fork body queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, peer_id);
                assert_eq!(reason, BlockSyncMisbehavior::InvalidBlock);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before stale body rejection: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_legacy_commit_dedups_inflight_request_and_reuses_budget() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, _inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        49,
        block::Height(2),
        blocks[1].hash(),
        1,
        u32::try_from(BS_PER_BLOCK_WORST_CASE_BYTES).unwrap_or(u32::MAX),
    )
    .await;

    tip_tx
        .send((block::Height(2), blocks[1].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: blocks[0].hash(),
            size: BlockSizeEstimate::Advertised(10_000),
        }]))
        .await
        .expect("first needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 1)
    );

    handle
        .send(BlockSyncEvent::ChainTipGrow(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        }))
        .await
        .expect("legacy commit grow event queues");

    tip_tx
        .send((block::Height(2), blocks[1].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(1),
            best_header_tip: block::Height(2),
        }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(2),
            hash: blocks[1].hash(),
            size: BlockSizeEstimate::Advertised(10_000),
        }]))
        .await
        .expect("second needed metadata queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(2), 1),
        "legacy commit must release the duplicate in-flight reservation"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_treats_duplicate_buffered_blocks_as_benign() {
    let config = immediate_body_download_config();
    let blocks = [
        mainnet_block(&BLOCK_MAINNET_1_BYTES),
        mainnet_block(&BLOCK_MAINNET_2_BYTES),
    ];
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(44);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);
    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(2),
                tip_hash: blocks[1].hash(),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status frame queues");

    tip_tx
        .send((block::Height(2), blocks[1].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks
                .iter()
                .map(|block| BlockSyncBlockMeta {
                    height: block.coinbase_height().expect("test block has height"),
                    hash: block.hash(),
                    size: BlockSizeEstimate::Advertised(block_size(block)),
                })
                .collect(),
        ))
        .await
        .expect("needed metadata queues");

    let (action_peer, start_height, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(action_peer, peer_id);
    assert_eq!(start_height, block::Height(1));
    assert_eq!(count, 2);

    for block in [&blocks[1], &blocks[1], &blocks[0]] {
        inbound_tx
            .send(
                BlockSyncMessage::Block(block.clone())
                    .encode_frame()
                    .expect("block encodes"),
            )
            .await
            .expect("block queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < 2 {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => submitted.push(
                block
                    .coinbase_height()
                    .expect("submitted test block has height"),
            ),
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::Misbehavior { reason, .. } => {
                panic!("duplicate buffered body was misclassified: {reason:?}")
            }
            action => panic!("unexpected action before submit: {action:?}"),
        }
    }
    assert_eq!(submitted, vec![block::Height(1), block::Height(2)]);

    let quiet = tokio::time::timeout(Duration::from_millis(100), async {
        while let Some(action) = actions.recv().await {
            if let BlockSyncAction::Misbehavior { reason, .. } = action {
                panic!("duplicate buffered body was misclassified after submit: {reason:?}");
            }
        }
    })
    .await;
    assert!(quiet.is_err());

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_accepts_rapid_status_growth_without_spam_score() {
    let config = ZakuraBlockSyncConfig::default();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    drop(tip_tx);
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle);
    let peer_id = peer(46);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);

    for servable_high in [block::Height(1), block::Height(2)] {
        let hash_byte = u8::try_from(servable_high.0).expect("test height fits in u8");
        inbound_tx
            .send(
                BlockSyncMessage::Status(BlockSyncStatus {
                    servable_low: block::Height(1),
                    servable_high,
                    tip_hash: block::Hash([hash_byte; 32]),
                    max_blocks_per_response: 4,
                    max_inflight_requests: 1,
                    max_response_bytes: MAX_BS_RESPONSE_BYTES,
                })
                .encode_frame()
                .expect("status encodes"),
            )
            .await
            .expect("status frame queues");
    }

    let quiet = tokio::time::timeout(Duration::from_millis(100), async {
        while let Some(action) = actions.recv().await {
            if let BlockSyncAction::Misbehavior { reason, .. } = action {
                panic!("rapid status growth was misclassified: {reason:?}");
            }
        }
    })
    .await;
    assert!(quiet.is_err());

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_redundant_status_burst_without_spam_score() {
    let config = ZakuraBlockSyncConfig::default();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    drop(tip_tx);
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle);
    let peer_id = peer(47);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);

    let status = BlockSyncMessage::Status(BlockSyncStatus {
        servable_low: block::Height(1),
        servable_high: block::Height(1),
        tip_hash: block::Hash([1; 32]),
        max_blocks_per_response: 4,
        max_inflight_requests: 1,
        max_response_bytes: MAX_BS_RESPONSE_BYTES,
    })
    .encode_frame()
    .expect("status encodes");
    for _ in 0..3 {
        inbound_tx
            .send(status.clone())
            .await
            .expect("redundant status queues");
    }

    let quiet = tokio::time::timeout(Duration::from_millis(100), async {
        while let Some(action) = actions.recv().await {
            if let BlockSyncAction::Misbehavior { reason, .. } = action {
                panic!("redundant status burst was misclassified: {reason:?}");
            }
        }
    })
    .await;
    assert!(quiet.is_err());

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_rejects_block_hash_mismatch_without_hard_drop_for_size_mismatch() {
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        immediate_body_download_config(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(
        immediate_body_download_config(),
        handle.clone(),
    );
    let peer = peer(41);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block::Hash([1; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status queues");
    tip_tx
        .send((block::Height(1), block::Hash([9; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block::Hash([9; 32]),
            size: BlockSizeEstimate::Advertised(1),
        }]))
        .await
        .expect("needed metadata queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::SendMessage {
            msg: BlockSyncMessage::GetBlocks { .. },
            ..
        }
    ) {}
    while !matches!(
        BlockSyncMessage::decode_frame(
            tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
                .await
                .expect("outbound frame arrives")
                .expect("outbound channel is live")
        )
        .expect("frame decodes"),
        BlockSyncMessage::GetBlocks { .. }
    ) {}

    inbound_tx
        .send(
            BlockSyncMessage::Block(mainnet_block(&BLOCK_MAINNET_1_BYTES))
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { reason, .. } => {
                assert_eq!(reason, BlockSyncMisbehavior::InvalidBlock);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before invalid-block report: {action:?}"),
        }
    }

    reactor_task.abort();
}

// SECURITY AUDIT (candidate claude-block-sync-source-task-unwired /
// trace-block-sync-source-task-unwired-source-task /
// subset-panic-runtime-containment-block-sync-idle-source-task): SR-4
// cleanup/anti-drift for the outbound block-sync send path.
//
// Production block-sync scheduling sends outbound `GetBlocks` *directly* through
// `BlockSyncPeerSession` (`reactor::schedule` -> `try_send_get_blocks`). The
// per-peer `BlockSyncSource` action pump (`BlockSyncPeerRecord::actions`) is
// test-only scaffolding with no production producer, and
// `drive_block_sync_actions` deliberately ignores the reactor's duplicate
// `SendMessage` action to avoid double-sending. The audit flagged the risk that
// the per-peer source is dead production scaffolding (per-peer overhead) and a
// latent double-send footgun if it were ever wired naively.
//
// This production-shaped scheduling test locks in the single-sourced outbound
// contract: one scheduled request yields EXACTLY ONE outbound `GetBlocks` frame
// (the authoritative direct session send), never a second copy from a per-peer
// source pump. It proves the outbound behavior is unchanged by the source task
// being idle/test-only, and guards against a future double-send regression.
#[tokio::test]
async fn scheduled_get_blocks_is_sent_once_via_session_not_duplicated_by_source() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Admit a peer through the production `add_peer` path with an observable
    // outbound transport channel.
    let peer = peer(57);
    let (inbound_tx, inbound_rx) = framed_channel(16);
    let (outbound_tx, mut outbound_rx) = framed_channel(16);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    // The peer can serve exactly one block above our tip; publish the header tip
    // and the needed metadata so the reactor schedules exactly one request.
    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block::Hash([9; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status queues");
    tip_tx
        .send((block::Height(1), block::Hash([9; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block::Hash([9; 32]),
            size: BlockSizeEstimate::Advertised(1),
        }]))
        .await
        .expect("needed metadata queues");
    // The reactor emits the duplicate `SendMessage` action on the global channel
    // (which the production driver ignores). Wait for it so we know the
    // authoritative direct send through `BlockSyncPeerSession` already happened.
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::SendMessage {
            msg: BlockSyncMessage::GetBlocks { .. },
            ..
        }
    ) {}

    // Drain the outbound stream for a bounded idle window and count `GetBlocks`
    // frames. A single scheduled request must produce exactly one outbound
    // `GetBlocks` (the direct session send); a second copy would mean the
    // per-peer source action pump is also (double-)sending on the wire.
    let mut get_blocks = 0usize;
    let mut frames = 0usize;
    while frames < 16 {
        match tokio::time::timeout(Duration::from_millis(300), outbound_rx.recv()).await {
            Ok(Some(frame)) => {
                frames += 1;
                if matches!(
                    BlockSyncMessage::decode_frame(frame).expect("outbound frame decodes"),
                    BlockSyncMessage::GetBlocks { .. }
                ) {
                    get_blocks += 1;
                }
            }
            _ => break,
        }
    }
    assert_eq!(
        get_blocks, 1,
        "one scheduled request must produce exactly one outbound GetBlocks via \
         BlockSyncPeerSession; a different count means the per-peer source pump is \
         (double-)sending in addition to the authoritative direct path",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_scores_peer_whose_invalid_body_is_rejected_by_consensus() {
    let request_bytes: u32 = 10_000;
    let config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: BS_PER_BLOCK_WORST_CASE_BYTES * 2,
        ..immediate_body_download_config()
    };

    let blocks = mainnet_blocks_1_to_3();
    // A body that keeps block 1's header (so it passes the reactor's hash and
    // height gates) but carries an extra transaction, so its merkle root no
    // longer matches the header. The reactor no longer recomputes the merkle
    // root at ingress, so this body reaches consensus, which rejects it.
    let bad_body = block_with_bad_merkle_root(&blocks[0], &blocks[1]);
    assert_eq!(bad_body.hash(), blocks[0].hash());

    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (bad_peer, bad_inbound, _bad_outbound) = connect_peer_with_status(
        &service,
        &mut actions,
        40,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    tip_tx
        .send((block::Height(1), blocks[0].hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: blocks[0].hash(),
            size: BlockSizeEstimate::Advertised(request_bytes),
        }]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (bad_peer.clone(), block::Height(1), 1)
    );

    bad_inbound
        .send(
            BlockSyncMessage::Block(bad_body)
                .encode_frame()
                .expect("bad block frame encodes"),
        )
        .await
        .expect("bad block frame queues");

    // The merkle-invalid body is no longer filtered at ingress: it is buffered
    // and submitted to consensus.
    let submit_token = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => {
                assert_eq!(block.hash(), blocks[0].hash());
                break token;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before invalid body submit: {action:?}"),
        }
    };

    // Consensus rejects the invalid body. The reactor must attribute the
    // rejection to the peer that delivered it and score the peer as misbehavior,
    // rather than silently rolling back scheduling state and letting the peer
    // keep feeding invalid bodies for needed heights.
    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token: submit_token,
            height: block::Height(1),
            hash: blocks[0].hash(),
            result: BlockApplyResult::Rejected,
            local_frontier: None,
        })
        .await
        .expect("apply-finished event queues");

    let mut scored = false;
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, bad_peer);
                assert_eq!(reason, BlockSyncMisbehavior::InvalidBlock);
                scored = true;
                break;
            }
            // Reaching the post-rejection re-query without a misbehavior report
            // means the peer was not scored.
            BlockSyncAction::QueryNeededBlocks { .. } => break,
            _ => {}
        }
    }
    assert!(
        scored,
        "a consensus apply rejection must score the peer that delivered the body",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_serves_committed_blocks_with_count_and_byte_clamps() {
    let blocks = mainnet_blocks_1_to_3();
    let block1_size = block_size(&blocks[0]);
    let mut config = ZakuraBlockSyncConfig {
        max_blocks_per_response: 2,
        max_response_bytes: block1_size,
        ..ZakuraBlockSyncConfig::default()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(4), block::Hash([4; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(3),
            verified_block_hash: blocks[2].hash(),
        },
        (block::Height(4), block::Hash([4; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, mut outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        60,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    inbound_tx
        .send(
            BlockSyncMessage::GetBlocks {
                start_height: block::Height(1),
                count: 10,
            }
            .encode_frame()
            .expect("GetBlocks frame encodes"),
        )
        .await
        .expect("GetBlocks frame queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                assert_eq!(peer, peer_id);
                assert_eq!(start, block::Height(1));
                assert_eq!(count, 2);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before block range query: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::BlockRangeResponseReady {
            peer: peer_id.clone(),
            start_height: block::Height(1),
            requested_count: 2,
            blocks: vec![
                (
                    block::Height(1),
                    blocks[0].clone(),
                    usize::try_from(block1_size).expect("block size fits usize"),
                ),
                (
                    block::Height(2),
                    blocks[1].clone(),
                    usize::try_from(block_size(&blocks[1])).expect("block size fits usize"),
                ),
            ],
        })
        .await
        .expect("served block response queues");

    assert_eq!(
        wait_for_outbound_block(&mut outbound_rx).await.hash(),
        blocks[0].hash()
    );
    assert_eq!(
        wait_for_outbound_blocks_done(&mut outbound_rx).await,
        (block::Height(1), 1),
        "max_response_bytes clamps the served response to one body"
    );

    inbound_tx
        .send(
            BlockSyncMessage::GetBlocks {
                start_height: block::Height(4),
                count: 1,
            }
            .encode_frame()
            .expect("GetBlocks frame encodes"),
        )
        .await
        .expect("above-tip GetBlocks frame queues");

    assert_eq!(
        wait_for_outbound_range_unavailable(&mut outbound_rx).await,
        (block::Height(4), 1)
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_never_serves_reorder_buffer_bodies() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(3), blocks[2].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, mut outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        61,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[2])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(3), 1)
    );
    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block frame encodes"),
        )
        .await
        .expect("block frame queues");

    let quiet = tokio::time::timeout(Duration::from_millis(50), async {
        while let Some(action) = actions.recv().await {
            if matches!(action, BlockSyncAction::SubmitBlock { .. }) {
                panic!("height 3 must stay buffered behind the height 2 gap");
            }
        }
    })
    .await;
    assert!(quiet.is_err());

    inbound_tx
        .send(
            BlockSyncMessage::GetBlocks {
                start_height: block::Height(3),
                count: 1,
            }
            .encode_frame()
            .expect("GetBlocks frame encodes"),
        )
        .await
        .expect("GetBlocks frame queues");

    assert_eq!(
        wait_for_outbound_range_unavailable(&mut outbound_rx).await,
        (block::Height(3), 1),
        "uncommitted reorder-buffer body must not be served"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_schedules_gap_below_buffered_reorder_run() {
    // Regression for the mainnet stuck-at-0 deadlock: a body run received above
    // an open gap must not starve the gap below it. The state reports every
    // header-known, body-missing height (it cannot see our in-memory reorder
    // buffer), so a re-query returns already-buffered heights too. With
    // multi-peer fanout the held range lingers in the scheduler queue. Because
    // `refresh_needed` builds one maximal contiguous range and `ensure` rejects
    // any range overlapping a queued one, the gap below the held run would never
    // be scheduled and `body_download_floor` would freeze forever while we
    // re-requested the already-held blocks. The reactor must drop already-held
    // heights from the needed set so the gap gets scheduled.
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.fanout = 3;
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(3), blocks[2].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        63,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    // Height 2 is momentarily not offered, so we fetch and buffer height 3 in
    // the reorder buffer above the open height-2 gap.
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[2])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(3), 1)
    );
    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block frame encodes"),
        )
        .await
        .expect("block frame queues");

    // Height 3 is now buffered in the reorder buffer and marked covered. Drain
    // to quiescence: this both lets the reactor finish processing the body and
    // asserts the core fix — a buffered (covered) height is NEVER re-requested.
    // The production deadlock re-requested the held run thousands of times via
    // the retry path (which bypasses the `needed`-set filter), pinning the queue
    // and every peer slot so the gap below the run never got a request.
    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(50), actions.recv()).await
    {
        match action {
            BlockSyncAction::SendMessage {
                msg: BlockSyncMessage::GetBlocks { start_height, .. },
                ..
            } => panic!("buffered (covered) height {start_height:?} must not be re-requested"),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            other => panic!("unexpected action after buffering block: {other:?}"),
        }
    }

    // The state still reports both 2 and 3 as body-missing because the reorder
    // buffer is invisible to it. With height 3 covered, the reactor must schedule
    // the gap at height 2 rather than staying trapped on the buffered run.
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[1]),
            block_meta(&blocks[2]),
        ]))
        .await
        .expect("needed metadata queues");

    let (got_peer, got_start, _count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(got_peer, peer_id);
    assert_eq!(
        got_start,
        block::Height(2),
        "reactor must schedule the gap at height 2 below the buffered reorder run",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_debounces_status_advertisements_on_serving_tip_change() {
    let mut config = ZakuraBlockSyncConfig {
        status_refresh_interval: Duration::from_secs(60),
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(4), block::Hash([4; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(4), block::Hash([4; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (_peer_id, _inbound_tx, mut outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        62,
        block::Height(4),
        block::Hash([4; 32]),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    assert!(matches!(
        next_outbound_message(&mut outbound_rx).await,
        BlockSyncMessage::Status(_)
    ));

    handle
        .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        }))
        .await
        .expect("unchanged frontier queues");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), outbound_rx.recv())
            .await
            .is_err(),
        "unchanged serving range must not advertise"
    );

    handle
        .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: block::Hash([1; 32]),
        }))
        .await
        .expect("changed frontier queues");
    match next_outbound_message(&mut outbound_rx).await {
        BlockSyncMessage::Status(status) => {
            assert_eq!(status.servable_high, block::Height(1));
            assert_eq!(handle.local_status().servable_high, block::Height(1));
        }
        msg => panic!("expected debounced Status after serving tip change, got {msg:?}"),
    }

    for height in [2, 3] {
        let hash_byte = u8::try_from(height).expect("test height fits in u8");
        handle
            .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
                finalized_height: block::Height(0),
                verified_block_tip: block::Height(height),
                verified_block_hash: block::Hash([hash_byte; 32]),
            }))
            .await
            .expect("burst frontier queues");
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(50), outbound_rx.recv())
            .await
            .is_err(),
        "rapid serving-tip changes must be debounced to one Status per window"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_retries_status_to_peer_without_status_when_local_status_unchanged() {
    let mut config = ZakuraBlockSyncConfig {
        status_refresh_interval: Duration::from_millis(50),
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, _actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle);
    let peer = peer(63);
    let (_inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer,
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    assert!(matches!(
        next_outbound_message(&mut outbound_rx).await,
        BlockSyncMessage::Status(_)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), outbound_rx.recv())
            .await
            .is_err(),
        "initial Status send must consume the peer refresh allowance"
    );
    assert!(matches!(
        next_outbound_message(&mut outbound_rx).await,
        BlockSyncMessage::Status(_)
    ));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_replies_to_status_after_status_send_allowance_reopens() {
    let mut config = ZakuraBlockSyncConfig {
        status_refresh_interval: Duration::from_millis(50),
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, _actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle);
    let peer = peer(64);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);

    service.add_peer(Peer::new_with_direction(
        peer,
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    assert!(matches!(
        next_outbound_message(&mut outbound_rx).await,
        BlockSyncMessage::Status(_)
    ));
    inbound_tx
        .send(
            BlockSyncMessage::Status(status())
                .encode_frame()
                .expect("inbound status frame encodes"),
        )
        .await
        .expect("inbound status queues");
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(60)).await;
    inbound_tx
        .send(
            BlockSyncMessage::Status(status())
                .encode_frame()
                .expect("inbound status frame encodes"),
        )
        .await
        .expect("second inbound status queues");

    assert!(matches!(
        next_outbound_message(&mut outbound_rx).await,
        BlockSyncMessage::Status(_)
    ));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_watch_converges_to_latest_valid_frontier() {
    let initial = test_frontier_update(0, 0, 0, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());

    exchange.publish_frontier(
        test_frontier_update(0, 0, 3, FrontierChange::HeaderAdvanced),
        "test",
    );
    exchange.publish_frontier(
        test_frontier_update(0, 0, 2, FrontierChange::HeaderAdvanced),
        "test",
    );
    exchange.publish_frontier(
        test_frontier_update(0, 0, 5, FrontierChange::HeaderAdvanced),
        "test",
    );
    exchange.publish_frontier(
        test_frontier_update(0, 0, 5, FrontierChange::HeaderAdvanced),
        "test",
    );

    let (_handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(5)).await;
    assert_eq!(
        exchange.current_frontier().frontier.best_header,
        test_frontier(5)
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_progress_retries_after_empty_needed_blocks() {
    let initial = test_frontier_update(0, 0, 0, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    exchange.publish_frontier(
        test_frontier_update(0, 0, 3, FrontierChange::HeaderAdvanced),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(3)).await;

    handle
        .send(BlockSyncEvent::NeededBlocks(Vec::new()))
        .await
        .expect("empty needed-blocks event queues");

    exchange.publish_frontier(
        test_frontier_update(0, 0, 4, FrontierChange::HeaderAdvanced),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(4)).await;

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_body_progress_retries_after_header_tip_stops() {
    let initial = test_frontier_update(0, 0, 0, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());
    let (_handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    exchange.publish_frontier(
        test_frontier_update(0, 0, 3, FrontierChange::HeaderAdvanced),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(3)).await;

    exchange.publish_frontier(
        test_frontier_update(0, 1, 0, FrontierChange::VerifiedGrow),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(1), block::Height(3)).await;

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_coalesced_header_advance_catches_body_frontier_up() {
    let initial = test_frontier_update(0, 0, 0, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());

    exchange.publish_frontier(
        test_frontier_update(0, 3, 0, FrontierChange::VerifiedGrow),
        "test",
    );
    exchange.publish_frontier(
        test_frontier_update(0, 0, 3, FrontierChange::HeaderAdvanced),
        "test",
    );

    let (handle, _actions, reactor_task) = spawn_block_sync_reactor(startup);

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let status = handle.local_status();
            if status.servable_high == block::Height(3) {
                assert_eq!(status.tip_hash, test_frontier(3).hash);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("coalesced header update catches the body frontier up");

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_ignores_stale_grow_but_accepts_reset() {
    let initial = test_frontier_update(0, 5, 10, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    wait_for_query_needed_blocks(&mut actions, block::Height(5), block::Height(10)).await;

    exchange.publish_frontier(
        test_frontier_update(0, 4, 10, FrontierChange::VerifiedGrow),
        "test",
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), actions.recv())
            .await
            .is_err(),
        "stale lower VerifiedGrow must not trigger a lower body query"
    );
    assert_eq!(handle.local_status().servable_high, block::Height(5));

    exchange.publish_frontier(
        test_frontier_update(0, 4, 0, FrontierChange::VerifiedReset),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(4), block::Height(10)).await;
    assert_eq!(handle.local_status().servable_high, block::Height(4));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_preserves_successor_work_across_stale_finalized_reset() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.peer_limits.outbound_queue_depth = 16;
    config.request_timeout = Duration::from_secs(300);

    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(2),
            verified_block_tip: block::Height(2),
            verified_block_hash: blocks[1].hash(),
        },
        (block::Height(3), blocks[2].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        72,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[2])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(3), 1)
    );

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[2].clone())
                .encode_frame()
                .expect("block frame encodes"),
        )
        .await
        .expect("block frame queues");

    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::SubmitBlock { .. }
    ) {}

    handle
        .send(BlockSyncEvent::ChainTipReset(BlockSyncFrontiers {
            finalized_height: block::Height(2),
            verified_block_tip: block::Height(2),
            verified_block_hash: blocks[1].hash(),
        }))
        .await
        .expect("stale finalized reset queues");
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[2])]))
        .await
        .expect("duplicate needed metadata queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(100), actions.recv()).await
    {
        if let BlockSyncAction::SendMessage {
            msg:
                BlockSyncMessage::GetBlocks {
                    start_height: block::Height(3),
                    ..
                },
            ..
        } = action
        {
            panic!("stale finalized reset made an already-submitted successor requestable again");
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_reanchor_lowers_only_best_header_target() {
    let initial = test_frontier_update(0, 5, 10, FrontierChange::Snapshot);
    let (exchange, startup) =
        exchange_block_sync_startup(initial, immediate_body_download_config());
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    wait_for_query_needed_blocks(&mut actions, block::Height(5), block::Height(10)).await;

    exchange.publish_frontier(
        test_frontier_update(0, 1, 7, FrontierChange::HeaderReanchored),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(5), block::Height(7)).await;
    assert_eq!(handle.local_status().servable_high, block::Height(5));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_exchange_reanchor_releases_stale_submitted_bodies() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    // Worst-case reservation: budget for exactly the three in-flight bodies.
    config.max_inflight_block_bytes =
        BS_PER_BLOCK_WORST_CASE_BYTES * u64::try_from(blocks.len()).expect("block count fits u64");
    config.request_timeout = Duration::from_secs(300);

    let initial = test_frontier_update(0, 0, 3, FrontierChange::Snapshot);
    let (exchange, startup) = exchange_block_sync_startup(initial, config.clone());
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        66,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 3)
    );

    for block in &blocks {
        inbound_tx
            .send(
                BlockSyncMessage::Block(block.clone())
                    .encode_frame()
                    .expect("block frame encodes"),
            )
            .await
            .expect("block frame queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < blocks.len() {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => submitted.push(
                block
                    .coinbase_height()
                    .expect("submitted test block has height"),
            ),
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before all submitted bodies: {action:?}"),
        }
    }
    assert_eq!(
        submitted,
        vec![block::Height(1), block::Height(2), block::Height(3)]
    );

    exchange.publish_frontier(
        test_frontier_update(0, 0, 1, FrontierChange::HeaderReanchored),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(1)).await;

    exchange.publish_frontier(
        test_frontier_update(0, 0, 3, FrontierChange::HeaderAdvanced),
        "test",
    );
    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(3)).await;

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata after reanchor queues");
    let (got_peer, got_start, got_count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(got_peer, peer_id);
    assert_eq!(got_start, block::Height(1));
    assert_eq!(
        got_count, 3,
        "reanchored headers must release old submitted bodies and request them again",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_caps_submitted_applies_until_completion_releases_slot() {
    let blocks = fake_sequential_blocks(4);
    let mut config = immediate_body_download_config();
    config.max_inflight_block_bytes = u64::MAX;
    config.max_submitted_block_applies = 2;
    config.request_timeout = Duration::from_secs(300);

    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(4), blocks[3].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    wait_for_query_needed_blocks(&mut actions, block::Height(0), block::Height(4)).await;
    let (_peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        67,
        block::Height(4),
        blocks[3].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(
            blocks.iter().map(block_meta).collect(),
        ))
        .await
        .expect("needed metadata queues");
    let (_peer_id, _start, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(count, 4);

    for block in &blocks {
        inbound_tx
            .send(
                BlockSyncMessage::Block(block.clone())
                    .encode_frame()
                    .expect("block frame encodes"),
            )
            .await
            .expect("block frame queues");
    }

    let mut submitted = Vec::new();
    while submitted.len() < 2 {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => submitted.push((
                token,
                block
                    .coinbase_height()
                    .expect("submitted test block has height"),
                block.hash(),
            )),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action before cap reached: {action:?}"),
        }
    }
    assert_eq!(submitted[0].1, block::Height(1));
    assert_eq!(submitted[1].1, block::Height(2));

    assert!(
        tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                match actions.recv().await {
                    Some(BlockSyncAction::SubmitBlock { block, .. }) => {
                        return block.coinbase_height();
                    }
                    Some(BlockSyncAction::SendMessage { .. })
                    | Some(BlockSyncAction::QueryNeededBlocks { .. }) => {}
                    Some(action) => panic!("unexpected action while capped: {action:?}"),
                    None => return None,
                }
            }
        })
        .await
        .is_err(),
        "third body must wait until an apply completion releases a slot",
    );

    let (token, height, hash) = submitted[0];
    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token,
            height,
            hash,
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("apply completion queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block, .. } => {
                assert_eq!(
                    block
                        .coinbase_height()
                        .expect("submitted test block has height"),
                    block::Height(3)
                );
                break;
            }
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action after slot release: {action:?}"),
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_stale_non_reset_frontier_updates() {
    let (_tip_tx, tip_rx) = watch::channel((block::Height(3600), block::Hash([36; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(3200),
            verified_block_tip: block::Height(3200),
            verified_block_hash: block::Hash([32; 32]),
        },
        (block::Height(3600), block::Hash([36; 32])),
        tip_rx,
        immediate_body_download_config(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);

    assert!(matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks {
            verified_block_tip: block::Height(3200),
            best_header_tip: block::Height(3600),
        }
    ));

    handle
        .send(BlockSyncEvent::ChainTipGrow(BlockSyncFrontiers {
            finalized_height: block::Height(3200),
            verified_block_tip: block::Height(2913),
            verified_block_hash: block::Hash([29; 32]),
        }))
        .await
        .expect("stale grow event queues");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), actions.recv())
            .await
            .is_err(),
        "stale lower grow frontier must not query from the lower height"
    );
    assert_eq!(handle.local_status().servable_high, block::Height(3200));

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_retries_matched_range_unavailable_without_scoring_peer() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        63,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[0]),
            block_meta(&blocks[1]),
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 2)
    );

    inbound_tx
        .send(
            BlockSyncMessage::RangeUnavailable {
                start_height: block::Height(1),
                count: 2,
            }
            .encode_frame()
            .expect("RangeUnavailable frame encodes"),
        )
        .await
        .expect("RangeUnavailable frame queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 2),
        "matched RangeUnavailable should retry the original range without scoring the serving peer",
    );
    assert_eq!(handle.peer_snapshot().outbound_peers, 1);

    inbound_tx
        .send(
            BlockSyncMessage::RangeUnavailable {
                start_height: block::Height(2),
                count: 1,
            }
            .encode_frame()
            .expect("RangeUnavailable frame encodes"),
        )
        .await
        .expect("unmatched RangeUnavailable frame queues");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), actions.recv())
            .await
            .is_err(),
        "unmatched RangeUnavailable should be treated as advisory backpressure"
    );
    assert_eq!(handle.peer_snapshot().outbound_peers, 1);

    reactor_task.abort();
}

/// Regression guard for F-88604: two misbehaving peers that sort ahead of an honest
/// peer and spam `RangeUnavailable` for a contested range do **not** wedge body sync
/// — the honest peer is still offered the range and makes progress.
///
/// The audit flagged the unpenalized retry path as a possible wedge (two peers
/// re-occupying the whole fanout). Verified here that it is not: `handle_range_unavailable`
/// reschedules immediately after each response, and with one in-flight request per
/// peer the other misbehaving peer is still busy holding its stale request when the
/// first frees its slot, so the honest peer claims the freed companion slot. The
/// behavior is bounded churn, not a liveness wedge, so no peer-scoring guard is added.
/// This test fails if a future change ever lets the fanout peers lock the honest peer
/// out.
#[tokio::test]
async fn reactor_does_not_wedge_honest_peer_under_range_unavailable_spam() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.fanout = 2;
    // A long request timeout ensures the timeout-driven retry self-heal cannot mask
    // the wedge within the test window.
    config.request_timeout = Duration::from_secs(300);
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Two misbehaving peers (ids 0x01, 0x02 sort first) and one honest peer (0x03).
    let (m1, m1_in, _m1_out) = connect_peer_with_status(
        &service,
        &mut actions,
        0x01,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (m2, m2_in, _m2_out) = connect_peer_with_status(
        &service,
        &mut actions,
        0x02,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (h, _h_in, _h_out) = connect_peer_with_status(
        &service,
        &mut actions,
        0x03,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[0]),
            block_meta(&blocks[1]),
        ]))
        .await
        .expect("needed metadata queues");

    // Every time a misbehaving peer is offered the range it answers RangeUnavailable.
    // The honest peer must eventually be offered the range.
    let range_unavailable = || {
        BlockSyncMessage::RangeUnavailable {
            start_height: block::Height(1),
            count: 2,
        }
        .encode_frame()
        .expect("RangeUnavailable frame encodes")
    };
    let mut honest_offered = false;
    for _ in 0..16 {
        let (peer, _start, _count) = wait_for_getblocks(&mut actions).await;
        if peer == h {
            honest_offered = true;
            break;
        } else if peer == m1 {
            m1_in
                .send(range_unavailable())
                .await
                .expect("m1 RangeUnavailable queues");
        } else if peer == m2 {
            m2_in
                .send(range_unavailable())
                .await
                .expect("m2 RangeUnavailable queues");
        }
    }

    assert!(
        honest_offered,
        "honest peer must be offered the contested range after both fanout peers fail it"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_range_unavailable_retries_only_unverified_suffix() {
    let blocks = mainnet_blocks_1_to_3();
    let mut config = immediate_body_download_config();
    config.peer_limits.outbound_queue_depth = 16;
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        64,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[0]),
            block_meta(&blocks[1]),
        ]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(1), 2)
    );

    handle
        .send(BlockSyncEvent::ChainTipGrow(BlockSyncFrontiers {
            finalized_height: block::Height(1),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        }))
        .await
        .expect("frontier grow queues");
    wait_for_query_needed_blocks(&mut actions, block::Height(1), block::Height(2)).await;

    inbound_tx
        .send(
            BlockSyncMessage::RangeUnavailable {
                start_height: block::Height(1),
                count: 2,
            }
            .encode_frame()
            .expect("RangeUnavailable frame encodes"),
        )
        .await
        .expect("RangeUnavailable frame queues");

    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id, block::Height(2), 1),
        "late RangeUnavailable must not retry the already verified prefix",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_backpressures_serving_slots_without_scoring_peer() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_requests: 1,
        ..ZakuraBlockSyncConfig::default()
    };
    config.peer_limits.outbound_queue_depth = 16;
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, mut outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        63,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    tokio::time::sleep(Duration::from_millis(10)).await;

    for _ in 0..2 {
        inbound_tx
            .send(
                BlockSyncMessage::GetBlocks {
                    start_height: block::Height(1),
                    count: 1,
                }
                .encode_frame()
                .expect("GetBlocks frame encodes"),
            )
            .await
            .expect("GetBlocks frame queues");
    }
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryBlocksByHeightRange { .. }
    ) {}

    assert_eq!(
        wait_for_outbound_range_unavailable(&mut outbound_rx).await,
        (block::Height(1), 1),
        "serving-slot saturation should backpressure the requester, not score it as spam",
    );
    assert_eq!(handle.peer_snapshot().outbound_peers, 1);

    handle
        .send(BlockSyncEvent::BlockRangeResponseFinished {
            peer: peer_id.clone(),
            start_height: block::Height(1),
            requested_count: 1,
            returned_count: 1,
        })
        .await
        .expect("serving slot release queues");

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_publishes_block_sync_candidate_gap() {
    let config = immediate_body_download_config();
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer_id = peer(77);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, _outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer_id.clone(),
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));
    assert_eq!(wait_for_connect_status(&mut actions).await, peer_id);

    tip_tx
        .send((block::Height(2), block::Hash([2; 32])))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            BlockSyncBlockMeta {
                height: block::Height(2),
                hash: block::Hash([2; 32]),
                size: BlockSizeEstimate::Unknown,
            },
            BlockSyncBlockMeta {
                height: block::Height(1),
                hash: block::Hash([1; 32]),
                size: BlockSizeEstimate::Unknown,
            },
        ]))
        .await
        .expect("needed blocks event queues");

    let mut candidates = handle.subscribe_candidate_state();
    let observed = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            candidates
                .changed()
                .await
                .expect("candidate watch remains open");
            let state = candidates.borrow().clone();
            if state.missing_block_bodies == vec![block::Height(1), block::Height(2)] {
                return state;
            }
        }
    })
    .await
    .unwrap_or_else(|_| handle.candidate_state());
    assert_eq!(
        observed.missing_block_bodies,
        vec![block::Height(1), block::Height(2)]
    );
    assert!(
        observed.admitted_node_ids.is_empty(),
        "a peer without block-sync status must not satisfy body-sync demand"
    );

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(2),
                tip_hash: block::Hash([2; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status queues");

    let peer_node_id =
        node_id_from_block_peer_id(&peer_id).expect("test peer id is a valid node id");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            candidates
                .changed()
                .await
                .expect("candidate watch remains open");
            if candidates.borrow().admitted_node_ids == vec![peer_node_id] {
                return;
            }
        }
    })
    .await
    .expect("status-bearing peer is published as an admitted candidate");

    handle
        .send(BlockSyncEvent::StateFrontiersChanged(BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(2),
            verified_block_hash: block::Hash([2; 32]),
        }))
        .await
        .expect("frontier event queues");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.candidate_state().missing_block_bodies.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("candidate state clears after the gap is gone");

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_reports_size_mismatch_softly_and_still_submits_valid_block() {
    let mut config = ZakuraBlockSyncConfig {
        size_deviation_tolerance: 100,
        ..immediate_body_download_config()
    };
    config.peer_limits.outbound_queue_depth = 8;
    let (tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let peer = peer(42);
    let (inbound_tx, inbound_rx) = framed_channel(8);
    let (outbound_tx, mut outbound_rx) = framed_channel(8);
    let streams = HashMap::from([(ZAKURA_STREAM_BLOCK_SYNC, (inbound_rx, outbound_tx))]);
    service.add_peer(Peer::new_with_direction(
        peer,
        None,
        ZAKURA_CAP_BLOCK_SYNC,
        ServicePeerDirection::Outbound,
        streams,
        CancellationToken::new(),
    ));

    inbound_tx
        .send(
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high: block::Height(1),
                tip_hash: block::Hash([1; 32]),
                max_blocks_per_response: 4,
                max_inflight_requests: 1,
                max_response_bytes: MAX_BS_RESPONSE_BYTES,
            })
            .encode_frame()
            .expect("status encodes"),
        )
        .await
        .expect("status queues");
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);
    tip_tx
        .send((block::Height(1), block.hash()))
        .expect("tip watch is live");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::QueryNeededBlocks { .. }
    ) {}
    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(1),
            hash: block.hash(),
            size: BlockSizeEstimate::Advertised(1),
        }]))
        .await
        .expect("needed metadata queues");
    while !matches!(
        next_action(&mut actions).await,
        BlockSyncAction::SendMessage {
            msg: BlockSyncMessage::GetBlocks { .. },
            ..
        }
    ) {}
    while !matches!(
        BlockSyncMessage::decode_frame(
            tokio::time::timeout(Duration::from_secs(1), outbound_rx.recv())
                .await
                .expect("outbound frame arrives")
                .expect("outbound channel is live")
        )
        .expect("frame decodes"),
        BlockSyncMessage::GetBlocks { .. }
    ) {}

    inbound_tx
        .send(
            BlockSyncMessage::Block(block.clone())
                .encode_frame()
                .expect("block encodes"),
        )
        .await
        .expect("block queues");

    let mut saw_size_mismatch = false;
    let mut saw_submit = false;
    while !(saw_size_mismatch && saw_submit) {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { reason, .. } => {
                assert_eq!(reason, BlockSyncMisbehavior::SizeMismatch);
                saw_size_mismatch = true;
            }
            BlockSyncAction::SubmitBlock {
                block: submitted, ..
            } => {
                assert_eq!(submitted.hash(), block.hash());
                saw_submit = true;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action during size mismatch test: {action:?}"),
        }
    }

    reactor_task.abort();
}

// SECURITY AUDIT (candidate claude-block-sync-unsolicited-blocksdone-not-rejected /
// codex-blocksync-unsolicited-blocksdone-not-rejected): SR-6/SR-7 response
// correlation + fail-closed.
//
// `handle_blocks_done` reports `UnsolicitedDone` only when the peer is *unknown*.
// For a known, active peer that sends a valid `BlocksDone` with no matching
// outstanding request, the `if let Some(index)` body is skipped and the reactor
// falls through to `schedule()` with no `else` reporting `UnsolicitedDone`.
// `UnsolicitedDone` is a *hard* block-sync misbehavior (`block_sync_misbehavior_is_hard`
// in zebrad start.rs), so the production driver `drive_block_sync_actions`
// disconnects on the first offense -- but this branch never emits it, so an
// admitted peer can stream uncorrelated response terminators forever and stay
// connected.
//
// This test asserts the SAFE behavior (the reactor must report `UnsolicitedDone`).
// It currently FAILS, which is the reproduction. Do not weaken it to pass; the
// fix is to add the missing `else` branch in `handle_blocks_done`.
#[tokio::test]
async fn reactor_known_peer_unsolicited_blocks_done_is_reported_as_misbehavior() {
    let config = ZakuraBlockSyncConfig::default();
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    // Connect a peer that advertises no downloadable work (servable_high == our
    // verified tip), so the reactor never schedules a GetBlocks and the peer has
    // zero outstanding requests. The peer is known/active (received_status=true).
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        63,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    // Surprising/hostile input: a valid `BlocksDone` terminator with a start
    // height that matches no outstanding request (there are none).
    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(7),
                returned: 1,
            }
            .encode_frame()
            .expect("BlocksDone frame encodes"),
        )
        .await
        .expect("BlocksDone frame queues");

    // Expected safe behavior: the reactor reports `UnsolicitedDone` for this peer
    // (which the production driver maps to a hard disconnect). Collect actions for
    // a bounded window and assert it appears.
    let mut saw_unsolicited_done = false;
    while let Ok(Some(action)) = tokio::time::timeout(Duration::from_secs(1), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior { peer, reason } = action {
            if peer == peer_id && reason == BlockSyncMisbehavior::UnsolicitedDone {
                saw_unsolicited_done = true;
                break;
            }
        }
    }

    assert!(
        saw_unsolicited_done,
        "a known peer's unsolicited BlocksDone with no matching outstanding request \
         must be reported as Misbehavior::UnsolicitedDone (SR-6/SR-7), but the reactor \
         silently tolerated it and kept the peer connected",
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_unmatched_response_for_height_active_on_another_request() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());

    let (peer1, _inbound1, _outbound1) = connect_peer_with_status(
        &service,
        &mut actions,
        65,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (peer2, _inbound2, _outbound2) = connect_peer_with_status(
        &service,
        &mut actions,
        66,
        block::Height(3),
        blocks[2].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[1])]))
        .await
        .expect("needed metadata queues");
    let (requested_peer, start_height, _count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(start_height, block::Height(2));

    // Simulate a later state snapshot that omits the active in-flight height.
    // The scheduler retains the outstanding request for correlation, but
    // `needed_heights` no longer contains the height, matching the production
    // late-response race after retries/reconnects.
    handle
        .send(BlockSyncEvent::NeededBlocks(Vec::new()))
        .await
        .expect("empty needed metadata queues");

    let late_peer = if requested_peer == peer1 {
        peer2
    } else {
        peer1
    };
    handle
        .send(BlockSyncEvent::WireMessage {
            peer: late_peer.clone(),
            msg: BlockSyncMessage::Block(blocks[1].clone()),
        })
        .await
        .expect("late body queues");
    handle
        .send(BlockSyncEvent::WireMessage {
            peer: late_peer.clone(),
            msg: BlockSyncMessage::BlocksDone {
                start_height: block::Height(2),
                returned: 1,
            },
        })
        .await
        .expect("late terminator queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(200), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior { peer, reason } = action {
            assert_ne!(
                peer, late_peer,
                "late response for an active request was reported as {reason:?}"
            );
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_duplicate_response_at_body_download_floor() {
    let config = immediate_body_download_config();
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(2), blocks[1].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(2), blocks[1].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        65,
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[1])]))
        .await
        .expect("needed metadata queues");
    assert_eq!(
        wait_for_getblocks(&mut actions).await,
        (peer_id.clone(), block::Height(2), 1)
    );
    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[1].clone())
                .encode_frame()
                .expect("block frame encodes"),
        )
        .await
        .expect("block frame queues");

    let (token, hash) = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => break (token, block.hash()),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action while waiting for submit: {action:?}"),
        }
    };

    // Simulate the verifier reporting success before the frontier mirror has
    // delivered the matching verified-tip update. The applying entry is gone,
    // but `body_download_floor` still proves this height was already accepted.
    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token,
            height: block::Height(2),
            hash,
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("apply result queues");

    inbound_tx
        .send(
            BlockSyncMessage::Block(blocks[1].clone())
                .encode_frame()
                .expect("duplicate block frame encodes"),
        )
        .await
        .expect("duplicate block frame queues");
    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(2),
                returned: 1,
            }
            .encode_frame()
            .expect("duplicate terminator frame encodes"),
        )
        .await
        .expect("duplicate terminator frame queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(200), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior { peer, reason } = action {
            assert_ne!(
                peer, peer_id,
                "duplicate response at body_download_floor was reported as {reason:?}"
            );
        }
    }

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_matched_duplicate_response_at_body_download_floor() {
    let blocks = mainnet_blocks_1_to_3();
    let block2_size = block_size(&blocks[1]);
    let mut config = immediate_body_download_config();
    config.fanout = 2;
    config.expected_peers = 0;
    config.max_inflight_block_bytes = BS_PER_BLOCK_WORST_CASE_BYTES * 2;

    let (_tip_tx, tip_rx) = watch::channel((block::Height(4), block::Hash([4; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(4), block::Hash([4; 32])),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (peer_a, _inbound_a, _outbound_a) = connect_peer_with_status(
        &service,
        &mut actions,
        66,
        block::Height(4),
        block::Hash([4; 32]),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (peer_b, _inbound_b, _outbound_b) = connect_peer_with_status(
        &service,
        &mut actions,
        67,
        block::Height(4),
        block::Hash([4; 32]),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![block_meta(&blocks[1])]))
        .await
        .expect("needed metadata queues");
    let first_request = wait_for_getblocks(&mut actions).await;
    let second_request = wait_for_getblocks(&mut actions).await;
    assert_eq!((first_request.1, first_request.2), (block::Height(2), 1));
    assert_eq!((second_request.1, second_request.2), (block::Height(2), 1));
    assert_ne!(
        first_request.0, second_request.0,
        "fanout should assign the same height to two distinct peers"
    );

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: first_request.0.clone(),
            msg: BlockSyncMessage::Block(blocks[1].clone()),
        })
        .await
        .expect("first body queues");

    let (token, hash) = loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { token, block } => break (token, block.hash()),
            BlockSyncAction::SendMessage { .. } | BlockSyncAction::QueryNeededBlocks { .. } => {}
            action => panic!("unexpected action while waiting for submit: {action:?}"),
        }
    };
    handle
        .send(BlockSyncEvent::BlockApplyFinished {
            token,
            height: block::Height(2),
            hash,
            result: BlockApplyResult::Committed,
            local_frontier: None,
        })
        .await
        .expect("apply result queues");

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: second_request.0.clone(),
            msg: BlockSyncMessage::Block(blocks[1].clone()),
        })
        .await
        .expect("late duplicate body queues");

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![
            block_meta(&blocks[2]),
            BlockSyncBlockMeta {
                height: block::Height(4),
                hash: block::Hash([4; 32]),
                size: BlockSizeEstimate::Advertised(block2_size),
            },
        ]))
        .await
        .expect("next needed metadata queues");

    let next_request = wait_for_getblocks(&mut actions).await;
    assert!(
        next_request.0 == peer_a || next_request.0 == peer_b,
        "request should target one of the connected peers"
    );
    assert_eq!(
        (next_request.1, next_request.2),
        (block::Height(3), 2),
        "a matched duplicate response at the body floor must not consume reorder budget"
    );

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_ignores_late_response_frames_after_peer_disconnect() {
    let config = ZakuraBlockSyncConfig::default();
    let blocks = mainnet_blocks_1_to_3();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(1), blocks[0].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(1),
            verified_block_hash: blocks[0].hash(),
        },
        (block::Height(1), blocks[0].hash()),
        tip_rx,
        config.clone(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config.clone(), handle.clone());

    let (peer_id, _inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        64,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;

    service.remove_peer(&peer_id);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.peer_snapshot().outbound_peers == 0 && service.peer_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("disconnect releases the block-sync peer slot");

    handle
        .send(BlockSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: BlockSyncMessage::Block(blocks[1].clone()),
        })
        .await
        .expect("late body frame queues");
    handle
        .send(BlockSyncEvent::WireMessage {
            peer: peer_id.clone(),
            msg: BlockSyncMessage::BlocksDone {
                start_height: block::Height(2),
                returned: 1,
            },
        })
        .await
        .expect("late terminator frame queues");

    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(200), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior { peer, reason } = action {
            assert_ne!(
                peer, peer_id,
                "late response from a disconnected peer was reported as {reason:?}"
            );
        }
    }

    let (peer_id, inbound_tx, _outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        64,
        block::Height(1),
        blocks[0].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    inbound_tx
        .send(
            BlockSyncMessage::BlocksDone {
                start_height: block::Height(7),
                returned: 1,
            }
            .encode_frame()
            .expect("BlocksDone frame encodes"),
        )
        .await
        .expect("unsolicited terminator frame queues");

    let mut saw_unsolicited_done = false;
    while let Ok(Some(action)) = tokio::time::timeout(Duration::from_secs(1), actions.recv()).await
    {
        if let BlockSyncAction::Misbehavior { peer, reason } = action {
            if peer == peer_id && reason == BlockSyncMisbehavior::UnsolicitedDone {
                saw_unsolicited_done = true;
                break;
            }
        }
    }
    assert!(
        saw_unsolicited_done,
        "reconnecting the same peer must restore hard unsolicited-terminator checks"
    );

    reactor_task.abort();
}

/// Regression for `claude-sync-reactor-action-backpressure-stalls-disconnect` in
/// the block-sync reactor: when the bounded 128-slot action channel is saturated
/// and the action driver is stalled, awaiting `actions.send` for `Misbehavior`
/// wedged the reactor, so it could no longer reach its own disconnect path. The
/// reactor must instead enqueue misbehavior non-blockingly and stay live — here,
/// live enough to still tear down a soft offender once it crosses the
/// disconnect threshold.
#[tokio::test]
async fn misbehaving_peer_is_disconnected_even_when_action_channel_is_saturated() {
    let config = ZakuraBlockSyncConfig::default();
    let (_tip_tx, tip_rx) = watch::channel((block::Height(0), block::Hash([0; 32])));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(0),
            verified_block_hash: block::Hash([0; 32]),
        },
        (block::Height(0), block::Hash([0; 32])),
        tip_rx,
        config,
    );
    // `_actions` is held but never drained: the production action driver is
    // "stalled".
    let (handle, _actions, reactor_task) = spawn_block_sync_reactor(startup);

    // Connect the probe peer with a session whose cancellation token we keep, so
    // we can observe the local disconnect. A bare session (no supervised pipe)
    // means the cancel is observable without removing the peer from the reactor.
    let probe = peer(7);
    let probe_cancel = CancellationToken::new();
    let (_inbound_tx, inbound_rx) = framed_channel(4);
    let (outbound_tx, _outbound_rx) = framed_channel(4);
    let session = PeerStreamSession::new(
        probe.clone(),
        ZAKURA_STREAM_BLOCK_SYNC,
        inbound_rx,
        outbound_tx,
        probe_cancel.clone(),
    );
    handle
        .send(BlockSyncEvent::PeerConnected(BlockSyncPeerSession::new(
            &session,
            ServicePeerDirection::Outbound,
        )))
        .await
        .expect("probe peer connects");

    // Saturate the bounded 128-slot action channel. Malformed-frame events from
    // an unknown filler peer enqueue `Misbehavior` actions until the channel is
    // full. A per-send timeout keeps the test from hanging if the (unfixed)
    // reactor wedges on a blocking `actions.send` and stops draining events.
    let filler = peer(200);
    let decode_error = Arc::new(BlockSyncWireError::TrailingBytes);
    for _ in 0..400 {
        let send = handle.send(BlockSyncEvent::WireDecodeFailed {
            peer: filler.clone(),
            error: decode_error.clone(),
        });
        if tokio::time::timeout(Duration::from_millis(200), send)
            .await
            .is_err()
        {
            break;
        }
    }

    // Drive the connected probe peer past the soft-misbehavior disconnect
    // threshold (3) while the action channel is saturated. A reactor wedged on a
    // blocking misbehavior send can never process these events, so it never
    // reaches the threshold cancel; a non-blocking reactor does.
    for _ in 0..4 {
        let _ = tokio::time::timeout(
            Duration::from_millis(200),
            handle.send(BlockSyncEvent::WireMessage {
                peer: probe.clone(),
                msg: BlockSyncMessage::GetBlocks {
                    start_height: block::Height(1),
                    count: 1,
                },
            }),
        )
        .await;
    }

    tokio::time::timeout(Duration::from_secs(1), probe_cancel.cancelled())
        .await
        .expect(
            "a repeatedly-misbehaving block-sync peer must still be disconnected when the action \
             channel is saturated and the action driver is stalled",
        );

    reactor_task.abort();
}
