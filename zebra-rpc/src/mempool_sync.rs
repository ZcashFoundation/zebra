//! Mempool follower: maintains a live, in-process replica of a primary Zebra
//! node's mempool — content and transaction-lifecycle observability — by syncing
//! a co-located trusted node's `SyncMempool` indexer gRPC (design
//! [`trusted_mempool_sync.md`](../../docs/design/trusted_mempool_sync.md)).
//!
//! [`TrustedMempoolSync`] is the mempool analogue of
//! [`TrustedChainSync`](crate::sync::TrustedChainSync), built for Zaino. It
//! bootstraps a [`MempoolReplica`] from the source's current state, then applies
//! one [`MempoolBatch`](zebra_node_services::mempool::MempoolBatch) per source
//! change cycle idempotently and lifecycle-monotonically (design §10), verifying
//! each batch's checksum against its own recomputed projection digest (design
//! §3a). A mismatch, stream break, or timeout triggers a backoff and reconnect,
//! which re-bootstraps from scratch.
//!
//! It publishes two first-class outputs for the consumer (design §6):
//! - a [`watch`] of the queryable verified + queued sets, and
//! - a lifecycle [`MempoolObservation`] feed (the events).

mod replica;

#[cfg(test)]
mod tests;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
};
use tonic::transport::{Channel, Endpoint};
use tower::BoxError;
use zebra_chain::{
    serialization::{BytesInDisplayOrder, ZcashDeserializeInto},
    transaction::{self, AuthDigest, Transaction, UnminedTx, UnminedTxId, WtxId},
};
use zebra_node_services::mempool::{QueuedStage, RemovedReason, REPLICA_DIGEST_LEN};

use crate::indexer::{
    indexer_client::IndexerClient, mempool_event, mempool_removed, Empty, MempoolAdded,
    MempoolEvent, MempoolQueued, MempoolRemoved, QueuedStage as ProtoQueuedStage,
    UnminedTxId as ProtoUnminedTxId,
};

pub use replica::{MempoolObservation, MempoolReplica, VerifiedReplicaTx};

/// HTTP/2 keep-alive ping interval for the indexer gRPC connection, so a half-open connection is
/// detected promptly. Mirrors the chain syncer's keep-alive (design §5). Keep-alive detects a dead
/// transport but not an application-level wedge (a stalled `sync_mempool` task on a connection that
/// still answers pings); [`STREAM_MESSAGE_TIMEOUT`] is the backstop for that case.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);

/// How long to wait for a keep-alive ping response before treating the connection as dead.
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for a message on the `SyncMempool` stream before treating the connection as
/// wedged and reconnecting.
///
/// Generous, because the mempool can be quiet for several minutes between change cycles; this is a
/// backstop against an application-level wedge that the keep-alive ping doesn't catch (e.g. a
/// stalled server-side `sync_mempool` task on a connection that still answers pings). On `Elapsed`
/// the session ends, so the reconnect + backoff + `Gap` path repairs the stale replica instead of
/// hanging. Mirrors the chain syncer's `STREAM_MESSAGE_TIMEOUT` (sync/stream.rs).
const STREAM_MESSAGE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long to wait to establish the `SyncMempool` subscription stream before assuming the request
/// is wedged and reconnecting. The subscription handshake should complete promptly.
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The capacity of the [`MempoolObservation`] broadcast feed. A consumer that lags past this
/// receives a [`broadcast::error::RecvError::Lagged`], the transient-observation analogue of the
/// stream gap (design §3b).
const OBSERVATION_CHANNEL_CAPACITY: usize = 1024;

/// The initial reconnect backoff, doubled after each failed session up to [`MAX_BACKOFF`].
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// The maximum reconnect backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Syncs a primary Zebra node's mempool over its `SyncMempool` indexer gRPC, maintaining a local
/// [`MempoolReplica`] and a lifecycle observation feed (design §6).
#[derive(Debug)]
pub struct TrustedMempoolSync {
    /// The address of the trusted node's indexer gRPC.
    indexer_rpc_address: SocketAddr,
    /// Publishes the current replica (verified + queued sets) to consumers.
    replica_sender: watch::Sender<Arc<MempoolReplica>>,
    /// Publishes lifecycle transitions and gap markers to consumers (design §3b).
    observation_sender: broadcast::Sender<MempoolObservation>,
}

impl TrustedMempoolSync {
    /// Spawns a task that syncs the mempool from the trusted node's indexer gRPC at
    /// `indexer_rpc_address`, reconnecting with backoff on any session failure.
    ///
    /// Returns a [`watch::Receiver`] of the current [`MempoolReplica`], a
    /// [`broadcast::Receiver`] of the [`MempoolObservation`] feed, and the task's [`JoinHandle`].
    /// The watch starts with an empty replica until the first bootstrap completes.
    pub fn spawn(
        indexer_rpc_address: SocketAddr,
    ) -> (
        watch::Receiver<Arc<MempoolReplica>>,
        broadcast::Receiver<MempoolObservation>,
        JoinHandle<()>,
    ) {
        let (replica_sender, replica_receiver) =
            watch::channel(Arc::new(MempoolReplica::default()));
        let (observation_sender, observation_receiver) =
            broadcast::channel(OBSERVATION_CHANNEL_CAPACITY);

        let syncer = Self {
            indexer_rpc_address,
            replica_sender,
            observation_sender,
        };

        let task = tokio::spawn(async move {
            syncer.run().await;
        });

        (replica_receiver, observation_receiver, task)
    }

    /// Runs the sync loop forever: a session per connection, reconnecting with exponential backoff
    /// and marking a transient-observation gap (design §3b) on each reconnect.
    #[tracing::instrument(skip_all)]
    async fn run(self) {
        let mut backoff = INITIAL_BACKOFF;
        let mut is_reconnect = false;

        loop {
            // On every (re)connect after the first, the transient observation layer may have a hole:
            // events lost during the gap are unrecoverable, so we say so (design §3b). The verified
            // and queued sets are repaired by the re-bootstrap that follows.
            if is_reconnect {
                let _ = self.observation_sender.send(MempoolObservation::Gap);
            }

            if let Err(error) = self.run_session(&mut backoff).await {
                tracing::warn!(?error, "mempool sync session ended, will reconnect");
            }

            is_reconnect = true;
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Connects, bootstraps a fresh replica, then applies live batches until the session fails.
    ///
    /// Resets `backoff` once the bootstrap completes, so a long-lived session that later drops
    /// reconnects promptly. Returns an error on any connection, stream, decode, or checksum failure;
    /// the caller backs off and reconnects, which re-bootstraps from scratch.
    async fn run_session(&self, backoff: &mut Duration) -> Result<(), BoxError> {
        let mut client = Self::connect(self.indexer_rpc_address).await?;
        let mut stream = tokio::time::timeout(SUBSCRIBE_TIMEOUT, client.sync_mempool(Empty {}))
            .await
            .map_err(|_| "timed out subscribing to the mempool sync stream")??
            .into_inner();

        // Each session re-bootstraps from scratch, so any divergent state from a previous session is
        // discarded rather than repaired in place (design §7 "full re-bootstrap on divergence").
        let mut replica = MempoolReplica::default();

        // (1) Bootstrap phase: apply replayed current-state events until the `initial_sync_complete`
        // bookmark, then verify the bootstrap checksum and publish (design §3a, §5).
        loop {
            let batch = tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, stream.message())
                .await
                .map_err(|_| "timed out waiting for a mempool stream message during bootstrap")??
                .ok_or("mempool stream closed during bootstrap")?;

            for event in batch.events {
                // Bootstrap events replay current state, not live transitions, so they are not
                // published on the observation feed; the watch replica conveys the snapshot.
                apply_event(&mut replica, event, &mut Vec::new())?;
            }

            if batch.initial_sync_complete {
                verify_checksum(&replica, batch.checksum.as_deref())?;
                self.publish_replica(&replica);
                *backoff = INITIAL_BACKOFF;
                break;
            }
        }

        // (2) Live phase: apply one batch per source change cycle, verify its checksum, then publish
        // the transitions and the updated replica (design §3a, §6).
        loop {
            let batch = tokio::time::timeout(STREAM_MESSAGE_TIMEOUT, stream.message())
                .await
                .map_err(|_| "timed out waiting for a mempool stream message")??
                .ok_or("mempool stream closed")?;

            let mut observations = Vec::new();
            for event in batch.events {
                apply_event(&mut replica, event, &mut observations)?;
            }

            // A live batch carries its post-cycle checksum; a missing one is a mid-cycle batch that
            // is not at a settled point, so it is not verified (design §3a-1).
            if let Some(checksum) = batch.checksum.as_deref() {
                verify_checksum(&replica, Some(checksum))?;
            }

            for observation in observations {
                let _ = self.observation_sender.send(observation);
            }
            self.publish_replica(&replica);
        }
    }

    /// Publishes a snapshot of the current replica to the watch channel.
    ///
    /// This clones the replica once per batch (never per event, design §6) into the published
    /// [`Arc`]; the set is small, so a per-batch clone is cheap.
    fn publish_replica(&self, replica: &MempoolReplica) {
        let _ = self.replica_sender.send(Arc::new(replica.clone()));
    }

    /// Connects an [`IndexerClient`] to the trusted node's indexer gRPC, with HTTP/2 keep-alive and
    /// an adaptive flow-control window for the bursty bootstrap (design §5).
    async fn connect(indexer_rpc_address: SocketAddr) -> Result<IndexerClient<Channel>, BoxError> {
        let channel = Endpoint::from_shared(format!("http://{indexer_rpc_address}"))?
            .keep_alive_while_idle(true)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .http2_adaptive_window(true)
            .connect()
            .await?;

        Ok(IndexerClient::new(channel))
    }
}

/// Verifies that the replica's recomputed projection digest matches the batch's `checksum`.
///
/// Both sides compute the digest with the same shared `replica_digest` function over the same
/// projection (design §3a-1), so in steady state they agree bit-for-bit. A mismatch is the
/// divergence detector and returns an error so the caller re-bootstraps (design §3a).
fn verify_checksum(replica: &MempoolReplica, checksum: Option<&[u8]>) -> Result<(), BoxError> {
    let checksum = checksum.ok_or("settled batch is missing its checksum")?;
    let expected: [u8; REPLICA_DIGEST_LEN] = checksum
        .try_into()
        .map_err(|_| format!("checksum has wrong length: {}", checksum.len()))?;

    if replica.digest() != expected {
        return Err("replica digest does not match batch checksum".into());
    }

    Ok(())
}

/// Applies one wire [`MempoolEvent`] to `replica`, pushing any resulting transition onto
/// `observations`. Decoding failures (malformed ids, undecodable content) end the session so it
/// re-bootstraps.
fn apply_event(
    replica: &mut MempoolReplica,
    event: MempoolEvent,
    observations: &mut Vec<MempoolObservation>,
) -> Result<(), BoxError> {
    let Some(event) = event.event else {
        // An event with no payload (a future or empty variant) is ignored.
        return Ok(());
    };

    match event {
        mempool_event::Event::Queued(MempoolQueued { tx_ids, stage }) => {
            let stage = decode_stage(stage)?;
            for id in tx_ids {
                let id = decode_txid(id)?;
                if let Some(observation) = replica.apply_queued(id, stage) {
                    observations.push(observation);
                }
            }
        }
        mempool_event::Event::Added(added) => {
            let (id, tx) = decode_added(added)?;
            if let Some(observation) = replica.apply_added(id, tx) {
                observations.push(observation);
            }
        }
        mempool_event::Event::Removed(MempoolRemoved { tx_ids, reason }) => {
            let reason = decode_removed_reason(reason)?;
            for id in tx_ids {
                let id = decode_txid(id)?;
                if let Some(observation) = replica.apply_removed(id, reason.clone()) {
                    observations.push(observation);
                }
            }
        }
    }

    Ok(())
}

/// Decodes a wire [`ProtoQueuedStage`] discriminant into a [`QueuedStage`].
fn decode_stage(stage: i32) -> Result<QueuedStage, BoxError> {
    match ProtoQueuedStage::try_from(stage) {
        Ok(ProtoQueuedStage::AwaitingDownload) => Ok(QueuedStage::AwaitingDownload),
        Ok(ProtoQueuedStage::AwaitingVerification) => Ok(QueuedStage::AwaitingVerification),
        Err(_) => Err(format!("unknown queued stage discriminant: {stage}").into()),
    }
}

/// Decodes a wire [`ProtoUnminedTxId`] (mined id + optional auth digest, in display order) into a
/// full [`UnminedTxId`] (design §9.1). An empty auth digest is a legacy (v1-v4) transaction.
fn decode_txid(id: ProtoUnminedTxId) -> Result<UnminedTxId, BoxError> {
    let mined_id = transaction::Hash::from_bytes_in_display_order(&decode_bytes32(id.mined_id)?);

    if id.auth_digest.is_empty() {
        Ok(UnminedTxId::Legacy(mined_id))
    } else {
        let auth_digest = AuthDigest::from_bytes_in_display_order(&decode_bytes32(id.auth_digest)?);
        Ok(UnminedTxId::Witnessed(WtxId {
            id: mined_id,
            auth_digest,
        }))
    }
}

/// Decodes a wire [`MempoolAdded`] into its [`UnminedTxId`] and [`VerifiedReplicaTx`],
/// reconstructing the unmined transaction (and its id) from the streamed content (design §6).
fn decode_added(added: MempoolAdded) -> Result<(UnminedTxId, VerifiedReplicaTx), BoxError> {
    let transaction: Transaction = added.transaction.zcash_deserialize_into()?;
    let transaction = UnminedTx::from(transaction);
    let id = transaction.id;

    Ok((
        id,
        VerifiedReplicaTx {
            transaction,
            miner_fee: added.miner_fee,
            legacy_sigop_count: added.legacy_sigop_count,
            p2sh_sigop_count: added.p2sh_sigop_count,
            conventional_actions: added.conventional_actions,
            unpaid_actions: added.unpaid_actions,
            fee_weight_ratio: added.fee_weight_ratio,
        },
    ))
}

/// Decodes a wire [`mempool_removed::Reason`] into a [`RemovedReason`].
fn decode_removed_reason(
    reason: Option<mempool_removed::Reason>,
) -> Result<RemovedReason, BoxError> {
    let reason = reason.ok_or("removed event is missing its reason")?;

    Ok(match reason {
        mempool_removed::Reason::FailedDownload(_) => RemovedReason::FailedDownload,
        mempool_removed::Reason::FailedVerification(code) => {
            RemovedReason::FailedVerification(code)
        }
        mempool_removed::Reason::Mined(block) => {
            let (block_hash, height) = block
                .try_into_hash_and_height()
                .ok_or("mined removal has an invalid block hash or height")?;
            RemovedReason::Mined { block_hash, height }
        }
        mempool_removed::Reason::Expired(_) => RemovedReason::Expired,
        mempool_removed::Reason::Evicted(_) => RemovedReason::Evicted,
        mempool_removed::Reason::Reorged(_) => RemovedReason::Reorged,
    })
}

/// Decodes a 32-byte field, rejecting wrong-length input.
fn decode_bytes32(bytes: Vec<u8>) -> Result<[u8; 32], BoxError> {
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected 32 bytes, got {}", bytes.len()).into())
}
