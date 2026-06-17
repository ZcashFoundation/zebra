use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{
    future::BoxFuture,
    stream::{FuturesUnordered, StreamExt},
    FutureExt,
};
use tokio::time::Instant as TokioInstant;
use tokio::{pin, select, sync::mpsc};
use tower::{Service, ServiceExt};
use tracing::{debug, warn};

use zebra_chain::{block, chain_tip::ChainTip};
use zebra_network::zakura::{
    commit_state_trace as cs_trace, BlockApplyResult, BlockApplyToken, BlockSizeEstimate,
    BlockSyncAction, BlockSyncBlockMeta, BlockSyncEvent, BlockSyncHandle, BlockSyncMessage,
    BlockSyncMisbehavior, Frontier, FrontierChange, ZakuraEndpoint, ZakuraTrace,
};

use crate::components::sync;

use super::{
    block_apply_result_label, block_verify_error_is_duplicate, emit_commit_state, insert_cs_bool,
    insert_cs_frontiers, insert_cs_hash, insert_cs_height, insert_cs_peer, insert_cs_str,
    insert_cs_u64, query_block_sync_frontiers, ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
};

pub(crate) const ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL: Duration =
    Duration::from_secs(5);
const ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_ATTEMPTS: usize = 24;
pub(crate) const ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW: u32 = 262_144;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum BlockApplyClass {
    Checkpoint,
    Full,
}

#[derive(Clone, Debug)]
struct PendingBlockApply {
    token: BlockApplyToken,
    class: BlockApplyClass,
    block: Arc<block::Block>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct BlockApplyCompletion {
    class: BlockApplyClass,
    checkpoint_refresh_floor: Option<block::Height>,
}

#[derive(Clone, Debug, Default)]
struct CheckpointFrontierRefresh {
    highest_sent: Option<block::Height>,
    attempts_remaining: usize,
    next_attempt_at: Option<TokioInstant>,
}

impl CheckpointFrontierRefresh {
    fn observe_checkpoint_commit(&mut self, highest_observed_at_apply: block::Height) {
        self.highest_sent = Some(
            self.highest_sent
                .map(|height| height.max(highest_observed_at_apply))
                .unwrap_or(highest_observed_at_apply),
        );
        self.attempts_remaining = ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_ATTEMPTS;
        if self.next_attempt_at.is_none() {
            self.next_attempt_at =
                Some(TokioInstant::now() + ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL);
        }
    }

    fn next_attempt_at(&self) -> Option<TokioInstant> {
        (self.attempts_remaining > 0)
            .then_some(self.next_attempt_at)
            .flatten()
    }

    fn finish_attempt(&mut self, highest_sent: block::Height) {
        self.highest_sent = Some(highest_sent);
        self.attempts_remaining = self.attempts_remaining.saturating_sub(1);
        self.next_attempt_at = (self.attempts_remaining > 0).then_some(
            TokioInstant::now() + ZAKURA_BLOCK_SYNC_CHECKPOINT_FRONTIER_REFRESH_INTERVAL,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn drive_block_sync_actions<ReadState, BlockVerifier>(
    mut actions: mpsc::Receiver<BlockSyncAction>,
    supervisor: zebra_network::zakura::ZakuraSupervisorHandle,
    endpoint: Option<ZakuraEndpoint>,
    block_sync: BlockSyncHandle,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    max_checkpoint_height: block::Height,
    checkpoint_apply_limit: usize,
    full_apply_limit: usize,
    combined_apply_limit: usize,
    trace: ZakuraTrace,
    shutdown: impl Future<Output = ()> + Send + 'static,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    pin!(shutdown);
    const {
        assert!(
            sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT <= zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP
        );
    }
    let checkpoint_apply_limit = checkpoint_apply_limit.clamp(
        sync::MIN_CHECKPOINT_CONCURRENCY_LIMIT,
        zebra_consensus::MAX_CHECKPOINT_HEIGHT_GAP,
    );
    let full_apply_limit = full_apply_limit.max(sync::MIN_CONCURRENCY_LIMIT);
    let combined_apply_limit = combined_apply_limit.max(sync::MIN_CONCURRENCY_LIMIT);
    let mut pending_applies = VecDeque::new();
    let mut in_flight_applies: FuturesUnordered<BoxFuture<'static, BlockApplyCompletion>> =
        FuturesUnordered::new();
    let mut checkpoint_in_flight = 0usize;
    let mut full_in_flight = 0usize;
    let mut deferred_actions = VecDeque::new();
    let mut checkpoint_frontier_refresh = CheckpointFrontierRefresh::default();

    loop {
        if !in_flight_applies.is_empty() {
            if let Some(Some(completed)) = in_flight_applies.next().now_or_never() {
                handle_completed_block_apply(
                    completed,
                    &mut pending_applies,
                    &mut in_flight_applies,
                    &mut checkpoint_in_flight,
                    &mut full_in_flight,
                    checkpoint_apply_limit,
                    full_apply_limit,
                    combined_apply_limit,
                    latest_chain_tip.clone(),
                    endpoint.clone(),
                    read_state.clone(),
                    block_verifier.clone(),
                    block_sync.clone(),
                    trace.clone(),
                    &mut checkpoint_frontier_refresh,
                );
                continue;
            }
        }

        let action = if let Some(action) =
            coalesce_ready_needed_block_queries(&mut actions, &mut deferred_actions)
        {
            action
        } else if let Some(action) = deferred_actions.pop_front() {
            action
        } else {
            select! {
                _ = &mut shutdown => return,
                completed = in_flight_applies.next(), if !in_flight_applies.is_empty() => {
                    let Some(completed) = completed else {
                        continue;
                    };
                    handle_completed_block_apply(
                        completed,
                        &mut pending_applies,
                        &mut in_flight_applies,
                        &mut checkpoint_in_flight,
                        &mut full_in_flight,
                        checkpoint_apply_limit,
                        full_apply_limit,
                        combined_apply_limit,
                        latest_chain_tip.clone(),
                        endpoint.clone(),
                        read_state.clone(),
                        block_verifier.clone(),
                        block_sync.clone(),
                        trace.clone(),
                        &mut checkpoint_frontier_refresh,
                    );
                    continue;
                }
                _ = async {
                    match checkpoint_frontier_refresh.next_attempt_at() {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                }, if checkpoint_frontier_refresh.next_attempt_at().is_some() => {
                    refresh_block_sync_frontiers_for_checkpoint_window(
                        read_state.clone(),
                        latest_chain_tip.clone(),
                        endpoint.clone(),
                        Some(block_sync.clone()),
                        trace.clone(),
                        &mut checkpoint_frontier_refresh,
                    ).await;
                    continue;
                }
                action = actions.recv() => {
                    let Some(action) = action else {
                        return;
                    };
                    action
                }
            }
        };
        let action =
            coalesce_stale_needed_block_queries(action, &mut actions, &mut deferred_actions);

        trace_block_driver_action(&trace, &action);
        match action {
            BlockSyncAction::SendMessage { .. } => {}
            BlockSyncAction::Misbehavior { peer, reason } => {
                if block_sync_misbehavior_is_hard(reason) {
                    debug!(
                        ?peer,
                        ?reason,
                        "disconnecting peer for Zakura block-sync violation"
                    );
                    let _ = supervisor.disconnect_peer(&peer).await;
                } else {
                    debug!(
                        ?peer,
                        ?reason,
                        "recorded soft Zakura block-sync peer violation"
                    );
                }
            }
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_START,
                    "block_sync_driver",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "query_needed_blocks");
                        insert_cs_height(row, cs_trace::VERIFIED_BLOCK_TIP, verified_block_tip);
                        insert_cs_height(row, cs_trace::BEST_HEADER_TIP, best_header_tip);
                    },
                );
                let started = Instant::now();
                match query_block_sync_needed_blocks(
                    read_state.clone(),
                    verified_block_tip,
                    best_header_tip,
                )
                .await
                {
                    Ok(blocks) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_SUCCESS,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "query_needed_blocks");
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, blocks.len() as u64);
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        let _ = block_sync.send_control(BlockSyncEvent::NeededBlocks(blocks));
                        emit_commit_state(
                            &trace,
                            cs_trace::REACTOR_EVENT_SENT,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "needed_blocks");
                            },
                        );
                    }
                    Err(error) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_ERROR,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "query_needed_blocks");
                                insert_cs_str(row, cs_trace::RESULT, "error");
                                insert_cs_str(row, cs_trace::REASON, &format!("{error}"));
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        warn!(
                            ?verified_block_tip,
                            ?best_header_tip,
                            ?error,
                            "failed to query Zakura block-sync needed blocks"
                        );
                    }
                }
            }
            BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_START,
                    "block_sync_driver",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "query_blocks_by_height_range");
                        insert_cs_peer(row, cs_trace::PEER, &peer);
                        insert_cs_height(row, cs_trace::RANGE_START, start);
                        insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                    },
                );
                let started = Instant::now();
                match tokio::time::timeout(
                    ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
                    read_state
                        .clone()
                        .oneshot(zebra_state::ReadRequest::BlocksByHeightRange { start, count }),
                )
                .await
                {
                    Ok(Ok(zebra_state::ReadResponse::Blocks(blocks))) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_SUCCESS,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(
                                    row,
                                    cs_trace::ACTION,
                                    "query_blocks_by_height_range",
                                );
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, blocks.len() as u64);
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        emit_commit_state(
                            &trace,
                            cs_trace::REACTOR_EVENT_SENT,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "block_range_response_ready");
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                            },
                        );
                        let _ = block_sync.send_control(BlockSyncEvent::BlockRangeResponseReady {
                            peer,
                            start_height: start,
                            requested_count: count,
                            blocks,
                        });
                    }
                    Ok(Ok(response)) => {
                        trace_block_range_error(
                            &trace,
                            &peer,
                            start,
                            count,
                            "unexpected_response",
                            started,
                        );
                        warn!(?peer, ?response, "unexpected BlocksByHeightRange response");
                        trace_block_range_finished(&trace, &peer, start, count, 0);
                        let _ =
                            block_sync.send_control(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            });
                    }
                    Ok(Err(error)) => {
                        trace_block_range_error(
                            &trace,
                            &peer,
                            start,
                            count,
                            &format!("{error}"),
                            started,
                        );
                        warn!(
                            ?peer,
                            ?error,
                            "failed to read Zakura Blocks response from state"
                        );
                        trace_block_range_finished(&trace, &peer, start, count, 0);
                        let _ =
                            block_sync.send_control(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            });
                    }
                    Err(_elapsed) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_TIMEOUT,
                            "block_sync_driver",
                            |row| {
                                insert_cs_str(
                                    row,
                                    cs_trace::ACTION,
                                    "query_blocks_by_height_range",
                                );
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        warn!(?peer, "timed out reading Zakura block-sync serving range");
                        trace_block_range_finished(&trace, &peer, start, count, 0);
                        let _ =
                            block_sync.send_control(BlockSyncEvent::BlockRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            });
                    }
                }
            }
            BlockSyncAction::SubmitBlock { token, block } => {
                let class = block_apply_class(block.as_ref(), max_checkpoint_height);
                emit_commit_state(
                    &trace,
                    cs_trace::BLOCK_SUBMIT_QUEUED,
                    "block_sync_driver",
                    |row| {
                        insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
                        insert_cs_str(row, cs_trace::APPLY_CLASS, block_apply_class_label(class));
                        insert_cs_hash(row, cs_trace::HASH, block.hash());
                        if let Some(height) = block.coinbase_height() {
                            insert_cs_height(row, cs_trace::HEIGHT, height);
                        }
                        insert_cs_u64(row, cs_trace::QUEUE_LEN, pending_applies.len() as u64);
                        insert_cs_u64(
                            row,
                            cs_trace::IN_FLIGHT_COUNT,
                            (checkpoint_in_flight.saturating_add(full_in_flight)) as u64,
                        );
                    },
                );
                pending_applies.push_back(PendingBlockApply {
                    token,
                    class,
                    block,
                });
                drain_pending_block_applies(
                    &mut pending_applies,
                    &mut in_flight_applies,
                    &mut checkpoint_in_flight,
                    &mut full_in_flight,
                    checkpoint_apply_limit,
                    full_apply_limit,
                    combined_apply_limit,
                    latest_chain_tip.clone(),
                    endpoint.clone(),
                    read_state.clone(),
                    block_verifier.clone(),
                    block_sync.clone(),
                    trace.clone(),
                );
            }
        }
    }
}

pub(crate) fn coalesce_ready_needed_block_queries(
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    deferred_actions: &mut VecDeque<BlockSyncAction>,
) -> Option<BlockSyncAction> {
    let mut latest_query = None;
    let mut retained = VecDeque::new();
    while let Some(action) = deferred_actions.pop_front() {
        match action {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                latest_query = Some((verified_block_tip, best_header_tip));
            }
            action => retained.push_back(action),
        }
    }
    *deferred_actions = retained;

    while let Ok(action) = actions.try_recv() {
        match action {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                latest_query = Some((verified_block_tip, best_header_tip));
            }
            action => deferred_actions.push_back(action),
        }
    }

    latest_query.map(
        |(verified_block_tip, best_header_tip)| BlockSyncAction::QueryNeededBlocks {
            verified_block_tip,
            best_header_tip,
        },
    )
}

pub(crate) fn coalesce_stale_needed_block_queries(
    action: BlockSyncAction,
    actions: &mut mpsc::Receiver<BlockSyncAction>,
    deferred_actions: &mut VecDeque<BlockSyncAction>,
) -> BlockSyncAction {
    let BlockSyncAction::QueryNeededBlocks {
        mut verified_block_tip,
        mut best_header_tip,
    } = action
    else {
        return action;
    };

    let mut coalesced_count = 0u64;
    while let Ok(action) = actions.try_recv() {
        match action {
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip: latest_verified_block_tip,
                best_header_tip: latest_best_header_tip,
            } => {
                verified_block_tip = latest_verified_block_tip;
                best_header_tip = latest_best_header_tip;
                coalesced_count = coalesced_count.saturating_add(1);
            }
            action => deferred_actions.push_back(action),
        }
    }

    if coalesced_count > 0 {
        metrics::counter!("sync.block.needed_query.coalesced").increment(coalesced_count);
    }

    BlockSyncAction::QueryNeededBlocks {
        verified_block_tip,
        best_header_tip,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_completed_block_apply<ReadState, BlockVerifier>(
    completed: BlockApplyCompletion,
    pending_applies: &mut VecDeque<PendingBlockApply>,
    in_flight_applies: &mut FuturesUnordered<BoxFuture<'static, BlockApplyCompletion>>,
    checkpoint_in_flight: &mut usize,
    full_in_flight: &mut usize,
    checkpoint_apply_limit: usize,
    full_apply_limit: usize,
    combined_apply_limit: usize,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
    endpoint: Option<ZakuraEndpoint>,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    block_sync: BlockSyncHandle,
    trace: ZakuraTrace,
    checkpoint_frontier_refresh: &mut CheckpointFrontierRefresh,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    match completed.class {
        BlockApplyClass::Checkpoint => {
            *checkpoint_in_flight = checkpoint_in_flight.saturating_sub(1);
        }
        BlockApplyClass::Full => {
            *full_in_flight = full_in_flight.saturating_sub(1);
        }
    }

    if let Some(highest_observed_at_apply) = completed.checkpoint_refresh_floor {
        checkpoint_frontier_refresh.observe_checkpoint_commit(highest_observed_at_apply);
    }

    drain_pending_block_applies(
        pending_applies,
        in_flight_applies,
        checkpoint_in_flight,
        full_in_flight,
        checkpoint_apply_limit,
        full_apply_limit,
        combined_apply_limit,
        latest_chain_tip,
        endpoint,
        read_state,
        block_verifier,
        block_sync,
        trace,
    );
}

#[allow(clippy::too_many_arguments)]
fn drain_pending_block_applies<ReadState, BlockVerifier>(
    pending_applies: &mut VecDeque<PendingBlockApply>,
    in_flight_applies: &mut FuturesUnordered<BoxFuture<'static, BlockApplyCompletion>>,
    checkpoint_in_flight: &mut usize,
    full_in_flight: &mut usize,
    checkpoint_apply_limit: usize,
    full_apply_limit: usize,
    combined_apply_limit: usize,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
    endpoint: Option<ZakuraEndpoint>,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    block_sync: BlockSyncHandle,
    trace: ZakuraTrace,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    // The checkpoint verifier can hold a complete range until its checkpoint is
    // reached. Keep room for the current range and the next complete range.
    let checkpoint_pipeline_apply_limit = checkpoint_apply_limit.saturating_mul(2);
    let checkpoint_combined_apply_limit = combined_apply_limit.max(checkpoint_pipeline_apply_limit);
    while let Some(index) = pending_applies
        .iter()
        .position(|pending| match pending.class {
            BlockApplyClass::Checkpoint => {
                *checkpoint_in_flight + *full_in_flight < checkpoint_combined_apply_limit
                    && *checkpoint_in_flight < checkpoint_pipeline_apply_limit
            }
            BlockApplyClass::Full => {
                *checkpoint_in_flight + *full_in_flight < combined_apply_limit
                    && *full_in_flight < full_apply_limit
            }
        })
    {
        let pending = pending_applies
            .remove(index)
            .expect("pending apply index was found in queue");

        match pending.class {
            BlockApplyClass::Checkpoint => {
                *checkpoint_in_flight = checkpoint_in_flight.saturating_add(1);
            }
            BlockApplyClass::Full => {
                *full_in_flight = full_in_flight.saturating_add(1);
            }
        }

        let class = pending.class;
        in_flight_applies.push(
            apply_block_sync_body(
                block_verifier.clone(),
                latest_chain_tip.clone(),
                endpoint.clone(),
                read_state.clone(),
                block_sync.clone(),
                pending.token,
                pending.block,
                class,
                trace.clone(),
            )
            .boxed(),
        );
    }
}

pub(crate) fn block_apply_class(
    block: &block::Block,
    max_checkpoint_height: block::Height,
) -> BlockApplyClass {
    if block
        .coinbase_height()
        .is_some_and(|height| height <= max_checkpoint_height)
    {
        BlockApplyClass::Checkpoint
    } else {
        BlockApplyClass::Full
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_block_sync_body<BlockVerifier, ReadState>(
    block_verifier: BlockVerifier,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
    endpoint: Option<ZakuraEndpoint>,
    read_state: ReadState,
    block_sync: BlockSyncHandle,
    token: BlockApplyToken,
    block: Arc<block::Block>,
    class: BlockApplyClass,
    trace: ZakuraTrace,
) -> BlockApplyCompletion
where
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let expected_hash = block.hash();
    let Some(height) = block.coinbase_height() else {
        warn!(
            ?expected_hash,
            "Zakura block sync cannot apply body without coinbase height"
        );
        return BlockApplyCompletion {
            class,
            checkpoint_refresh_floor: None,
        };
    };

    emit_commit_state(&trace, cs_trace::COMMIT_START, "block_sync_driver", |row| {
        insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
        insert_cs_str(row, cs_trace::APPLY_CLASS, block_apply_class_label(class));
        insert_cs_height(row, cs_trace::HEIGHT, height);
        insert_cs_hash(row, cs_trace::HASH, expected_hash);
    });
    let started = Instant::now();
    let result = commit_block_sync_body_with_stall_trace(
        block_verifier.clone(),
        block,
        class,
        &trace,
        token,
        height,
        expected_hash,
    )
    .await;
    emit_commit_state(
        &trace,
        cs_trace::COMMIT_FINISH,
        "block_sync_driver",
        |row| {
            insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
            insert_cs_str(row, cs_trace::APPLY_CLASS, block_apply_class_label(class));
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, expected_hash);
            insert_cs_str(row, cs_trace::RESULT, block_apply_result_label(result));
            insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
        },
    );
    emit_commit_state(
        &trace,
        cs_trace::FRONTIER_QUERY_START,
        "block_sync_driver",
        |row| {
            insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, expected_hash);
        },
    );
    let local_frontier =
        query_block_sync_frontiers(read_state.clone(), latest_chain_tip.clone()).await;
    if let Some(frontiers) = local_frontier {
        let change =
            if result == BlockApplyResult::Committed || result == BlockApplyResult::Duplicate {
                FrontierChange::VerifiedGrow
            } else {
                FrontierChange::Snapshot
            };
        if class == BlockApplyClass::Full || change != FrontierChange::VerifiedGrow {
            publish_body_frontier(endpoint.as_ref(), frontiers, change);
        }
    }
    emit_commit_state(
        &trace,
        cs_trace::FRONTIER_QUERY_FINISH,
        "block_sync_driver",
        |row| {
            insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, expected_hash);
            insert_cs_bool(row, cs_trace::LOCAL_FRONTIER, local_frontier.is_some());
            if let Some(frontiers) = &local_frontier {
                insert_cs_frontiers(row, frontiers);
            }
        },
    );

    let _ = block_sync.send_control(BlockSyncEvent::BlockApplyFinished {
        token,
        height,
        hash: expected_hash,
        result,
        local_frontier,
    });
    emit_commit_state(
        &trace,
        cs_trace::REACTOR_EVENT_SENT,
        "block_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, "block_apply_finished");
            insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, expected_hash);
            insert_cs_str(row, cs_trace::RESULT, block_apply_result_label(result));
            insert_cs_bool(row, cs_trace::LOCAL_FRONTIER, local_frontier.is_some());
        },
    );

    BlockApplyCompletion {
        class,
        checkpoint_refresh_floor: (class == BlockApplyClass::Checkpoint
            && result == BlockApplyResult::Committed)
            .then(|| {
                local_frontier
                    .map(|frontiers| frontiers.verified_block_tip)
                    .unwrap_or_else(|| height.previous().unwrap_or(height))
            }),
    }
}

pub(crate) fn block_sync_misbehavior_is_hard(reason: BlockSyncMisbehavior) -> bool {
    matches!(
        reason,
        BlockSyncMisbehavior::MalformedMessage
            | BlockSyncMisbehavior::UnsolicitedBlock
            | BlockSyncMisbehavior::GetBlocksTooLong
            | BlockSyncMisbehavior::InvalidBlock
            | BlockSyncMisbehavior::InvalidStatus
            | BlockSyncMisbehavior::UnsolicitedDone
            | BlockSyncMisbehavior::StatusSpam
    )
}

#[cfg(test)]
pub(crate) async fn commit_block_sync_body<BlockVerifier>(
    block_verifier: BlockVerifier,
    block: Arc<block::Block>,
    class: BlockApplyClass,
) -> BlockApplyResult
where
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    let expected_hash = block.hash();
    let height = block.coinbase_height();
    let commit = block_verifier
        .clone()
        .oneshot(zebra_consensus::Request::Commit(block));
    match class {
        BlockApplyClass::Checkpoint => block_commit_result(height, expected_hash, commit.await),
        BlockApplyClass::Full => {
            match tokio::time::timeout(ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT, commit).await {
                Ok(outcome) => block_commit_result(height, expected_hash, outcome),
                Err(_elapsed) => block_commit_timed_out(height, expected_hash),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn commit_block_sync_body_with_stall_trace<BlockVerifier>(
    block_verifier: BlockVerifier,
    block: Arc<block::Block>,
    class: BlockApplyClass,
    trace: &ZakuraTrace,
    token: BlockApplyToken,
    height: block::Height,
    expected_hash: block::Hash,
) -> BlockApplyResult
where
    BlockVerifier:
        Service<zebra_consensus::Request, Response = block::Hash> + Clone + Send + 'static,
    BlockVerifier::Error: std::fmt::Debug + Send + Sync + 'static,
    BlockVerifier::Future: Send + 'static,
{
    let commit = block_verifier
        .clone()
        .oneshot(zebra_consensus::Request::Commit(block));

    match class {
        BlockApplyClass::Checkpoint => {
            tokio::pin!(commit);
            tokio::select! {
                outcome = &mut commit => block_commit_result(Some(height), expected_hash, outcome),
                _ = tokio::time::sleep(ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT) => {
                    emit_commit_state(
                        trace,
                        cs_trace::COMMIT_STALLED,
                        "block_sync_driver",
                        |row| {
                            insert_cs_u64(row, cs_trace::APPLY_TOKEN, token);
                            insert_cs_str(row, cs_trace::APPLY_CLASS, block_apply_class_label(class));
                            insert_cs_height(row, cs_trace::HEIGHT, height);
                            insert_cs_hash(row, cs_trace::HASH, expected_hash);
                            insert_cs_u64(
                                row,
                                cs_trace::ELAPSED_MS,
                                ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT.as_millis().try_into().unwrap_or(u64::MAX),
                            );
                        },
                    );
                    block_commit_result(Some(height), expected_hash, commit.await)
                }
            }
        }
        BlockApplyClass::Full => {
            match tokio::time::timeout(ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT, commit).await {
                Ok(outcome) => block_commit_result(Some(height), expected_hash, outcome),
                Err(_elapsed) => block_commit_timed_out(Some(height), expected_hash),
            }
        }
    }
}

fn block_commit_result<E>(
    height: Option<block::Height>,
    expected_hash: block::Hash,
    outcome: Result<block::Hash, E>,
) -> BlockApplyResult
where
    E: std::fmt::Debug + Send + Sync + 'static,
{
    match outcome {
        Ok(committed_hash) if committed_hash == expected_hash => {
            debug!(
                ?height,
                ?committed_hash,
                "Zakura block sync committed block body through verifier"
            );
            BlockApplyResult::Committed
        }
        Ok(committed_hash) => {
            warn!(
                ?height,
                ?expected_hash,
                ?committed_hash,
                "Zakura block-sync verifier returned an unexpected hash"
            );
            BlockApplyResult::Rejected
        }
        Err(error) => {
            if block_verify_error_is_duplicate(&error) {
                debug!(
                    ?height,
                    ?expected_hash,
                    ?error,
                    "Zakura block-sync body was already known by the block verifier"
                );
                BlockApplyResult::Duplicate
            } else {
                debug!(
                    ?height,
                    ?expected_hash,
                    ?error,
                    "Zakura block-sync body rejected by block verifier"
                );
                BlockApplyResult::Rejected
            }
        }
    }
}

fn block_commit_timed_out(
    height: Option<block::Height>,
    expected_hash: block::Hash,
) -> BlockApplyResult {
    warn!(
        ?height,
        ?expected_hash,
        "timed out committing Zakura block-sync body"
    );
    BlockApplyResult::TimedOut
}

async fn refresh_block_sync_frontiers_for_checkpoint_window<ReadState>(
    read_state: ReadState,
    latest_chain_tip: impl ChainTip + Clone + Send + Sync + 'static,
    endpoint: Option<ZakuraEndpoint>,
    block_sync: Option<BlockSyncHandle>,
    trace: ZakuraTrace,
    refresh: &mut CheckpointFrontierRefresh,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let Some(mut highest_sent) = refresh.highest_sent else {
        return;
    };

    emit_commit_state(
        &trace,
        cs_trace::CHECKPOINT_REFRESH_ATTEMPT,
        "block_sync_driver",
        |row| {
            insert_cs_u64(row, "attempts_remaining", refresh.attempts_remaining as u64);
            insert_cs_height(row, cs_trace::VERIFIED_BLOCK_TIP, highest_sent);
        },
    );
    let Some(frontiers) =
        query_block_sync_frontiers(read_state.clone(), latest_chain_tip.clone()).await
    else {
        refresh.finish_attempt(highest_sent);
        return;
    };

    if frontiers.verified_block_tip <= highest_sent {
        refresh.finish_attempt(highest_sent);
        return;
    }

    highest_sent = frontiers.verified_block_tip;
    publish_body_frontier(endpoint.as_ref(), frontiers, FrontierChange::VerifiedGrow);
    if let Some(block_sync) = &block_sync {
        let _ = block_sync.send_control(BlockSyncEvent::ChainTipGrow(frontiers));
    }
    emit_commit_state(
        &trace,
        cs_trace::CHECKPOINT_REFRESH_SENT,
        "block_sync_driver",
        |row| {
            insert_cs_frontiers(row, &frontiers);
        },
    );
    refresh.finish_attempt(highest_sent);
}

fn publish_body_frontier(
    endpoint: Option<&ZakuraEndpoint>,
    frontiers: zebra_network::zakura::BlockSyncFrontiers,
    change: FrontierChange,
) {
    let Some(endpoint) = endpoint else {
        return;
    };
    let Some(mut update) = endpoint.current_sync_frontier() else {
        return;
    };
    if frontiers.finalized_height == frontiers.verified_block_tip {
        update.frontier.finalized =
            Frontier::new(frontiers.finalized_height, frontiers.verified_block_hash);
    }
    update.frontier.verified_body =
        Frontier::new(frontiers.verified_block_tip, frontiers.verified_block_hash);
    update.change = change;
    endpoint.publish_sync_frontier_from(update, "block_sync_driver");
}

pub(crate) async fn query_block_sync_needed_blocks<ReadState>(
    read_state: ReadState,
    verified_block_tip: block::Height,
    best_header_tip: block::Height,
) -> Result<Vec<BlockSyncBlockMeta>, zebra_state::BoxError>
where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let Some((from, limit)) = block_sync_missing_body_window(verified_block_tip, best_header_tip)
    else {
        return Ok(Vec::new());
    };

    let mut needed = Vec::new();
    let mut next_from = from;
    let mut remaining = limit;

    while remaining > 0 {
        let chunk_limit = remaining.min(zebra_state::constants::MAX_HEADER_SYNC_HEIGHT_RANGE);
        needed.extend(
            query_block_sync_needed_blocks_chunk(read_state.clone(), next_from, chunk_limit)
                .await?,
        );

        remaining = remaining.saturating_sub(chunk_limit);
        let Some(after_chunk) = next_from.0.checked_add(chunk_limit).map(block::Height) else {
            break;
        };
        next_from = after_chunk;
    }

    Ok(needed)
}

async fn query_block_sync_needed_blocks_chunk<ReadState>(
    read_state: ReadState,
    from: block::Height,
    limit: u32,
) -> Result<Vec<BlockSyncBlockMeta>, zebra_state::BoxError>
where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    let missing = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::MissingBlockBodies { from, limit }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::MissingBlockBodies(heights))) => heights,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected MissingBlockBodies response");
            return Ok(Vec::new());
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    let Some(first) = missing.first().copied() else {
        return Ok(Vec::new());
    };
    let Some(last) = missing.last().copied() else {
        return Ok(Vec::new());
    };
    let span = last.0.saturating_sub(first.0).saturating_add(1);

    let headers = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::HeadersByHeightRange {
                start: first,
                count: span,
            }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::Headers(headers))) => headers,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected HeadersByHeightRange response");
            return Ok(Vec::new());
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    let size_hints = match tokio::time::timeout(
        ZAKURA_BLOCK_SYNC_DRIVER_TIMEOUT,
        read_state.oneshot(zebra_state::ReadRequest::BlockSizeHints {
            from: first,
            count: span,
        }),
    )
    .await
    {
        Ok(Ok(zebra_state::ReadResponse::BlockSizeHints(hints))) => hints,
        Ok(Ok(response)) => {
            warn!(?response, "unexpected BlockSizeHints response");
            Vec::new()
        }
        Ok(Err(error)) => return Err(error),
        Err(elapsed) => return Err(Box::new(elapsed)),
    };

    Ok(block_sync_needed_blocks_from_state(
        missing, headers, size_hints,
    ))
}

pub(crate) fn block_sync_missing_body_window(
    verified_block_tip: block::Height,
    best_header_tip: block::Height,
) -> Option<(block::Height, u32)> {
    if best_header_tip <= verified_block_tip {
        return None;
    }

    let from = block::Height(verified_block_tip.0.saturating_add(1));
    let limit = best_header_tip
        .0
        .saturating_sub(verified_block_tip.0)
        .clamp(1, ZAKURA_BLOCK_SYNC_MISSING_BODY_WINDOW);
    Some((from, limit))
}

pub(crate) fn block_sync_needed_blocks_from_state(
    missing: Vec<block::Height>,
    headers: Vec<(block::Height, block::Hash, Arc<block::Header>)>,
    size_hints: Vec<(block::Height, Option<u32>)>,
) -> Vec<BlockSyncBlockMeta> {
    let headers: HashMap<_, _> = headers
        .into_iter()
        .map(|(height, hash, _header)| (height, hash))
        .collect();
    let size_hints: HashMap<_, _> = size_hints.into_iter().collect();

    missing
        .into_iter()
        .filter_map(|height| {
            let hash = *headers.get(&height)?;
            let size = size_hints
                .get(&height)
                .copied()
                .flatten()
                .filter(|size| *size > 0)
                .map(BlockSizeEstimate::Advertised)
                .unwrap_or(BlockSizeEstimate::Unknown);

            Some(BlockSyncBlockMeta { height, hash, size })
        })
        .collect()
}

fn trace_block_driver_action(trace: &ZakuraTrace, action: &BlockSyncAction) {
    emit_commit_state(
        trace,
        cs_trace::ACTION_RECEIVED,
        "block_sync_driver",
        |row| match action {
            BlockSyncAction::SendMessage { peer, msg } => {
                insert_cs_str(row, cs_trace::ACTION, "send_message");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_str(row, cs_trace::REASON, block_sync_message_label(msg));
            }
            BlockSyncAction::Misbehavior { peer, reason } => {
                insert_cs_str(row, cs_trace::ACTION, "misbehavior");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_str(row, cs_trace::REASON, block_sync_misbehavior_label(*reason));
            }
            BlockSyncAction::QueryNeededBlocks {
                verified_block_tip,
                best_header_tip,
            } => {
                insert_cs_str(row, cs_trace::ACTION, "query_needed_blocks");
                insert_cs_height(row, cs_trace::VERIFIED_BLOCK_TIP, *verified_block_tip);
                insert_cs_height(row, cs_trace::BEST_HEADER_TIP, *best_header_tip);
            }
            BlockSyncAction::QueryBlocksByHeightRange { peer, start, count } => {
                insert_cs_str(row, cs_trace::ACTION, "query_blocks_by_height_range");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_height(row, cs_trace::RANGE_START, *start);
                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(*count));
            }
            BlockSyncAction::SubmitBlock { token, block } => {
                insert_cs_str(row, cs_trace::ACTION, "submit_block");
                insert_cs_u64(row, cs_trace::APPLY_TOKEN, *token);
                insert_cs_hash(row, cs_trace::HASH, block.hash());
                if let Some(height) = block.coinbase_height() {
                    insert_cs_height(row, cs_trace::HEIGHT, height);
                }
            }
        },
    );
}

fn trace_block_range_error(
    trace: &ZakuraTrace,
    peer: &zebra_network::zakura::ZakuraPeerId,
    start: block::Height,
    count: u32,
    reason: &str,
    started: Instant,
) {
    emit_commit_state(
        trace,
        cs_trace::STATE_READ_ERROR,
        "block_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, "query_blocks_by_height_range");
            insert_cs_peer(row, cs_trace::PEER, peer);
            insert_cs_height(row, cs_trace::RANGE_START, start);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
            insert_cs_str(row, cs_trace::RESULT, "error");
            insert_cs_str(row, cs_trace::REASON, reason);
            insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
        },
    );
}

fn trace_block_range_finished(
    trace: &ZakuraTrace,
    peer: &zebra_network::zakura::ZakuraPeerId,
    start: block::Height,
    requested_count: u32,
    returned_count: u32,
) {
    emit_commit_state(
        trace,
        cs_trace::REACTOR_EVENT_SENT,
        "block_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, "block_range_response_finished");
            insert_cs_peer(row, cs_trace::PEER, peer);
            insert_cs_height(row, cs_trace::RANGE_START, start);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(returned_count));
            insert_cs_u64(row, "requested_count", u64::from(requested_count));
        },
    );
}

fn block_apply_class_label(class: BlockApplyClass) -> &'static str {
    match class {
        BlockApplyClass::Checkpoint => "checkpoint",
        BlockApplyClass::Full => "full",
    }
}

fn block_sync_message_label(msg: &BlockSyncMessage) -> &'static str {
    match msg {
        BlockSyncMessage::Status(_) => "status",
        BlockSyncMessage::Block(_) => "block",
        BlockSyncMessage::BlocksDone { .. } => "blocks_done",
        BlockSyncMessage::RangeUnavailable { .. } => "range_unavailable",
        BlockSyncMessage::GetBlocks { .. } => "get_blocks",
    }
}

fn block_sync_misbehavior_label(reason: BlockSyncMisbehavior) -> &'static str {
    match reason {
        BlockSyncMisbehavior::MalformedMessage => "malformed_message",
        BlockSyncMisbehavior::UnsolicitedBlock => "unsolicited_block",
        BlockSyncMisbehavior::GetBlocksTooLong => "get_blocks_too_long",
        BlockSyncMisbehavior::GetBlocksSpam => "get_blocks_spam",
        BlockSyncMisbehavior::InvalidBlock => "invalid_block",
        BlockSyncMisbehavior::SizeMismatch => "size_mismatch",
        BlockSyncMisbehavior::InvalidStatus => "invalid_status",
        BlockSyncMisbehavior::UnsolicitedDone => "unsolicited_done",
        BlockSyncMisbehavior::RangeUnavailable => "range_unavailable",
        BlockSyncMisbehavior::StatusSpam => "status_spam",
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
