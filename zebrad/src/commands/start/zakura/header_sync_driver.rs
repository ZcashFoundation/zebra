use std::{future::Future, time::Instant};

use color_eyre::eyre::{eyre, Report};
use tokio::{pin, select, sync::mpsc};
use tower::{Service, ServiceExt};
use tracing::{debug, warn};

use zebra_chain::{
    block::{self},
    chain_tip::ChainTip,
};
use zebra_network::zakura::{
    commit_state_trace as cs_trace, BlockSyncFrontiers, Frontier, FrontierChange, HeaderSyncAction,
    HeaderSyncCommitFailureKind, HeaderSyncEvent, HeaderSyncFrontiers, ZakuraEndpoint,
    ZakuraHeaderSyncDriverStartup, ZakuraTrace, DEFAULT_HS_RANGE,
};

#[cfg(test)]
use zebra_network::zakura::{BlockSyncEvent, BlockSyncHandle};

use super::{
    block_verify_error_is_duplicate, emit_commit_state, insert_cs_frontiers, insert_cs_hash,
    insert_cs_height, insert_cs_peer, insert_cs_str, insert_cs_u64, verified_block_tip_from_state,
};

pub(crate) async fn zakura_header_sync_driver_startup(
    read_state: zebra_state::ReadStateService,
    network: &zebra_chain::parameters::Network,
) -> Result<ZakuraHeaderSyncDriverStartup, Report> {
    let best_header_tip = match read_state
        .clone()
        .oneshot(zebra_state::ReadRequest::BestHeaderTip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::BestHeaderTip(tip) => tip,
        response => Err(eyre!("unexpected BestHeaderTip response: {response:?}"))?,
    };

    let finalized_tip = match read_state
        .clone()
        .oneshot(zebra_state::ReadRequest::FinalizedTip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::FinalizedTip(tip) => tip,
        response => Err(eyre!("unexpected FinalizedTip response: {response:?}"))?,
    };

    let verified_block_tip = match read_state
        .oneshot(zebra_state::ReadRequest::Tip)
        .await
        .map_err(|error| eyre!("{error}"))?
    {
        zebra_state::ReadResponse::Tip(tip) => tip,
        response => Err(eyre!("unexpected Tip response: {response:?}"))?,
    };

    let empty_state_tip = (block::Height(0), network.genesis_hash());
    let finalized_height = finalized_tip.map_or(block::Height(0), |(height, _)| height);
    let verified_block_tip =
        verified_block_tip_from_state(finalized_tip, verified_block_tip, empty_state_tip);
    Ok(ZakuraHeaderSyncDriverStartup {
        frontiers: HeaderSyncFrontiers {
            finalized_height,
            verified_block_tip: verified_block_tip.0,
            verified_block_hash: verified_block_tip.1,
        },
        best_header_tip: Some(best_header_tip.unwrap_or(empty_state_tip)),
        verified_block_tip_hash: verified_block_tip.1,
    })
}

#[derive(Clone)]
pub(crate) struct ZakuraHeaderSyncDriverHandles {
    pub(crate) endpoint: ZakuraEndpoint,
    pub(crate) header_sync: zebra_network::zakura::HeaderSyncHandle,
}

pub(crate) async fn drive_zakura_header_sync_actions<State, ReadState, BlockVerifier>(
    mut actions: mpsc::Receiver<HeaderSyncAction>,
    handles: ZakuraHeaderSyncDriverHandles,
    state: State,
    read_state: ReadState,
    block_verifier: BlockVerifier,
    trace: ZakuraTrace,
    shutdown: impl Future<Output = ()> + Send + 'static,
) where
    State: Service<
            zebra_state::Request,
            Response = zebra_state::Response,
            Error = zebra_state::BoxError,
        > + Clone
        + Send
        + 'static,
    State::Future: Send + 'static,
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
    loop {
        let action = select! {
            _ = &mut shutdown => return,
            action = actions.recv() => {
                let Some(action) = action else {
                    return;
                };
                action
            }
        };

        trace_header_driver_action(&trace, &action);
        match action {
            HeaderSyncAction::Misbehavior { peer, reason } => {
                debug!(
                    ?peer,
                    ?reason,
                    "disconnecting peer for Zakura header-sync violation"
                );
                let _ = handles.endpoint.supervisor().disconnect_peer(&peer).await;
            }
            HeaderSyncAction::NewBlockReceived {
                peer,
                height,
                hash,
                block,
            } => {
                emit_commit_state(
                    &trace,
                    cs_trace::COMMIT_START,
                    "header_sync_driver",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "new_block");
                        insert_cs_peer(row, cs_trace::PEER, &peer);
                        insert_cs_height(row, cs_trace::HEIGHT, height);
                        insert_cs_hash(row, cs_trace::HASH, hash);
                    },
                );
                let started = Instant::now();
                match block_verifier
                    .clone()
                    .oneshot(zebra_consensus::Request::Commit(block.clone()))
                    .await
                {
                    Ok(committed_hash) if committed_hash == hash => {
                        trace_header_commit_finish(
                            &trace,
                            "new_block",
                            &peer,
                            height,
                            hash,
                            "accepted",
                            started,
                        );
                        trace_header_reactor_event(
                            &trace,
                            "new_block_accepted",
                            Some(&peer),
                            height,
                            hash,
                            1,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockAccepted {
                                peer,
                                height,
                                hash,
                                block,
                            })
                            .await;
                    }
                    Ok(committed_hash) => {
                        trace_header_commit_finish(
                            &trace,
                            "new_block",
                            &peer,
                            height,
                            hash,
                            "rejected",
                            started,
                        );
                        warn!(
                            ?peer,
                            ?hash,
                            ?committed_hash,
                            "Zakura NewBlock verifier returned an unexpected hash"
                        );
                        trace_header_reactor_event(
                            &trace,
                            "new_block_rejected",
                            Some(&peer),
                            height,
                            hash,
                            1,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockRejected { peer, hash })
                            .await;
                    }
                    Err(error) => {
                        if block_verify_error_is_duplicate(&error) {
                            trace_header_commit_finish(
                                &trace,
                                "new_block",
                                &peer,
                                height,
                                hash,
                                "duplicate",
                                started,
                            );
                            debug!(
                                ?peer,
                                ?height,
                                ?hash,
                                ?error,
                                "Zakura NewBlock was already known by the block verifier"
                            );
                            trace_header_reactor_event(
                                &trace,
                                "new_block_duplicate",
                                Some(&peer),
                                height,
                                hash,
                                1,
                            );
                            let _ = handles
                                .header_sync
                                .send(HeaderSyncEvent::NewBlockDuplicate { peer, height, hash })
                                .await;
                            continue;
                        }

                        trace_header_commit_finish(
                            &trace,
                            "new_block",
                            &peer,
                            height,
                            hash,
                            "rejected",
                            started,
                        );
                        debug!(
                            ?peer,
                            ?hash,
                            ?error,
                            "Zakura NewBlock rejected by block verifier"
                        );
                        trace_header_reactor_event(
                            &trace,
                            "new_block_rejected",
                            Some(&peer),
                            height,
                            hash,
                            1,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::NewBlockRejected { peer, hash })
                            .await;
                    }
                }
            }
            HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                trace_state_read_start(
                    &trace,
                    "query_headers_by_height_range",
                    Some(&peer),
                    start,
                    count,
                );
                let started = Instant::now();
                match read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::HeadersByHeightRange { start, count })
                    .await
                {
                    Ok(zebra_state::ReadResponse::Headers(headers)) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_SUCCESS,
                            "header_sync_driver",
                            |row| {
                                insert_cs_str(
                                    row,
                                    cs_trace::ACTION,
                                    "query_headers_by_height_range",
                                );
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, headers.len() as u64);
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        trace_state_read_start(
                            &trace,
                            "block_size_hints",
                            Some(&peer),
                            start,
                            count,
                        );
                        let body_size_hints = match read_state
                            .clone()
                            .oneshot(zebra_state::ReadRequest::BlockSizeHints {
                                from: start,
                                count,
                            })
                            .await
                        {
                            Ok(zebra_state::ReadResponse::BlockSizeHints(hints)) => hints,
                            Ok(response) => {
                                trace_state_read_error(
                                    &trace,
                                    "block_size_hints",
                                    Some(&peer),
                                    start,
                                    count,
                                    "unexpected_response",
                                    started,
                                );
                                warn!(?peer, ?response, "unexpected BlockSizeHints response");
                                Vec::new()
                            }
                            Err(error) => {
                                trace_state_read_error(
                                    &trace,
                                    "block_size_hints",
                                    Some(&peer),
                                    start,
                                    count,
                                    &format!("{error}"),
                                    started,
                                );
                                warn!(
                                    ?peer,
                                    ?error,
                                    "failed to read Zakura BlockSizeHints response from state"
                                );
                                Vec::new()
                            }
                        };
                        let body_sizes = body_sizes_for_served_header_range(
                            start,
                            headers.iter().map(|(height, _, _)| *height),
                            &body_size_hints,
                        );
                        let headers = headers
                            .into_iter()
                            .map(|(_height, _hash, header)| header)
                            .collect();
                        trace_header_reactor_event(
                            &trace,
                            "header_range_response_ready",
                            Some(&peer),
                            start,
                            block::Hash([0; 32]),
                            count,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseReady {
                                peer,
                                start_height: start,
                                requested_count: count,
                                headers,
                                body_sizes,
                            })
                            .await;
                    }
                    Ok(response) => {
                        trace_state_read_error(
                            &trace,
                            "query_headers_by_height_range",
                            Some(&peer),
                            start,
                            count,
                            "unexpected_response",
                            started,
                        );
                        warn!(?peer, ?response, "unexpected HeadersByHeightRange response");
                        trace_header_range_finished(&trace, &peer, start, count, 0);
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                    Err(error) => {
                        trace_state_read_error(
                            &trace,
                            "query_headers_by_height_range",
                            Some(&peer),
                            start,
                            count,
                            &format!("{error}"),
                            started,
                        );
                        warn!(
                            ?peer,
                            ?error,
                            "failed to read Zakura Headers response from state"
                        );
                        trace_header_range_finished(&trace, &peer, start, count, 0);
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeResponseFinished {
                                peer,
                                start_height: start,
                                requested_count: count,
                                returned_count: 0,
                            })
                            .await;
                    }
                }
            }
            HeaderSyncAction::CommitHeaderRange {
                peer,
                anchor,
                start_height,
                headers,
                body_sizes,
                finalized: _finalized,
            } => {
                let count = u32::try_from(headers.len()).unwrap_or(u32::MAX);
                emit_commit_state(
                    &trace,
                    cs_trace::COMMIT_START,
                    "header_sync_driver",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "commit_header_range");
                        insert_cs_peer(row, cs_trace::PEER, &peer);
                        insert_cs_height(row, cs_trace::RANGE_START, start_height);
                        insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                        insert_cs_hash(row, cs_trace::HASH, anchor);
                    },
                );
                let started = Instant::now();
                match state
                    .clone()
                    .oneshot(zebra_state::Request::CommitHeaderRange {
                        anchor,
                        headers,
                        body_sizes,
                    })
                    .await
                {
                    Ok(zebra_state::Response::Committed(tip_hash)) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::COMMIT_FINISH,
                            "header_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "commit_header_range");
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start_height);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                                insert_cs_str(row, cs_trace::RESULT, "committed");
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        let tip_height =
                            block::Height(start_height.0.saturating_add(count.saturating_sub(1)));
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitted {
                                start_height,
                                tip_height,
                                tip_hash,
                            })
                            .await;
                        trace_header_reactor_event(
                            &trace,
                            "header_range_committed",
                            None,
                            tip_height,
                            tip_hash,
                            count,
                        );
                        publish_header_frontier(
                            &handles.endpoint,
                            tip_height,
                            tip_hash,
                            FrontierChange::HeaderAdvanced,
                            &trace,
                        );
                    }
                    Ok(response) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::COMMIT_FINISH,
                            "header_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "commit_header_range");
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start_height);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                                insert_cs_str(row, cs_trace::RESULT, "unexpected_response");
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        warn!(?peer, ?response, "unexpected CommitHeaderRange response");
                        trace_header_reactor_event(
                            &trace,
                            "header_range_commit_failed",
                            Some(&peer),
                            start_height,
                            block::Hash([0; 32]),
                            count,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                peer,
                                start_height,
                                count,
                                kind: HeaderSyncCommitFailureKind::Local,
                            })
                            .await;
                    }
                    Err(error) => {
                        let kind = header_range_commit_failure_kind(error.as_ref());
                        emit_commit_state(
                            &trace,
                            cs_trace::COMMIT_FINISH,
                            "header_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "commit_header_range");
                                insert_cs_peer(row, cs_trace::PEER, &peer);
                                insert_cs_height(row, cs_trace::RANGE_START, start_height);
                                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
                                insert_cs_str(
                                    row,
                                    cs_trace::RESULT,
                                    commit_failure_result_label(kind),
                                );
                                insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                            },
                        );
                        debug!(
                            ?peer,
                            ?start_height,
                            ?count,
                            ?kind,
                            ?error,
                            "Zakura header range commit failed"
                        );
                        trace_header_reactor_event(
                            &trace,
                            "header_range_commit_failed",
                            Some(&peer),
                            start_height,
                            block::Hash([0; 32]),
                            count,
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitFailed {
                                peer,
                                start_height,
                                count,
                                kind,
                            })
                            .await;
                    }
                }
            }
            HeaderSyncAction::QueryBestHeaderTip => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_START,
                    "header_sync_driver",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "query_best_header_tip");
                    },
                );
                match read_state
                    .clone()
                    .oneshot(zebra_state::ReadRequest::BestHeaderTip)
                    .await
                {
                    Ok(zebra_state::ReadResponse::BestHeaderTip(Some((tip_height, tip_hash)))) => {
                        emit_commit_state(
                            &trace,
                            cs_trace::STATE_READ_SUCCESS,
                            "header_sync_driver",
                            |row| {
                                insert_cs_str(row, cs_trace::ACTION, "query_best_header_tip");
                                insert_cs_height(row, cs_trace::BEST_HEADER_TIP, tip_height);
                                insert_cs_hash(row, cs_trace::HASH, tip_hash);
                            },
                        );
                        let _ = handles
                            .header_sync
                            .send(HeaderSyncEvent::HeaderRangeCommitted {
                                start_height: tip_height,
                                tip_height,
                                tip_hash,
                            })
                            .await;
                        publish_header_frontier(
                            &handles.endpoint,
                            tip_height,
                            tip_hash,
                            FrontierChange::HeaderAdvanced,
                            &trace,
                        );
                    }
                    Ok(zebra_state::ReadResponse::BestHeaderTip(None)) => {}
                    Ok(response) => {
                        trace_state_read_error(
                            &trace,
                            "query_best_header_tip",
                            None,
                            block::Height(0),
                            0,
                            "unexpected_response",
                            Instant::now(),
                        );
                        warn!(?response, "unexpected BestHeaderTip response")
                    }
                    Err(error) => {
                        trace_state_read_error(
                            &trace,
                            "query_best_header_tip",
                            None,
                            block::Height(0),
                            0,
                            &format!("{error}"),
                            Instant::now(),
                        );
                        warn!(?error, "failed to query Zakura best header tip")
                    }
                }
            }
            HeaderSyncAction::QueryMissingBlockBodies { from, limit } => {
                log_missing_block_bodies(read_state.clone(), from, limit, &trace).await;
            }
            HeaderSyncAction::BodyGaps { from, to } => {
                let limit =
                    to.0.saturating_sub(from.0)
                        .saturating_add(1)
                        .min(DEFAULT_HS_RANGE);
                log_missing_block_bodies(read_state.clone(), from, limit, &trace).await;
            }
            HeaderSyncAction::HeaderAdvanced { height, hash } => {
                publish_header_frontier(
                    &handles.endpoint,
                    height,
                    hash,
                    FrontierChange::HeaderAdvanced,
                    &trace,
                );
            }
            HeaderSyncAction::HeaderReanchored { old: _, new } => {
                publish_header_frontier(
                    &handles.endpoint,
                    new.0,
                    new.1,
                    FrontierChange::HeaderReanchored,
                    &trace,
                );
            }
        }
    }
}

pub(crate) fn publish_header_frontier(
    endpoint: &ZakuraEndpoint,
    height: block::Height,
    hash: block::Hash,
    change: FrontierChange,
    trace: &ZakuraTrace,
) {
    let Some(mut update) = endpoint.current_sync_frontier() else {
        return;
    };

    update.frontier.best_header = Frontier::new(height, hash);
    update.change = change;
    endpoint.publish_sync_frontier_from(update, "header_sync_driver");
    emit_commit_state(
        trace,
        cs_trace::BLOCK_SYNC_NOTIFY_SENT,
        "header_sync_driver",
        |row| {
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, hash);
        },
    );
}

#[cfg(test)]
pub(crate) async fn notify_block_sync_header_tip(
    block_sync: Option<&BlockSyncHandle>,
    height: block::Height,
    hash: block::Hash,
    trace: &ZakuraTrace,
) {
    if let Some(block_sync) = block_sync {
        let _ = block_sync
            .send(BlockSyncEvent::HeaderTipChanged { height, hash })
            .await;
        emit_commit_state(
            trace,
            cs_trace::BLOCK_SYNC_NOTIFY_SENT,
            "header_sync_driver",
            |row| {
                insert_cs_height(row, cs_trace::HEIGHT, height);
                insert_cs_hash(row, cs_trace::HASH, hash);
            },
        );
    }
}

pub(crate) fn body_sizes_for_served_header_range(
    start: block::Height,
    header_heights: impl IntoIterator<Item = block::Height>,
    body_size_hints: &[(block::Height, Option<u32>)],
) -> Vec<u32> {
    header_heights
        .into_iter()
        .map(|height| {
            let Some(offset) = usize::try_from(height - start).ok() else {
                return 0;
            };

            body_size_hints
                .get(offset)
                .and_then(|(hint_height, size)| {
                    (*hint_height == height).then_some(size.unwrap_or(0))
                })
                .unwrap_or(0)
        })
        .collect()
}

async fn log_missing_block_bodies<ReadState>(
    read_state: ReadState,
    from: block::Height,
    limit: u32,
    trace: &ZakuraTrace,
) where
    ReadState: Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = zebra_state::BoxError,
        > + Send
        + 'static,
    ReadState::Future: Send + 'static,
{
    trace_state_read_start(trace, "missing_block_bodies", None, from, limit);
    let started = Instant::now();
    match read_state
        .oneshot(zebra_state::ReadRequest::MissingBlockBodies { from, limit })
        .await
    {
        Ok(zebra_state::ReadResponse::MissingBlockBodies(heights)) => {
            emit_commit_state(
                trace,
                cs_trace::STATE_READ_SUCCESS,
                "header_sync_driver",
                |row| {
                    insert_cs_str(row, cs_trace::ACTION, "missing_block_bodies");
                    insert_cs_height(row, cs_trace::RANGE_START, from);
                    insert_cs_u64(row, cs_trace::RANGE_COUNT, heights.len() as u64);
                    insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
                },
            );
            let first = heights.first().copied();
            let last = heights.last().copied();
            let count = heights.len();
            debug!(
                ?from,
                ?limit,
                ?count,
                ?first,
                ?last,
                "Zakura header-known body gaps from state"
            );
        }
        Ok(response) => {
            trace_state_read_error(
                trace,
                "missing_block_bodies",
                None,
                from,
                limit,
                "unexpected_response",
                started,
            );
            warn!(?response, "unexpected MissingBlockBodies response")
        }
        Err(error) => {
            trace_state_read_error(
                trace,
                "missing_block_bodies",
                None,
                from,
                limit,
                &format!("{error}"),
                started,
            );
            warn!(?error, "failed to query Zakura missing block bodies")
        }
    }
}

pub(crate) fn header_range_commit_failure_kind(
    error: &(dyn std::error::Error + Send + Sync + 'static),
) -> HeaderSyncCommitFailureKind {
    let Some(error) = error.downcast_ref::<zebra_state::CommitHeaderRangeError>() else {
        return HeaderSyncCommitFailureKind::Local;
    };

    match error {
        zebra_state::CommitHeaderRangeError::StorageWriteError { .. }
        | zebra_state::CommitHeaderRangeError::MissingGenesisAnchor { .. }
        | zebra_state::CommitHeaderRangeError::SendCommitRequestFailed
        // A lower-work conflicting range is individually valid (each header passed
        // PoW, difficulty, and contextual checks); the peer simply offered a worse
        // fork. Treat it as non-scoring so this stays a liveness/correctness guard,
        // not peer punishment.
        | zebra_state::CommitHeaderRangeError::LowerWorkConflict { .. }
        | zebra_state::CommitHeaderRangeError::CommitResponseDropped => {
            HeaderSyncCommitFailureKind::Local
        }
        zebra_state::CommitHeaderRangeError::EmptyRange
        | zebra_state::CommitHeaderRangeError::RangeTooLong { .. }
        | zebra_state::CommitHeaderRangeError::UnknownAnchor { .. }
        | zebra_state::CommitHeaderRangeError::HeightOverflow
        | zebra_state::CommitHeaderRangeError::ImmutableConflict { .. }
        | zebra_state::CommitHeaderRangeError::ReorgTooDeep { .. }
        | zebra_state::CommitHeaderRangeError::CheckpointConflict { .. }
        | zebra_state::CommitHeaderRangeError::ConflictingFullBlockHeader { .. }
        | zebra_state::CommitHeaderRangeError::ValidateContextError(_) => {
            HeaderSyncCommitFailureKind::InvalidPeerRange
        }
        _ => HeaderSyncCommitFailureKind::Local,
    }
}

pub(crate) async fn mirror_zakura_full_block_commits<ReadState>(
    mut chain_tip_change: zebra_state::ChainTipChange,
    latest_chain_tip: zebra_state::LatestChainTip,
    read_state: ReadState,
    header_sync: zebra_network::zakura::HeaderSyncHandle,
    endpoint: ZakuraEndpoint,
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
{
    pin!(shutdown);
    loop {
        let action = select! {
            _ = &mut shutdown => return,
            action = chain_tip_change.wait_for_tip_change() => {
                let Ok(action) = action else {
                    return;
                };
                action
            }
        };
        let height = action.best_tip_height();
        let hash = action.best_tip_hash();
        emit_commit_state(
            &trace,
            cs_trace::CHAIN_TIP_ACTION,
            "chain_tip_mirror",
            |row| {
                insert_cs_str(row, cs_trace::ACTION, tip_action_label(&action));
                insert_cs_height(row, cs_trace::HEIGHT, height);
                insert_cs_hash(row, cs_trace::HASH, hash);
            },
        );

        let finalized_tip = match read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::FinalizedTip)
            .await
        {
            Ok(zebra_state::ReadResponse::FinalizedTip(tip)) => tip,
            Ok(response) => {
                warn!(?response, "unexpected FinalizedTip response");
                None
            }
            Err(error) => {
                warn!(?error, "failed to query Zakura finalized frontier");
                None
            }
        };
        let finalized_height = finalized_tip.map_or(block::Height(0), |(height, _)| height);
        emit_commit_state(
            &trace,
            cs_trace::STATE_READ_SUCCESS,
            "chain_tip_mirror",
            |row| {
                insert_cs_str(row, cs_trace::ACTION, "finalized_tip");
                insert_cs_height(row, cs_trace::FINALIZED_HEIGHT, finalized_height);
            },
        );
        let action_tip = Some((height, hash));
        let verified_block_tip =
            verified_block_tip_from_state(finalized_tip, action_tip, (height, hash));
        let verified_block_tip = verified_block_tip_from_state(
            Some(verified_block_tip),
            latest_chain_tip.best_tip_height_and_hash(),
            verified_block_tip,
        );

        emit_commit_state(
            &trace,
            cs_trace::FRONTIER_DERIVED,
            "chain_tip_mirror",
            |row| {
                insert_cs_str(row, cs_trace::ACTION, "sync_exchange_frontier_derived");
                insert_cs_height(row, cs_trace::FINALIZED_HEIGHT, finalized_height);
                insert_cs_height(row, cs_trace::VERIFIED_BLOCK_TIP, verified_block_tip.0);
                insert_cs_hash(row, cs_trace::VERIFIED_BLOCK_HASH, verified_block_tip.1);
            },
        );
        if let Some(mut update) = endpoint.current_sync_frontier() {
            let previous_verified_body = update.frontier.verified_body.height;
            if let Some((finalized_height, finalized_hash)) = finalized_tip {
                update.frontier.finalized = Frontier::new(finalized_height, finalized_hash);
            }
            update.frontier.verified_body =
                Frontier::new(verified_block_tip.0, verified_block_tip.1);
            update.change = chain_tip_mirror_frontier_change(
                &action,
                previous_verified_body,
                verified_block_tip.0,
            );
            endpoint.publish_sync_frontier_from(update, "chain_tip_mirror");
            emit_commit_state(
                &trace,
                cs_trace::FRONTIER_DERIVED,
                "chain_tip_mirror",
                |row| {
                    let frontiers = BlockSyncFrontiers {
                        finalized_height,
                        verified_block_tip: verified_block_tip.0,
                        verified_block_hash: verified_block_tip.1,
                    };
                    insert_cs_str(row, cs_trace::ACTION, "sync_exchange_frontier_sent");
                    insert_cs_frontiers(row, &frontiers);
                },
            );
        }

        emit_commit_state(
            &trace,
            cs_trace::STATE_READ_START,
            "chain_tip_mirror",
            |row| {
                insert_cs_str(row, cs_trace::ACTION, "committed_tip_block");
                insert_cs_height(row, cs_trace::HEIGHT, height);
                insert_cs_hash(row, cs_trace::HASH, hash);
            },
        );
        match read_state
            .clone()
            .oneshot(zebra_state::ReadRequest::Block(hash.into()))
            .await
        {
            Ok(zebra_state::ReadResponse::Block(Some(block))) => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_SUCCESS,
                    "chain_tip_mirror",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "committed_tip_block");
                        insert_cs_height(row, cs_trace::HEIGHT, height);
                        insert_cs_hash(row, cs_trace::HASH, hash);
                        insert_cs_str(row, cs_trace::RESULT, "found");
                    },
                );
                let _ = header_sync
                    .send(HeaderSyncEvent::FullBlockCommitted {
                        height,
                        hash,
                        header: block.header.clone(),
                    })
                    .await;
                emit_commit_state(
                    &trace,
                    cs_trace::REACTOR_EVENT_SENT,
                    "chain_tip_mirror",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "full_block_committed");
                        insert_cs_height(row, cs_trace::HEIGHT, height);
                        insert_cs_hash(row, cs_trace::HASH, hash);
                    },
                );
            }
            Ok(zebra_state::ReadResponse::Block(None)) => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_SUCCESS,
                    "chain_tip_mirror",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "committed_tip_block");
                        insert_cs_height(row, cs_trace::HEIGHT, height);
                        insert_cs_hash(row, cs_trace::HASH, hash);
                        insert_cs_str(row, cs_trace::RESULT, "missing");
                    },
                );
                debug!(
                    ?height,
                    ?hash,
                    "Zakura full-block mirror could not find committed tip block"
                );
            }
            Ok(response) => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_ERROR,
                    "chain_tip_mirror",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "committed_tip_block");
                        insert_cs_str(row, cs_trace::REASON, "unexpected_response");
                    },
                );
                warn!(?response, "unexpected block lookup response")
            }
            Err(error) => {
                emit_commit_state(
                    &trace,
                    cs_trace::STATE_READ_ERROR,
                    "chain_tip_mirror",
                    |row| {
                        insert_cs_str(row, cs_trace::ACTION, "committed_tip_block");
                        insert_cs_str(row, cs_trace::REASON, &format!("{error}"));
                    },
                );
                warn!(?error, "failed to mirror Zakura full-block commit")
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn block_sync_chain_tip_event(
    action: &zebra_state::TipAction,
    frontiers: BlockSyncFrontiers,
) -> BlockSyncEvent {
    match action {
        zebra_state::TipAction::Grow { .. } => BlockSyncEvent::ChainTipGrow(frontiers),
        zebra_state::TipAction::Reset { .. } => BlockSyncEvent::ChainTipReset(frontiers),
    }
}

pub(crate) fn chain_tip_mirror_frontier_change(
    action: &zebra_state::TipAction,
    previous_verified_body: block::Height,
    verified_block_tip: block::Height,
) -> FrontierChange {
    match action {
        zebra_state::TipAction::Grow { .. } => FrontierChange::VerifiedGrow,
        zebra_state::TipAction::Reset { .. } if verified_block_tip > previous_verified_body => {
            FrontierChange::VerifiedGrow
        }
        zebra_state::TipAction::Reset { .. } => FrontierChange::VerifiedReset,
    }
}

fn trace_header_driver_action(trace: &ZakuraTrace, action: &HeaderSyncAction) {
    emit_commit_state(
        trace,
        cs_trace::ACTION_RECEIVED,
        "header_sync_driver",
        |row| match action {
            HeaderSyncAction::CommitHeaderRange {
                peer,
                start_height,
                headers,
                ..
            } => {
                insert_cs_str(row, cs_trace::ACTION, "commit_header_range");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_height(row, cs_trace::RANGE_START, *start_height);
                insert_cs_u64(row, cs_trace::RANGE_COUNT, headers.len() as u64);
            }
            HeaderSyncAction::QueryBestHeaderTip => {
                insert_cs_str(row, cs_trace::ACTION, "query_best_header_tip");
            }
            HeaderSyncAction::QueryHeadersByHeightRange { peer, start, count } => {
                insert_cs_str(row, cs_trace::ACTION, "query_headers_by_height_range");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_height(row, cs_trace::RANGE_START, *start);
                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(*count));
            }
            HeaderSyncAction::QueryMissingBlockBodies { from, limit } => {
                insert_cs_str(row, cs_trace::ACTION, "query_missing_block_bodies");
                insert_cs_height(row, cs_trace::RANGE_START, *from);
                insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(*limit));
            }
            HeaderSyncAction::Misbehavior { peer, reason } => {
                insert_cs_str(row, cs_trace::ACTION, "misbehavior");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_str(row, cs_trace::REASON, header_misbehavior_label(*reason));
            }
            HeaderSyncAction::BodyGaps { from, to } => {
                insert_cs_str(row, cs_trace::ACTION, "body_gaps");
                insert_cs_height(row, cs_trace::RANGE_START, *from);
                insert_cs_u64(
                    row,
                    cs_trace::RANGE_COUNT,
                    u64::from(to.0.saturating_sub(from.0).saturating_add(1)),
                );
            }
            HeaderSyncAction::HeaderAdvanced { height, hash } => {
                insert_cs_str(row, cs_trace::ACTION, "header_advanced");
                insert_cs_height(row, cs_trace::HEIGHT, *height);
                insert_cs_hash(row, cs_trace::HASH, *hash);
            }
            HeaderSyncAction::HeaderReanchored { old, new } => {
                insert_cs_str(row, cs_trace::ACTION, "header_reanchored");
                insert_cs_height(row, cs_trace::BEST_HEADER_TIP, old.0);
                insert_cs_height(row, cs_trace::HEIGHT, new.0);
                insert_cs_hash(row, cs_trace::HASH, new.1);
            }
            HeaderSyncAction::NewBlockReceived {
                peer, height, hash, ..
            } => {
                insert_cs_str(row, cs_trace::ACTION, "new_block_received");
                insert_cs_peer(row, cs_trace::PEER, peer);
                insert_cs_height(row, cs_trace::HEIGHT, *height);
                insert_cs_hash(row, cs_trace::HASH, *hash);
            }
        },
    );
}

fn trace_header_commit_finish(
    trace: &ZakuraTrace,
    action: &'static str,
    peer: &zebra_network::zakura::ZakuraPeerId,
    height: block::Height,
    hash: block::Hash,
    result: &'static str,
    started: Instant,
) {
    emit_commit_state(
        trace,
        cs_trace::COMMIT_FINISH,
        "header_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, action);
            insert_cs_peer(row, cs_trace::PEER, peer);
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, hash);
            insert_cs_str(row, cs_trace::RESULT, result);
            insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
        },
    );
}

fn trace_header_reactor_event(
    trace: &ZakuraTrace,
    action: &'static str,
    peer: Option<&zebra_network::zakura::ZakuraPeerId>,
    height: block::Height,
    hash: block::Hash,
    count: u32,
) {
    emit_commit_state(
        trace,
        cs_trace::REACTOR_EVENT_SENT,
        "header_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, action);
            if let Some(peer) = peer {
                insert_cs_peer(row, cs_trace::PEER, peer);
            }
            insert_cs_height(row, cs_trace::HEIGHT, height);
            insert_cs_hash(row, cs_trace::HASH, hash);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
        },
    );
}

fn trace_header_range_finished(
    trace: &ZakuraTrace,
    peer: &zebra_network::zakura::ZakuraPeerId,
    start: block::Height,
    requested_count: u32,
    returned_count: u32,
) {
    emit_commit_state(
        trace,
        cs_trace::REACTOR_EVENT_SENT,
        "header_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, "header_range_response_finished");
            insert_cs_peer(row, cs_trace::PEER, peer);
            insert_cs_height(row, cs_trace::RANGE_START, start);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(returned_count));
            insert_cs_u64(row, "requested_count", u64::from(requested_count));
        },
    );
}

fn trace_state_read_start(
    trace: &ZakuraTrace,
    action: &'static str,
    peer: Option<&zebra_network::zakura::ZakuraPeerId>,
    start: block::Height,
    count: u32,
) {
    emit_commit_state(
        trace,
        cs_trace::STATE_READ_START,
        "header_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, action);
            if let Some(peer) = peer {
                insert_cs_peer(row, cs_trace::PEER, peer);
            }
            insert_cs_height(row, cs_trace::RANGE_START, start);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
        },
    );
}

fn trace_state_read_error(
    trace: &ZakuraTrace,
    action: &'static str,
    peer: Option<&zebra_network::zakura::ZakuraPeerId>,
    start: block::Height,
    count: u32,
    reason: &str,
    started: Instant,
) {
    emit_commit_state(
        trace,
        cs_trace::STATE_READ_ERROR,
        "header_sync_driver",
        |row| {
            insert_cs_str(row, cs_trace::ACTION, action);
            if let Some(peer) = peer {
                insert_cs_peer(row, cs_trace::PEER, peer);
            }
            insert_cs_height(row, cs_trace::RANGE_START, start);
            insert_cs_u64(row, cs_trace::RANGE_COUNT, u64::from(count));
            insert_cs_str(row, cs_trace::REASON, reason);
            insert_cs_u64(row, cs_trace::ELAPSED_MS, elapsed_ms(started));
        },
    );
}

fn commit_failure_result_label(kind: HeaderSyncCommitFailureKind) -> &'static str {
    match kind {
        HeaderSyncCommitFailureKind::InvalidPeerRange => "invalid_peer_range",
        HeaderSyncCommitFailureKind::Local => "local_error",
    }
}

fn header_misbehavior_label(reason: zebra_network::zakura::HeaderSyncMisbehavior) -> &'static str {
    match reason {
        zebra_network::zakura::HeaderSyncMisbehavior::InvalidStatus => "invalid_status",
        zebra_network::zakura::HeaderSyncMisbehavior::UnsolicitedHeaders => "unsolicited_headers",
        zebra_network::zakura::HeaderSyncMisbehavior::EmptyHeaders => "empty_headers",
        zebra_network::zakura::HeaderSyncMisbehavior::ResponseTooLong => "response_too_long",
        zebra_network::zakura::HeaderSyncMisbehavior::InvalidRange => "invalid_range",
        zebra_network::zakura::HeaderSyncMisbehavior::MalformedMessage => "malformed_message",
        zebra_network::zakura::HeaderSyncMisbehavior::StatusSpam => "status_spam",
        zebra_network::zakura::HeaderSyncMisbehavior::NewBlockSpam => "new_block_spam",
        zebra_network::zakura::HeaderSyncMisbehavior::GetHeadersSpam => "get_headers_spam",
        zebra_network::zakura::HeaderSyncMisbehavior::GetHeadersTooLong => "get_headers_too_long",
        zebra_network::zakura::HeaderSyncMisbehavior::UnknownPeer => "unknown_peer",
        zebra_network::zakura::HeaderSyncMisbehavior::InvalidNewBlock => "invalid_new_block",
    }
}

fn tip_action_label(action: &zebra_state::TipAction) -> &'static str {
    match action {
        zebra_state::TipAction::Grow { .. } => "grow",
        zebra_state::TipAction::Reset { .. } => "reset",
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
