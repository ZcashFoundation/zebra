//! Implements `Indexer` methods on the `IndexerRPC` type

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    time::Duration,
};

use futures::Stream;
use prost::Message as _;
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Response, Status};
use tower::{util::ServiceExt, BoxError, Service};

use tracing::Span;
use zebra_chain::{
    block,
    chain_tip::ChainTip,
    serialization::{BytesInDisplayOrder, ZcashSerialize},
    transaction::{UnminedTxId, VerifiedUnminedTx},
};
use zebra_node_services::mempool::{
    self, replica_digest, MempoolChangeKind, QueuedStage, RemovedReason, REPLICA_DIGEST_LEN,
};
use zebra_state::{ReadRequest, ReadResponse, ReadState, MAX_NON_FINALIZED_CHAIN_FORKS};

use super::{
    indexer_server::Indexer, mempool_event, mempool_removed, server::IndexerRPC, BlockAndHash,
    BlockHashAndHeight, BlockRequest, ChainStateChangeMessage, ChainStateChangeRequest, Empty,
    MempoolAdded, MempoolBatch, MempoolEvent, MempoolQueued, MempoolRemoved,
    NonFinalizedStateChangeRequest, QueuedStage as ProtoQueuedStage, StateInfo, StateInfoProvider,
    UnminedTxId as ProtoUnminedTxId,
};

/// The maximum number of messages that can be queued to be streamed to a client.
const RESPONSE_BUFFER_SIZE: usize = 64;

/// How long to wait for a backpressured send to the non-finalized stream before treating the
/// consumer as hung and dropping the subscription.
///
/// The non-finalized stream applies backpressure (rather than dropping blocks) so a slow consumer
/// doesn't miss blocks, but without a bound a consumer whose connection is half-open (dead TCP not
/// yet detected) would block the listener task indefinitely.
const NON_FINALIZED_SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a backpressured send to a `SyncMempool` follower before treating it as a
/// slow consumer and dropping the connection (design §5 "drop-and-resync").
///
/// Unlike the chain-state stream, the mempool stream cannot let a slow consumer fall behind: the
/// per-batch checksum requires applying every batch in order, so a dropped batch would force a
/// re-bootstrap anyway. We make that explicit by dropping the connection once a send stays blocked
/// past this bound; the follower reconnects and re-bootstraps.
const SYNC_MEMPOOL_SEND_TIMEOUT: Duration = Duration::from_secs(60);

/// The maximum encoded size of a single bootstrap `MempoolBatch`, kept well under gRPC's default
/// 4 MiB message limit so a large bootstrap is split across several batches (design §5).
const BOOTSTRAP_MAX_BATCH_BYTES: usize = 3 * 1024 * 1024;

#[tonic::async_trait]
impl<ReadStateService, Tip, Mempool> Indexer for IndexerRPC<ReadStateService, Tip, Mempool>
where
    ReadStateService: ReadState + StateInfoProvider,
    Tip: ChainTip + Clone + Send + Sync + 'static,
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Clone
        + Send
        + Sync
        + 'static,
    Mempool::Future: Send,
{
    type ChainStateChangeStream =
        Pin<Box<dyn Stream<Item = Result<ChainStateChangeMessage, Status>> + Send>>;
    type ChainTipChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockHashAndHeight, Status>> + Send>>;
    type NonFinalizedStateChangeStream =
        Pin<Box<dyn Stream<Item = Result<BlockAndHash, Status>> + Send>>;
    type SyncMempoolStream = Pin<Box<dyn Stream<Item = Result<MempoolBatch, Status>> + Send>>;

    async fn chain_state_change(
        &self,
        request: tonic::Request<ChainStateChangeRequest>,
    ) -> Result<Response<Self::ChainStateChangeStream>, Status> {
        let span = Span::current();
        let read_state = self.read_state.clone();
        let mut chain_tip_change = self.chain_tip_change.clone();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        // The caller may provide the hashes of the chain tips it already has so the server only
        // streams non-finalized blocks after those tips. Malformed hashes (wrong length) are
        // rejected up front.
        let known_chain_tips = decode_known_chain_tips(request.into_inner().chain_tip_hashes)?;

        tokio::spawn(async move {
            let mut non_finalized_blocks = match read_state
                .oneshot(ReadRequest::NonFinalizedBlocksListener { known_chain_tips })
                .await
            {
                Ok(ReadResponse::NonFinalizedBlocksListener(listener)) => listener.unwrap(),
                Ok(_) => unreachable!("unexpected response type from ReadStateService"),
                Err(error) => {
                    span.in_scope(|| {
                        tracing::error!(?error, "failed to subscribe to chain state changes");
                    });

                    let _ = response_sender
                        .send(Err(Status::unavailable(
                            "failed to subscribe to chain state changes",
                        )))
                        .await;
                    return;
                }
            };

            // Interleave new non-finalized blocks with finalized-tip-change signals onto one stream.
            loop {
                tokio::select! {
                    // A new non-finalized block. Like the non-finalized stream, this applies
                    // backpressure (`send().await`) rather than dropping blocks for a slow consumer.
                    // A send error means the client disconnected; a timeout means it's hung.
                    block = non_finalized_blocks.recv() => {
                        let Some((hash, block)) = block else {
                            span.in_scope(|| {
                                tracing::warn!("non-finalized state change channel has closed");
                            });
                            let _ = response_sender
                                .send(Err(Status::unavailable(
                                    "non-finalized state change channel has closed",
                                )))
                                .await;
                            return;
                        };

                        let send = response_sender
                            .send(Ok(ChainStateChangeMessage::non_finalized_block(hash, block)));
                        match tokio::time::timeout(NON_FINALIZED_SEND_TIMEOUT, send).await {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) => {
                                span.in_scope(|| {
                                    tracing::info!(
                                        "client disconnected, dropping chain_state_change task"
                                    );
                                });
                                return;
                            }
                            Err(_) => {
                                span.in_scope(|| {
                                    tracing::warn!(
                                        "slow consumer, dropping chain_state_change stream after \
                                         send timed out"
                                    );
                                });
                                return;
                            }
                        }
                    }

                    // The primary's best chain advanced. We forward this as a finalized-tip-change
                    // signal: a co-located follower uses it to catch its own finalized state up and
                    // publish its finalized tip. Best-effort (`try_send`) because the signal is
                    // idempotent — only the latest matters, so dropping one when the buffer is full
                    // (of block messages) is harmless; the next change supersedes it.
                    tip_changed = chain_tip_change.best_tip_changed() => {
                        if tip_changed.is_err() {
                            span.in_scope(|| {
                                tracing::warn!("chain_tip_change channel has closed");
                            });
                            let _ = response_sender
                                .send(Err(Status::unavailable(
                                    "chain_tip_change channel has closed",
                                )))
                                .await;
                            return;
                        }

                        let Some((height, hash)) = chain_tip_change.best_tip_height_and_hash() else {
                            continue;
                        };

                        match response_sender
                            .try_send(Ok(ChainStateChangeMessage::finalized_tip_change(hash, height)))
                        {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                span.in_scope(|| {
                                    tracing::info!(
                                        "client disconnected, dropping chain_state_change task"
                                    );
                                });
                                return;
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                        }
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn chain_tip_change(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        let span = Span::current();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);
        let mut chain_tip_change = self.chain_tip_change.clone();

        tokio::spawn(async move {
            // Notify the client of chain tip changes until the channel is closed
            while let Ok(()) = chain_tip_change.best_tip_changed().await {
                let Some((tip_height, tip_hash)) = chain_tip_change.best_tip_height_and_hash()
                else {
                    continue;
                };

                match response_sender.try_send(Ok(BlockHashAndHeight::new(tip_hash, tip_height))) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping chain_tip_change task");
                        });
                        return;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                        span.in_scope(|| {
                            tracing::warn!("slow consumer, dropping chain_tip_change stream");
                        });
                        return;
                    }
                }
            }

            span.in_scope(|| {
                tracing::warn!("chain_tip_change channel has closed");
            });

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "chain_tip_change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn non_finalized_state_change(
        &self,
        request: tonic::Request<NonFinalizedStateChangeRequest>,
    ) -> Result<Response<Self::NonFinalizedStateChangeStream>, Status> {
        let span = Span::current();
        let read_state = self.read_state.clone();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        // The caller may provide the hashes of the chain tips it already has so the server only
        // streams blocks after those tips. Malformed hashes (wrong length) are rejected up front.
        let known_chain_tips = decode_known_chain_tips(request.into_inner().chain_tip_hashes)?;

        tokio::spawn(async move {
            let mut non_finalized_state_change = match read_state
                .oneshot(ReadRequest::NonFinalizedBlocksListener { known_chain_tips })
                .await
            {
                Ok(ReadResponse::NonFinalizedBlocksListener(listener)) => listener.unwrap(),
                Ok(_) => unreachable!("unexpected response type from ReadStateService"),
                Err(error) => {
                    span.in_scope(|| {
                        tracing::error!(
                            ?error,
                            "failed to subscribe to non-finalized state changes"
                        );
                    });

                    let _ = response_sender
                        .send(Err(Status::unavailable(
                            "failed to subscribe to non-finalized state changes",
                        )))
                        .await;
                    return;
                }
            };

            // Notify the client of new blocks until the channel is closed.
            //
            // Unlike the other streams, this uses `send().await` to apply backpressure to the
            // non-finalized state listener rather than dropping blocks for a slow consumer. A send
            // error means the client disconnected; a send that doesn't complete within
            // `NON_FINALIZED_SEND_TIMEOUT` means the consumer is hung. In both cases the task ends
            // rather than blocking forever.
            while let Some((hash, block)) = non_finalized_state_change.recv().await {
                let send = response_sender.send(Ok(BlockAndHash::new(hash, block)));
                match tokio::time::timeout(NON_FINALIZED_SEND_TIMEOUT, send).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        span.in_scope(|| {
                            tracing::info!(
                                "client disconnected, dropping non_finalized_state_change task"
                            );
                        });
                        return;
                    }
                    Err(_) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                "slow consumer, dropping non_finalized_state_change stream after \
                                 send timed out"
                            );
                        });
                        return;
                    }
                }
            }

            span.in_scope(|| {
                tracing::warn!("non-finalized state change channel has closed");
            });

            let _ = response_sender
                .send(Err(Status::unavailable(
                    "non-finalized state change channel has closed",
                )))
                .await;
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn sync_mempool(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<Self::SyncMempoolStream>, Status> {
        let span = Span::current();
        let mempool = self.mempool.clone();
        // Subscribe to the live broadcast *before* reading the bootstrap snapshot (design §10), so
        // no change is lost in the gap between the read and the subscription. Events that land
        // during the bootstrap burst are applied idempotently over it by the follower.
        let mut mempool_change = self.mempool_change.subscribe();
        let (response_sender, response_receiver) = tokio::sync::mpsc::channel(RESPONSE_BUFFER_SIZE);
        let response_stream = ReceiverStream::new(response_receiver);

        tokio::spawn(async move {
            // (1) Read a consistent point-in-time bootstrap snapshot from the local mempool tower
            // service (in-process, microseconds — design §6). The mempool handles one request at a
            // time, so the queued and verified sets observe a single coherent state.
            let (queued, verified, rejected) = match mempool
                .clone()
                .oneshot(mempool::Request::MempoolBootstrapState)
                .await
            {
                Ok(mempool::Response::MempoolBootstrapState {
                    queued,
                    verified,
                    rejected,
                }) => (queued, verified, rejected),
                Ok(_) => unreachable!("unexpected response type from mempool service"),
                Err(error) => {
                    span.in_scope(|| {
                        tracing::error!(?error, "failed to read mempool bootstrap state");
                    });
                    let _ = response_sender
                        .send(Err(Status::unavailable(
                            "failed to read mempool bootstrap state",
                        )))
                        .await;
                    return;
                }
            };

            // The bootstrap checksum is over the full replica projection: the verified set plus the
            // queued set (design §3a-1). The rejection cache is a transient observation (design §3b)
            // and is *not* part of the projection, so it is excluded from the checksum.
            let verified_ids: HashSet<UnminedTxId> =
                verified.iter().map(|tx| tx.transaction.id).collect();
            let checksum = replica_digest(&verified_ids, &queued);

            // (2) Replay the snapshot as ordinary lifecycle events (design §3a "bootstrap is still
            // incremental"): queued txs as `Queued{stage}`, verified txs as `Added{content}`, and
            // recent rejections as `Removed{FailedVerification}` (design §3b partial recovery).
            let mut events = Vec::new();
            // Group the queued set by stage into at most two `Queued` events (one per stage).
            let mut awaiting_download = Vec::new();
            let mut awaiting_verification = Vec::new();
            for (id, stage) in &queued {
                match stage {
                    QueuedStage::AwaitingDownload => awaiting_download.push(wire_txid(id)),
                    QueuedStage::AwaitingVerification => awaiting_verification.push(wire_txid(id)),
                }
            }
            if !awaiting_download.is_empty() {
                events.push(queued_event(
                    awaiting_download,
                    QueuedStage::AwaitingDownload,
                ));
            }
            if !awaiting_verification.is_empty() {
                events.push(queued_event(
                    awaiting_verification,
                    QueuedStage::AwaitingVerification,
                ));
            }
            for tx in &verified {
                match wire_added(tx) {
                    Ok(added) => events.push(added_event(added)),
                    Err(error) => {
                        span.in_scope(|| {
                            tracing::error!(?error, "failed to serialize bootstrap transaction");
                        });
                        let _ = response_sender
                            .send(Err(Status::internal(
                                "failed to serialize bootstrap transaction",
                            )))
                            .await;
                        return;
                    }
                }
            }
            for (id, code) in &rejected {
                events.push(removed_event(MempoolRemoved {
                    tx_ids: vec![wire_txid(id)],
                    reason: Some(mempool_removed::Reason::FailedVerification(code.clone())),
                }));
            }

            // (3) Send the bootstrap events as one or more batches chunked under the gRPC message
            // limit. Only the final batch carries `initial_sync_complete=true` and the checksum
            // (the k8s SendInitialEvents bookmark, design §5).
            if send_bootstrap_batches(&response_sender, &span, events, checksum)
                .await
                .is_err()
            {
                return;
            }

            // (4) Forward one live `MempoolBatch` per source change cycle, each carrying its events
            // (including the `reorged` removal marker, design §5a) and post-cycle checksum.
            loop {
                let batch = match mempool_change.recv().await {
                    Ok(batch) => batch,
                    // The server's own broadcast receiver lagged: it missed events, so its view is
                    // no longer authoritative. Drop the connection so the follower re-bootstraps
                    // (design §5 "drop-and-resync").
                    Err(RecvError::Lagged(skipped)) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                skipped,
                                "mempool broadcast lagged, dropping sync_mempool stream"
                            );
                        });
                        return;
                    }
                    Err(RecvError::Closed) => {
                        span.in_scope(|| {
                            tracing::warn!("mempool change channel has closed");
                        });
                        let _ = response_sender
                            .send(Err(Status::unavailable(
                                "mempool change channel has closed",
                            )))
                            .await;
                        return;
                    }
                };

                let wire_batch = match live_batch_to_wire(&mempool, &batch).await {
                    Ok(wire_batch) => wire_batch,
                    Err(error) => {
                        span.in_scope(|| {
                            tracing::error!(?error, "failed to build live mempool batch");
                        });
                        let _ = response_sender
                            .send(Err(Status::internal("failed to build live mempool batch")))
                            .await;
                        return;
                    }
                };

                let send = response_sender.send(Ok(wire_batch));
                match tokio::time::timeout(SYNC_MEMPOOL_SEND_TIMEOUT, send).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        span.in_scope(|| {
                            tracing::info!("client disconnected, dropping sync_mempool task");
                        });
                        return;
                    }
                    Err(_) => {
                        span.in_scope(|| {
                            tracing::warn!(
                                "slow consumer, dropping sync_mempool stream after send timed out"
                            );
                        });
                        return;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(response_stream)))
    }

    async fn get_block(
        &self,
        request: tonic::Request<BlockRequest>,
    ) -> Result<Response<BlockAndHash>, Status> {
        // The request carries a single `hash_or_height` byte string: a 32-byte block hash in
        // display order, or a 4-byte big-endian block height. The length tells the two apart.
        let hash_or_height = request.into_inner().hash_or_height;
        let hash_or_height = match hash_or_height.len() {
            32 => zebra_state::HashOrHeight::Hash(hash_from_display_bytes(hash_or_height)?),
            4 => {
                let height = u32::from_be_bytes(
                    hash_or_height
                        .try_into()
                        .expect("a 4-byte vec always converts to a [u8; 4]"),
                );
                let height = block::Height::try_from(height).map_err(|_| {
                    Status::invalid_argument(format!("block height out of range: {height}"))
                })?;
                zebra_state::HashOrHeight::Height(height)
            }
            len => {
                return Err(Status::invalid_argument(format!(
                    "block request must be a 32-byte hash or a 4-byte height, got {len} bytes"
                )));
            }
        };

        match self
            .read_state
            .clone()
            .oneshot(ReadRequest::Block(hash_or_height))
            .await
        {
            Ok(ReadResponse::Block(Some(block))) => {
                Ok(Response::new(BlockAndHash::new(block.hash(), block)))
            }
            Ok(ReadResponse::Block(None)) => Err(Status::not_found("block not found")),
            Ok(_) => unreachable!("unexpected response type from ReadStateService"),
            Err(error) => Err(Status::unavailable(format!(
                "failed to read block: {error}"
            ))),
        }
    }

    async fn get_state_info(
        &self,
        _: tonic::Request<Empty>,
    ) -> Result<Response<StateInfo>, Status> {
        let info = self.read_state.state_info();

        Ok(Response::new(StateInfo {
            // `to_string_lossy` losslessly encodes UTF-8 paths (cache dirs are UTF-8 in practice);
            // a non-UTF-8 path would be lossily encoded, which is acceptable and documented.
            db_path: info.db_path.to_string_lossy().into_owned(),
            db_format_version: info.db_format_version.to_string(),
            network: info.network.to_string(),
        }))
    }
}

/// Converts an [`UnminedTxId`] to its wire form (mined id + optional auth digest, in display order).
fn wire_txid(id: &UnminedTxId) -> ProtoUnminedTxId {
    ProtoUnminedTxId {
        mined_id: id.mined_id().bytes_in_display_order().to_vec(),
        auth_digest: id
            .auth_digest()
            .map(|d| d.bytes_in_display_order().to_vec())
            .unwrap_or_default(),
    }
}

/// Maps a mempool [`QueuedStage`] to its wire enum.
fn wire_queued_stage(stage: QueuedStage) -> ProtoQueuedStage {
    match stage {
        QueuedStage::AwaitingDownload => ProtoQueuedStage::AwaitingDownload,
        QueuedStage::AwaitingVerification => ProtoQueuedStage::AwaitingVerification,
    }
}

/// Builds a `Queued` lifecycle event for a set of transactions at one stage.
fn queued_event(tx_ids: Vec<ProtoUnminedTxId>, stage: QueuedStage) -> MempoolEvent {
    MempoolEvent {
        event: Some(mempool_event::Event::Queued(MempoolQueued {
            tx_ids,
            // The wire enum is encoded as its `i32` discriminant.
            stage: wire_queued_stage(stage) as i32,
        })),
    }
}

/// Wraps a [`MempoolAdded`] payload in an `Added` lifecycle event.
fn added_event(added: MempoolAdded) -> MempoolEvent {
    MempoolEvent {
        event: Some(mempool_event::Event::Added(added)),
    }
}

/// Wraps a [`MempoolRemoved`] payload in a `Removed` lifecycle event.
fn removed_event(removed: MempoolRemoved) -> MempoolEvent {
    MempoolEvent {
        event: Some(mempool_event::Event::Removed(removed)),
    }
}

/// Builds an `Added` payload from a verified mempool transaction, serializing its content so the
/// follower can reconstruct it without a network round-trip (design §6).
fn wire_added(tx: &VerifiedUnminedTx) -> Result<MempoolAdded, BoxError> {
    Ok(MempoolAdded {
        transaction: tx.transaction.transaction.zcash_serialize_to_vec()?,
        // The miner fee is `NonNegative`, so the `i64` → `u64` cast never wraps.
        miner_fee: tx.miner_fee.zatoshis() as u64,
        legacy_sigop_count: tx.legacy_sigop_count,
        p2sh_sigop_count: tx.p2sh_sigop_count,
        conventional_actions: tx.conventional_actions,
        unpaid_actions: tx.unpaid_actions,
        fee_weight_ratio: tx.fee_weight_ratio,
    })
}

/// Maps a [`RemovedReason`] to its wire `Reason`.
fn wire_removed_reason(reason: &RemovedReason) -> mempool_removed::Reason {
    match reason {
        RemovedReason::FailedDownload => mempool_removed::Reason::FailedDownload(Empty {}),
        RemovedReason::FailedVerification(code) => {
            mempool_removed::Reason::FailedVerification(code.clone())
        }
        RemovedReason::Mined { block_hash, height } => {
            mempool_removed::Reason::Mined(BlockHashAndHeight::new(*block_hash, *height))
        }
        RemovedReason::Expired => mempool_removed::Reason::Expired(Empty {}),
        RemovedReason::Evicted => mempool_removed::Reason::Evicted(Empty {}),
        RemovedReason::Reorged => mempool_removed::Reason::Reorged(Empty {}),
    }
}

/// Sends the bootstrap `events` to a follower as one or more [`MempoolBatch`]es, each kept under
/// [`BOOTSTRAP_MAX_BATCH_BYTES`] (design §5).
///
/// Only the final batch carries `initial_sync_complete=true` and the `checksum` (the k8s
/// SendInitialEvents bookmark). An empty mempool still sends one terminal batch with no events.
/// Returns `Err(())` if the follower disconnected or stalled past [`SYNC_MEMPOOL_SEND_TIMEOUT`].
async fn send_bootstrap_batches(
    response_sender: &tokio::sync::mpsc::Sender<Result<MempoolBatch, Status>>,
    span: &Span,
    events: Vec<MempoolEvent>,
    checksum: [u8; REPLICA_DIGEST_LEN],
) -> Result<(), ()> {
    // Chunk the events so each batch's encoded size stays under the limit. An event larger on its
    // own than the limit still goes out in its own batch — a single transaction can't exceed gRPC's
    // frame limit, so this never produces an over-limit frame.
    let mut chunks: Vec<Vec<MempoolEvent>> = Vec::new();
    let mut current: Vec<MempoolEvent> = Vec::new();
    let mut current_bytes = 0usize;
    for event in events {
        let event_bytes = event.encoded_len();
        if !current.is_empty() && current_bytes + event_bytes > BOOTSTRAP_MAX_BATCH_BYTES {
            chunks.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
        current_bytes += event_bytes;
        current.push(event);
    }
    // Always push the final chunk (even if empty) so the terminal bootstrap batch is sent.
    chunks.push(current);

    let last_index = chunks.len() - 1;
    for (index, chunk) in chunks.into_iter().enumerate() {
        let is_last = index == last_index;
        let batch = MempoolBatch {
            events: chunk,
            checksum: is_last.then(|| checksum.to_vec()),
            initial_sync_complete: is_last,
        };

        let send = response_sender.send(Ok(batch));
        match tokio::time::timeout(SYNC_MEMPOOL_SEND_TIMEOUT, send).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                span.in_scope(|| {
                    tracing::info!("client disconnected during sync_mempool bootstrap");
                });
                return Err(());
            }
            Err(_) => {
                span.in_scope(|| {
                    tracing::warn!(
                        "slow consumer, dropping sync_mempool bootstrap after send timed out"
                    );
                });
                return Err(());
            }
        }
    }

    Ok(())
}

/// Converts a live [`mempool::MempoolBatch`] to its wire form, fetching the content of any added
/// transactions from the local mempool service in-process (design §6).
///
/// The batch's source-computed post-cycle checksum is carried through unchanged.
async fn live_batch_to_wire<Mempool>(
    mempool: &Mempool,
    batch: &mempool::MempoolBatch,
) -> Result<MempoolBatch, BoxError>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    Mempool::Future: Send,
{
    // The broadcast carries only ids for `Added` changes; fetch the content of every added tx in
    // this cycle in a single in-process call.
    let mut added_ids: HashSet<UnminedTxId> = HashSet::new();
    for change in batch.events() {
        if matches!(change.kind(), MempoolChangeKind::Added) {
            added_ids.extend(change.tx_ids().iter().copied());
        }
    }
    let content = if added_ids.is_empty() {
        HashMap::new()
    } else {
        fetch_added_content(mempool, added_ids).await?
    };

    let mut events = Vec::new();
    for change in batch.events() {
        match change.kind() {
            MempoolChangeKind::Queued(stage) => {
                let tx_ids = change.tx_ids().iter().map(wire_txid).collect();
                events.push(queued_event(tx_ids, *stage));
            }
            MempoolChangeKind::Added => {
                for id in change.tx_ids() {
                    // A tx added then removed within the same cycle may already be gone from the
                    // mempool; the follower's checksum compare catches any divergence and
                    // re-bootstraps.
                    if let Some(tx) = content.get(id) {
                        events.push(added_event(wire_added(tx)?));
                    }
                }
            }
            MempoolChangeKind::Removed(reason) => {
                let tx_ids = change.tx_ids().iter().map(wire_txid).collect();
                events.push(removed_event(MempoolRemoved {
                    tx_ids,
                    reason: Some(wire_removed_reason(reason)),
                }));
            }
        }
    }

    Ok(MempoolBatch {
        events,
        checksum: batch.checksum().map(|c| c.to_vec()),
        initial_sync_complete: false,
    })
}

/// Fetches the verified content of the given transaction ids from the local mempool service, keyed
/// by [`UnminedTxId`]. Uses a single `FullTransactions` call and filters to the requested ids.
async fn fetch_added_content<Mempool>(
    mempool: &Mempool,
    ids: HashSet<UnminedTxId>,
) -> Result<HashMap<UnminedTxId, VerifiedUnminedTx>, BoxError>
where
    Mempool: Service<mempool::Request, Response = mempool::Response, Error = BoxError>
        + Clone
        + Send
        + 'static,
    Mempool::Future: Send,
{
    match mempool
        .clone()
        .oneshot(mempool::Request::FullTransactions)
        .await?
    {
        mempool::Response::FullTransactions { transactions, .. } => Ok(transactions
            .into_iter()
            .filter(|tx| ids.contains(&tx.transaction.id))
            .map(|tx| (tx.transaction.id, tx))
            .collect()),
        _ => Err("unexpected response type from mempool service".into()),
    }
}

/// Decodes the chain tip hashes from a [`NonFinalizedStateChangeRequest`] into a set of
/// [`block::Hash`]es.
///
/// Each hash is expected to be 32 bytes in display order, matching the encoding used when the
/// server streams [`BlockAndHash`] messages back to the caller.
///
/// # Errors
///
/// Returns an [`invalid_argument`](Status::invalid_argument) status if there are more hashes than
/// the non-finalized state can hold chains ([`MAX_NON_FINALIZED_CHAIN_FORKS`]), or if any hash is
/// not exactly 32 bytes long.
fn decode_known_chain_tips(chain_tip_hashes: Vec<Vec<u8>>) -> Result<HashSet<block::Hash>, Status> {
    // The non-finalized state holds at most `MAX_NON_FINALIZED_CHAIN_FORKS` chains, so a caller can
    // never legitimately have more chain tips than that. Bound the untrusted input up front rather
    // than allocating a set sized by the request.
    if chain_tip_hashes.len() > MAX_NON_FINALIZED_CHAIN_FORKS {
        return Err(Status::invalid_argument(format!(
            "too many chain tip hashes: got {}, expected at most {MAX_NON_FINALIZED_CHAIN_FORKS}",
            chain_tip_hashes.len(),
        )));
    }

    chain_tip_hashes
        .into_iter()
        .map(hash_from_display_bytes)
        .collect()
}

/// Decodes a 32-byte block hash in display order, rejecting wrong-length input.
fn hash_from_display_bytes(hash: Vec<u8>) -> Result<block::Hash, Status> {
    let bytes: [u8; 32] = hash.try_into().map_err(|hash: Vec<u8>| {
        Status::invalid_argument(format!(
            "invalid block hash length: expected 32 bytes, got {}",
            hash.len()
        ))
    })?;

    Ok(block::Hash::from_bytes_in_display_order(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::Code;

    fn hash(byte: u8) -> block::Hash {
        block::Hash::from_bytes_in_display_order(&[byte; 32])
    }

    #[test]
    fn decode_known_chain_tips_round_trips_display_order() {
        let hashes = [hash(1), hash(2), hash(3)];
        let encoded = hashes
            .iter()
            .map(|h| h.bytes_in_display_order().to_vec())
            .collect();

        let decoded = decode_known_chain_tips(encoded).expect("valid hashes should decode");

        assert_eq!(decoded, hashes.into_iter().collect());
    }

    #[test]
    fn decode_known_chain_tips_accepts_empty() {
        assert!(decode_known_chain_tips(Vec::new())
            .expect("empty input should decode")
            .is_empty());
    }

    #[test]
    fn decode_known_chain_tips_dedups() {
        let encoded = vec![
            hash(7).bytes_in_display_order().to_vec(),
            hash(7).bytes_in_display_order().to_vec(),
        ];

        let decoded = decode_known_chain_tips(encoded).expect("duplicate hashes should decode");

        assert_eq!(decoded, std::iter::once(hash(7)).collect());
    }

    #[test]
    fn decode_known_chain_tips_rejects_wrong_length() {
        let status = decode_known_chain_tips(vec![vec![0; 31]])
            .expect_err("a 31-byte hash should be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
    }

    #[test]
    fn decode_known_chain_tips_rejects_too_many() {
        let encoded = (0..=MAX_NON_FINALIZED_CHAIN_FORKS as u8)
            .map(|b| hash(b).bytes_in_display_order().to_vec())
            .collect();

        let status = decode_known_chain_tips(encoded)
            .expect_err("more than MAX_NON_FINALIZED_CHAIN_FORKS hashes should be rejected");

        assert_eq!(status.code(), Code::InvalidArgument);
    }
}
