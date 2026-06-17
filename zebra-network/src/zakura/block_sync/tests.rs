use std::{collections::HashMap, future};

use super::*;
use super::{
    config::{DEFAULT_BS_MAX_INFLIGHT, MAX_BS_RESPONSE_BYTES},
    reorder::*,
    scheduler::*,
    state::*,
};
use crate::zakura::{
    framed_channel, FramedRecv, FramedSend, Peer, PeerStreamSession, Service, ServicePeerSnapshot,
    ServiceRegistry, StreamMode, ZakuraBlockSyncCandidateState,
};
use zebra_chain::{
    fmt::HexDebug,
    serialization::{ZcashDeserializeInto, ZcashSerialize},
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
            BlockSyncAction::SubmitBlock { block } => {
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
    (peer, state)
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
            BlockSyncMessage::Status(BlockSyncStatus {
                servable_low: block::Height(1),
                servable_high,
                tip_hash,
                max_blocks_per_response: 16,
                max_inflight_requests,
                max_response_bytes,
            })
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
    assert_eq!(status.max_inflight_requests, DEFAULT_BS_MAX_INFLIGHT);
    assert_eq!(status.max_response_bytes, MAX_BS_RESPONSE_BYTES);
}

#[test]
fn scheduler_assigns_needed_ranges_with_fanout_slots_and_dedup() {
    let mut scheduler = BlockRangeScheduler::new(2);
    scheduler.refresh_needed(vec![
        needed(1, BlockSizeEstimate::Advertised(10_000)),
        needed(2, BlockSizeEstimate::Advertised(10_000)),
        needed(3, BlockSizeEstimate::Advertised(10_000)),
    ]);
    let mut budget = ByteBudget::new(1_000_000);
    let (peer1, state1) = peer_state(31);
    let (peer2, state2) = peer_state(32);
    let (peer3, state3) = peer_state(33);

    let first = scheduler
        .next_for_peer(&peer1, &state1, &mut budget)
        .expect("first peer gets the range");
    assert_eq!(first.start_height, block::Height(1));
    assert_eq!(first.count, 3);

    let second = scheduler
        .next_for_peer(&peer2, &state2, &mut budget)
        .expect("fanout allows a second peer");
    assert_eq!(second.start_height, block::Height(1));
    assert_eq!(second.count, 3);

    assert!(
        scheduler
            .next_for_peer(&peer1, &state1, &mut budget)
            .is_none(),
        "same peer must not receive overlapping duplicate assignment"
    );
    assert!(
        scheduler
            .next_for_peer(&peer3, &state3, &mut budget)
            .is_none(),
        "fanout cap must deduplicate the covered range"
    );
}

#[test]
fn scheduler_byte_budget_sizing_shrinks_or_defers_requests() {
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![
        needed(10, BlockSizeEstimate::Advertised(100)),
        needed(11, BlockSizeEstimate::Advertised(100)),
        needed(12, BlockSizeEstimate::Advertised(100)),
    ]);
    let (peer, mut state) = peer_state(34);
    state.max_response_bytes = 180;
    state.max_blocks_per_response = 10;
    let mut budget = ByteBudget::new(1_000);

    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
        .expect("one block fits the peer response-byte cap");
    assert_eq!(request.start_height, block::Height(10));
    assert_eq!(request.count, 1);
    assert_eq!(budget.reserved(), 100);

    scheduler.complete(&request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    let mut small_budget = ByteBudget::new(50);
    assert!(
        scheduler
            .next_for_peer(&peer, &state, &mut small_budget)
            .is_none(),
        "a first block that does not fit is deferred"
    );
    assert_eq!(small_budget.reserved(), 0);
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
    state.max_response_bytes = 200;
    state.max_blocks_per_response = 10;
    let mut budget = ByteBudget::new(1_000);

    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
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
fn scheduler_releases_budget_on_completion_timeout_and_cancel() {
    let (peer, state) = peer_state(35);
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1);
    scheduler.refresh_needed(vec![needed(20, BlockSizeEstimate::Advertised(1_000))]);
    let mut budget = ByteBudget::new(10_000);
    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
        .expect("range fits");
    assert_eq!(budget.reserved(), 1_000);
    scheduler.complete(&request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    scheduler.refresh_needed(vec![needed(21, BlockSizeEstimate::Advertised(2_000))]);
    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
        .expect("range fits");
    scheduler.timeout(request, &mut budget);
    assert_eq!(budget.reserved(), 0);

    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
        .expect("retried range fits");
    assert_eq!(budget.reserved(), 2_000);
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
            .next_for_peer(&peer, &state, &mut budget)
            .is_none(),
        "stale queued anchors must not survive a re-derived needed set"
    );
    assert_eq!(budget.reserved(), 0);
}

#[test]
fn scheduler_uses_ewma_for_unknown_and_confirmed_size_values() {
    let (peer, mut state) = peer_state(36);
    state.max_response_bytes = 100_000;
    let mut scheduler = BlockRangeScheduler::new(1);
    scheduler.set_estimator_for_tests(750, 1_000);
    scheduler.refresh_needed(vec![
        needed(30, BlockSizeEstimate::Unknown),
        needed(31, BlockSizeEstimate::Advertised(500)),
        needed(32, BlockSizeEstimate::Confirmed(50_000)),
    ]);
    let mut budget = ByteBudget::new(100_000);

    let request = scheduler
        .next_for_peer(&peer, &state, &mut budget)
        .expect("range fits");
    assert_eq!(request.count, 3);
    assert_eq!(request.estimated_bytes, 52_000);
}

#[test]
fn reorder_releases_only_contiguous_prefix_and_clears_budget() {
    let mut reorder = ReorderBuffer::new();
    let mut budget = ByteBudget::new(10_000);
    let block = mainnet_block(&BLOCK_MAINNET_1_BYTES);

    assert_eq!(
        reorder.insert(block::Height(3), block.clone(), 300, &mut budget),
        ReorderInsertResult::Inserted
    );
    assert!(reorder
        .drain_contiguous_prefix(block::Height(0), &mut budget)
        .is_empty());
    assert_eq!(reorder.buffered_bytes(), 300);
    assert_eq!(budget.reserved(), 300);

    assert_eq!(
        reorder.insert(block::Height(1), block.clone(), 100, &mut budget),
        ReorderInsertResult::Inserted
    );
    let released = reorder.drain_contiguous_prefix(block::Height(0), &mut budget);
    assert_eq!(
        released
            .iter()
            .map(|(height, _)| *height)
            .collect::<Vec<_>>(),
        vec![block::Height(1)]
    );
    assert_eq!(reorder.buffered_bytes(), 300);
    assert_eq!(budget.reserved(), 300);

    assert_eq!(
        reorder.insert(block::Height(2), block.clone(), 200, &mut budget),
        ReorderInsertResult::Inserted
    );
    let released = reorder.drain_contiguous_prefix(block::Height(1), &mut budget);
    assert_eq!(
        released
            .iter()
            .map(|(height, _)| *height)
            .collect::<Vec<_>>(),
        vec![block::Height(2), block::Height(3)]
    );
    assert_eq!(budget.reserved(), 0);

    assert_eq!(
        reorder.insert(block::Height(2), block.clone(), 200, &mut budget),
        ReorderInsertResult::Inserted
    );
    assert_eq!(
        reorder.insert(block::Height(3), block, 300, &mut budget),
        ReorderInsertResult::Inserted
    );
    reorder.drop_from(block::Height(3), &mut budget);
    assert_eq!(reorder.buffered_bytes(), 200);
    assert_eq!(budget.reserved(), 200);
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
            assert_eq!(
                reorder.insert(block::Height(height), block.clone(), 100, &mut budget),
                ReorderInsertResult::Inserted
            );
            for (released, _) in reorder.drain_contiguous_prefix(tip, &mut budget) {
                assert_eq!(released, block::Height(tip.0 + 1));
                tip = released;
                released_all.push(released);
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

    for byte in 6..=7 {
        let peer_id = peer(byte);
        let (_inbound_tx, inbound_rx) = framed_channel(4);
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
    let config = ZakuraBlockSyncConfig::default();
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
            BlockSyncAction::SubmitBlock { block: submitted } => {
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
async fn reactor_accepts_multi_block_range_and_submits_parent_first() {
    let config = ZakuraBlockSyncConfig::default();
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
            BlockSyncAction::SubmitBlock { block } => submitted.push(
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

#[tokio::test]
async fn reactor_reset_mid_download_drops_stale_anchors_and_releases_budget() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: 20_000,
        ..ZakuraBlockSyncConfig::default()
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
        20_000,
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
async fn reactor_fast_forward_reset_clears_buffered_bodies_and_releases_budget() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: 20_000,
        ..ZakuraBlockSyncConfig::default()
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
        20_000,
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
            max_inflight_block_bytes: 60_000,
            ..ZakuraBlockSyncConfig::default()
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
            60_000,
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
        ZakuraBlockSyncConfig::default(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(
        ZakuraBlockSyncConfig::default(),
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
        max_inflight_block_bytes: 10_000,
        ..ZakuraBlockSyncConfig::default()
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
        10_000,
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
    let config = ZakuraBlockSyncConfig::default();
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
            BlockSyncAction::SubmitBlock { block } => submitted.push(
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
        ZakuraBlockSyncConfig::default(),
    );
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(
        ZakuraBlockSyncConfig::default(),
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
    let config = ZakuraBlockSyncConfig::default();
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
async fn reactor_scores_header_valid_merkle_invalid_body_and_accepts_clean_peer() {
    let request_bytes: u32 = 10_000;
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_block_bytes: u64::from(request_bytes) * 2,
        fanout: 2,
        ..ZakuraBlockSyncConfig::default()
    };
    config.peer_limits.outbound_queue_depth = 16;

    let blocks = mainnet_blocks_1_to_3();
    let bad_body = block_with_bad_merkle_root(&blocks[0], &blocks[1]);
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
        block::Height(2),
        blocks[1].hash(),
        1,
        MAX_BS_RESPONSE_BYTES,
    )
    .await;
    let (good_peer, good_inbound, _good_outbound) = connect_peer_with_status(
        &service,
        &mut actions,
        41,
        block::Height(2),
        blocks[1].hash(),
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

    let mut requested_peers = Vec::new();
    while requested_peers.len() < 2 {
        let (peer, start_height, count) = wait_for_getblocks(&mut actions).await;
        assert_eq!(start_height, block::Height(1));
        assert_eq!(count, 1);
        requested_peers.push(peer);
    }
    assert!(requested_peers.contains(&bad_peer));
    assert!(requested_peers.contains(&good_peer));

    bad_inbound
        .send(
            BlockSyncMessage::Block(bad_body)
                .encode_frame()
                .expect("bad block frame encodes"),
        )
        .await
        .expect("bad block frame queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, bad_peer);
                assert_eq!(reason, BlockSyncMisbehavior::InvalidBlock);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::SubmitBlock { block } => {
                panic!("merkle-invalid block was buffered and submitted: {block:?}");
            }
            action => panic!("unexpected action before invalid body scoring: {action:?}"),
        }
    }

    good_inbound
        .send(
            BlockSyncMessage::Block(blocks[0].clone())
                .encode_frame()
                .expect("good block frame encodes"),
        )
        .await
        .expect("good block frame queues");

    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::SubmitBlock { block } => {
                assert_eq!(block.hash(), blocks[0].hash());
                assert_eq!(block.coinbase_height(), Some(block::Height(1)));
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before clean body submit: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::NeededBlocks(vec![BlockSyncBlockMeta {
            height: block::Height(2),
            hash: blocks[1].hash(),
            size: BlockSizeEstimate::Advertised(request_bytes * 2),
        }]))
        .await
        .expect("follow-up needed metadata queues");
    let (_peer, start_height, count) = wait_for_getblocks(&mut actions).await;
    assert_eq!(start_height, block::Height(2));
    assert_eq!(count, 1);

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
    let (_tip_tx, tip_rx) = watch::channel((block::Height(3), blocks[2].hash()));
    let startup = BlockSyncStartup::new(
        BlockSyncFrontiers {
            finalized_height: block::Height(0),
            verified_block_tip: block::Height(3),
            verified_block_hash: blocks[2].hash(),
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
    let mut config = ZakuraBlockSyncConfig::default();
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
async fn reactor_debounces_status_advertisements_on_serving_tip_change() {
    let mut config = ZakuraBlockSyncConfig {
        status_refresh_interval: Duration::from_secs(60),
        ..ZakuraBlockSyncConfig::default()
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
    let (handle, mut actions, reactor_task) = spawn_block_sync_reactor(startup);
    let service = BlockSyncService::new_with_handle_for_test(config, handle.clone());
    let (_peer_id, _inbound_tx, mut outbound_rx) = connect_peer_with_status(
        &service,
        &mut actions,
        62,
        block::Height(3),
        block::Hash([3; 32]),
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
async fn reactor_limits_serving_slots_and_disconnects_repeated_soft_misbehavior() {
    let mut config = ZakuraBlockSyncConfig {
        max_inflight_requests: 1,
        ..ZakuraBlockSyncConfig::default()
    };
    config.peer_limits.outbound_queue_depth = 16;
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
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, peer_id);
                assert_eq!(reason, BlockSyncMisbehavior::GetBlocksSpam);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before serving-slot spam report: {action:?}"),
        }
    }

    handle
        .send(BlockSyncEvent::BlockRangeResponseFinished {
            peer: peer_id.clone(),
            start_height: block::Height(1),
            requested_count: 1,
            returned_count: 1,
        })
        .await
        .expect("serving slot release queues");

    inbound_tx
        .send(
            BlockSyncMessage::RangeUnavailable {
                start_height: block::Height(1),
                count: 1,
            }
            .encode_frame()
            .expect("RangeUnavailable frame encodes"),
        )
        .await
        .expect("single soft report queues");
    loop {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, peer_id);
                assert_eq!(reason, BlockSyncMisbehavior::RangeUnavailable);
                break;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action before first soft report: {action:?}"),
        }
    }
    assert_eq!(handle.peer_snapshot().outbound_peers, 1);

    for _ in 0..2 {
        inbound_tx
            .send(
                BlockSyncMessage::RangeUnavailable {
                    start_height: block::Height(1),
                    count: 1,
                }
                .encode_frame()
                .expect("RangeUnavailable frame encodes"),
            )
            .await
            .expect("soft report queues");
    }
    let mut soft_reports = 0;
    while soft_reports < 2 {
        match next_action(&mut actions).await {
            BlockSyncAction::Misbehavior { peer, reason } => {
                assert_eq!(peer, peer_id);
                assert_eq!(reason, BlockSyncMisbehavior::RangeUnavailable);
                soft_reports += 1;
            }
            BlockSyncAction::SendMessage { .. } => {}
            action => panic!("unexpected action during repeated soft reports: {action:?}"),
        }
    }

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if handle.peer_snapshot().outbound_peers == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("repeated soft misbehavior disconnects the peer");

    reactor_task.abort();
}

#[tokio::test]
async fn reactor_publishes_block_sync_candidate_gap() {
    let config = ZakuraBlockSyncConfig::default();
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
    drop(inbound_tx);
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
        ..ZakuraBlockSyncConfig::default()
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
            BlockSyncAction::SubmitBlock { block: submitted } => {
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
    while let Ok(Some(action)) =
        tokio::time::timeout(Duration::from_millis(300), actions.recv()).await
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
                msg: BlockSyncMessage::RangeUnavailable {
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
