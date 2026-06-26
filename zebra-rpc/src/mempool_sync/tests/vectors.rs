//! Fixed test vectors for the [`TrustedMempoolSync`] follower.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};

use tokio::{
    sync::{broadcast, watch},
    task::JoinHandle,
    time::timeout,
};
use tower::BoxError;
use zebra_chain::{
    block::Block,
    chain_tip::mock::{MockChainTip, MockChainTipSender},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, UnminedTx, UnminedTxId},
};
use zebra_node_services::mempool::{
    self, replica_digest, MempoolBatch, MempoolChange, MempoolTxSubscriber, QueuedStage,
    RemovedReason, REPLICA_DIGEST_LEN,
};
use zebra_state::{ReadRequest, ReadResponse};
use zebra_test::mock_service::{MockService, PanicAssertion};

use crate::indexer::{
    self, mempool_event, mempool_removed, Empty, MempoolAdded, MempoolEvent, MempoolQueued,
    MempoolRemoved, QueuedStage as ProtoQueuedStage, UnminedTxId as ProtoUnminedTxId,
};

use super::super::{
    apply_event, MempoolObservation, MempoolReplica, TrustedMempoolSync, VerifiedReplicaTx,
};

/// A mock mempool tower service, matching the one driving the indexer `SyncMempool` handler.
type MockMempool = MockService<mempool::Request, mempool::Response, PanicAssertion, BoxError>;

/// A test legacy [`UnminedTxId`] derived from a single byte. Uniform bytes make display and
/// serialized order coincide, so the wire round-trip is byte-for-byte stable.
fn txid(byte: u8) -> UnminedTxId {
    UnminedTxId::Legacy(transaction::Hash::from([byte; 32]))
}

/// The wire form of [`txid`].
fn proto_txid(byte: u8) -> ProtoUnminedTxId {
    ProtoUnminedTxId {
        mined_id: vec![byte; 32],
        auth_digest: Vec::new(),
    }
}

/// A wire `Queued` event for a single transaction at the given stage.
fn queued_event(byte: u8, stage: ProtoQueuedStage) -> MempoolEvent {
    MempoolEvent {
        event: Some(mempool_event::Event::Queued(MempoolQueued {
            tx_ids: vec![proto_txid(byte)],
            stage: stage as i32,
        })),
    }
}

/// A wire `Removed` event for a single transaction with the given reason.
fn removed_event(byte: u8, reason: mempool_removed::Reason) -> MempoolEvent {
    MempoolEvent {
        event: Some(mempool_event::Event::Removed(MempoolRemoved {
            tx_ids: vec![proto_txid(byte)],
            reason: Some(reason),
        })),
    }
}

/// A real serialized transaction (the genesis-successor block's coinbase) and its reconstructed
/// [`UnminedTx`], for exercising `Added` content reconstruction.
fn sample_tx() -> (Vec<u8>, UnminedTx) {
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("test block deserializes");
    let tx = block.transactions[0].clone();
    let bytes = tx.zcash_serialize_to_vec().expect("test tx serializes");
    (bytes, UnminedTx::from(tx))
}

/// A [`VerifiedReplicaTx`] wrapping `transaction` with zeroed metadata, for unit tests.
fn replica_tx(transaction: UnminedTx) -> VerifiedReplicaTx {
    VerifiedReplicaTx {
        transaction,
        miner_fee: 0,
        legacy_sigop_count: 0,
        p2sh_sigop_count: 0,
        conventional_actions: 0,
        unpaid_actions: 0,
        fee_weight_ratio: 0.0,
    }
}

#[test]
fn test_replica_idempotent_apply() {
    let mut replica = MempoolReplica::default();
    let (_, unmined) = sample_tx();
    let added_id = unmined.id;
    let content = replica_tx(unmined);

    // Apply a queued tx and a verified tx.
    replica.apply_queued(txid(1), QueuedStage::AwaitingDownload);
    replica.apply_added(added_id, content.clone());

    let digest = replica.digest();
    let verified_len = replica.verified_len();
    let queued_len = replica.queued_len();

    // Re-applying the same operations leaves the state byte-for-byte unchanged.
    assert!(replica
        .apply_queued(txid(1), QueuedStage::AwaitingDownload)
        .is_none());
    assert!(replica.apply_added(added_id, content).is_none());

    assert_eq!(replica.digest(), digest);
    assert_eq!(replica.verified_len(), verified_len);
    assert_eq!(replica.queued_len(), queued_len);

    // A stale queued event never regresses a verified tx (lifecycle-monotonic apply, §10).
    assert!(replica
        .apply_queued(added_id, QueuedStage::AwaitingDownload)
        .is_none());
    assert!(replica.contains_verified(&added_id));
    assert_eq!(replica.digest(), digest);
}

#[test]
fn test_queued_set_tracking() {
    let mut replica = MempoolReplica::default();
    let (_, unmined) = sample_tx();
    let id = unmined.id;

    assert!(replica
        .apply_queued(id, QueuedStage::AwaitingDownload)
        .is_some());
    assert_eq!(
        replica.queued_stage(&id),
        Some(QueuedStage::AwaitingDownload)
    );
    assert_eq!(replica.queued_len(), 1);
    assert_eq!(replica.verified_len(), 0);

    // The stage advances and is visible in the queued set.
    assert!(replica
        .apply_queued(id, QueuedStage::AwaitingVerification)
        .is_some());
    assert_eq!(
        replica.queued_stage(&id),
        Some(QueuedStage::AwaitingVerification)
    );

    // Verification moves it out of the queued set into the verified set.
    assert!(matches!(
        replica.apply_added(id, replica_tx(unmined)),
        Some(MempoolObservation::Added { tx_id }) if tx_id == id
    ));
    assert!(replica.contains_verified(&id));
    assert_eq!(replica.queued_stage(&id), None);
    assert_eq!(replica.queued_len(), 0);
}

#[test]
fn test_observation_feed() {
    let mut replica = MempoolReplica::default();
    let mut observations = Vec::new();

    apply_event(
        &mut replica,
        queued_event(1, ProtoQueuedStage::AwaitingDownload),
        &mut observations,
    )
    .expect("queued event applies");
    apply_event(
        &mut replica,
        removed_event(1, mempool_removed::Reason::Expired(Empty {})),
        &mut observations,
    )
    .expect("removed event applies");

    assert!(matches!(
        observations.as_slice(),
        [
            MempoolObservation::Queued { tx_id: queued, stage: QueuedStage::AwaitingDownload },
            MempoolObservation::Removed { tx_id: removed, reason: RemovedReason::Expired },
        ] if *queued == txid(1) && *removed == txid(1)
    ));
}

#[test]
fn test_added_event_reconstructs_content() {
    let (bytes, unmined) = sample_tx();
    let id = unmined.id;

    let event = MempoolEvent {
        event: Some(mempool_event::Event::Added(MempoolAdded {
            transaction: bytes,
            miner_fee: 7,
            legacy_sigop_count: 1,
            p2sh_sigop_count: 2,
            conventional_actions: 3,
            unpaid_actions: 4,
            fee_weight_ratio: 1.5,
        })),
    };

    let mut replica = MempoolReplica::default();
    let mut observations = Vec::new();
    apply_event(&mut replica, event, &mut observations).expect("added event applies");

    let stored = replica.verified_tx(&id).expect("tx is in the verified set");
    assert_eq!(stored.miner_fee, 7);
    assert_eq!(stored.transaction.id, id);
    assert!(matches!(
        observations.as_slice(),
        [MempoolObservation::Added { tx_id }] if *tx_id == id
    ));
}

#[test]
fn test_reorg_removes_then_readds() {
    let (_, unmined) = sample_tx();
    let id = unmined.id;

    let mut replica = MempoolReplica::default();
    replica.apply_added(id, replica_tx(unmined.clone()));
    assert!(replica.contains_verified(&id));
    let verified_digest = replica.digest();

    // A reorg starts a new generation by removing the tx entirely (design §10); its
    // content is re-sent on the wire by the source's re-verification `Added`, so
    // nothing is retained.
    let observation = replica.apply_removed(id, RemovedReason::Reorged);
    assert!(matches!(
        observation,
        Some(MempoolObservation::Removed {
            reason: RemovedReason::Reorged,
            ..
        })
    ));
    assert!(!replica.contains_verified(&id));
    assert_eq!(replica.queued_stage(&id), None);
    assert!(replica.verified_tx(&id).is_none());
    // The verified-set digest reflects the removal with no spurious mismatch: it is
    // now the empty-set digest, distinct from the pre-reorg digest.
    assert_ne!(replica.digest(), verified_digest);
    assert_eq!(replica.digest(), MempoolReplica::default().digest());

    // The source re-establishes the tx cleanly: Queued{AwaitingDownload} →
    // Queued{AwaitingVerification} → Added, with no stage dropped by the monotonic
    // guard, and the verified-set digest converges back.
    assert!(replica
        .apply_queued(id, QueuedStage::AwaitingDownload)
        .is_some());
    assert!(replica
        .apply_queued(id, QueuedStage::AwaitingVerification)
        .is_some());
    assert!(matches!(
        replica.apply_added(id, replica_tx(unmined)),
        Some(MempoolObservation::Added { tx_id }) if tx_id == id
    ));
    assert!(replica.contains_verified(&id));
    assert_eq!(replica.digest(), verified_digest);
}

#[test]
fn test_digest_matches_source_projection() {
    let (_, unmined) = sample_tx();
    let id = unmined.id;

    let mut replica = MempoolReplica::default();
    // A queued tx is excluded from the digest; only the verified set is hashed.
    replica.apply_queued(txid(1), QueuedStage::AwaitingDownload);
    replica.apply_added(id, replica_tx(unmined));

    // The follower's recomputed digest equals the source's digest over the same verified set.
    let verified: HashSet<UnminedTxId> = [id].into_iter().collect();

    assert_eq!(replica.digest(), replica_digest(&verified));
}

/// An empty bootstrap-state response.
fn empty_bootstrap_state() -> mempool::Response {
    mempool::Response::MempoolBootstrapState {
        queued: HashMap::new(),
        verified: Vec::new(),
        rejected: Vec::new(),
    }
}

/// A bootstrap-state response with a single queued transaction.
fn queued_bootstrap_state(byte: u8, stage: QueuedStage) -> mempool::Response {
    let mut queued = HashMap::new();
    queued.insert(txid(byte), stage);
    mempool::Response::MempoolBootstrapState {
        queued,
        verified: Vec::new(),
        rejected: Vec::new(),
    }
}

/// Starts an indexer gRPC server backed by mock services and a mempool change broadcast of the
/// given capacity, returning the listen address, a handle to drive the mock mempool, the broadcast
/// sender, and the handles that must stay alive for the server to keep serving.
async fn start_indexer_server(
    broadcast_capacity: usize,
) -> (
    SocketAddr,
    MockMempool,
    broadcast::Sender<MempoolBatch>,
    JoinHandle<Result<(), BoxError>>,
    MockChainTipSender,
) {
    let listen_addr: SocketAddr = "127.0.0.1:0"
        .parse()
        .expect("hard-coded IP and port should parse");

    let mock_read_service: MockService<ReadRequest, ReadResponse, PanicAssertion, BoxError> =
        MockService::build()
            .with_max_request_delay(Duration::from_secs(2))
            .for_unit_tests();
    let mock_mempool: MockMempool = MockService::build()
        .with_max_request_delay(Duration::from_secs(2))
        .for_unit_tests();
    let (mock_chain_tip, mock_chain_tip_sender) = MockChainTip::new();
    let (sender, _) = broadcast::channel(broadcast_capacity);
    let subscriber = MempoolTxSubscriber::new(sender.clone());

    let (server_task, listen_addr) = indexer::server::init(
        listen_addr,
        mock_read_service,
        mock_chain_tip,
        mock_mempool.clone(),
        subscriber,
    )
    .await
    .expect("indexer server starts");

    // Wait for the server to start.
    tokio::time::sleep(Duration::from_secs(1)).await;

    (
        listen_addr,
        mock_mempool,
        sender,
        server_task,
        mock_chain_tip_sender,
    )
}

/// Waits until the published replica satisfies `predicate`.
async fn wait_for_replica(
    receiver: &mut watch::Receiver<Arc<MempoolReplica>>,
    predicate: impl Fn(&MempoolReplica) -> bool,
) -> Arc<MempoolReplica> {
    loop {
        {
            let current = receiver.borrow_and_update();
            if predicate(&current) {
                return current.clone();
            }
        }

        timeout(Duration::from_secs(10), receiver.changed())
            .await
            .expect("replica should update before timeout")
            .expect("sync task should keep the watch sender alive");
    }
}

/// Waits until an observation satisfying `predicate` is received.
async fn wait_for_observation(
    receiver: &mut broadcast::Receiver<MempoolObservation>,
    predicate: impl Fn(&MempoolObservation) -> bool,
) -> MempoolObservation {
    loop {
        match timeout(Duration::from_secs(10), receiver.recv()).await {
            Ok(Ok(observation)) => {
                if predicate(&observation) {
                    return observation;
                }
            }
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => panic!("observation feed closed"),
            Err(_) => panic!("observation should arrive before timeout"),
        }
    }
}

/// End-to-end: the follower bootstraps from the indexer server, then applies a live change cycle,
/// publishing the updated replica and the lifecycle transition on the observation feed.
#[tokio::test]
async fn e2e_bootstrap_and_live_cycle() {
    let _init_guard = zebra_test::init();

    let (listen_addr, mut mock_mempool, sender, _server_task, _chain_tip_sender) =
        start_indexer_server(16).await;

    let (mut replica_receiver, mut observation_receiver, _task) =
        TrustedMempoolSync::spawn(listen_addr);

    // The follower bootstraps from a single queued transaction.
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(queued_bootstrap_state(1, QueuedStage::AwaitingDownload));

    wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(1)) == Some(QueuedStage::AwaitingDownload)
    })
    .await;

    // A live change cycle queues a second transaction for verification. The checksum is over the
    // follower's verified set, which is still empty (both txs are queued), so it is the empty-set
    // digest.
    let checksum = replica_digest(&HashSet::new());

    sender
        .send(MempoolBatch::new(
            vec![MempoolChange::queued_awaiting_verification(
                [txid(2)].into_iter().collect(),
            )],
            Some(checksum),
        ))
        .expect("the indexer server has a live subscriber");

    let replica = wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(2)) == Some(QueuedStage::AwaitingVerification)
    })
    .await;
    // The bootstrap tx is still present, and the steady-state digest matches the source checksum.
    assert_eq!(
        replica.queued_stage(&txid(1)),
        Some(QueuedStage::AwaitingDownload)
    );
    assert_eq!(replica.digest(), checksum);

    wait_for_observation(&mut observation_receiver, |observation| {
        matches!(
            observation,
            MempoolObservation::Queued { tx_id, stage: QueuedStage::AwaitingVerification }
                if *tx_id == txid(2)
        )
    })
    .await;
}

/// End-to-end: a live batch whose checksum doesn't match the follower's projection forces a
/// reconnect and re-bootstrap, and the reconnect marks a transient-observation gap (§3a, §3b).
#[tokio::test]
async fn e2e_checksum_mismatch_triggers_rebootstrap_and_gap() {
    let _init_guard = zebra_test::init();

    let (listen_addr, mut mock_mempool, sender, _server_task, _chain_tip_sender) =
        start_indexer_server(16).await;

    let (mut replica_receiver, mut observation_receiver, _task) =
        TrustedMempoolSync::spawn(listen_addr);

    // First bootstrap from an empty mempool.
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(empty_bootstrap_state());

    // A live batch carrying a checksum that cannot match the follower's projection.
    sender
        .send(MempoolBatch::new(
            vec![MempoolChange::queued_awaiting_download(
                [txid(5)].into_iter().collect(),
            )],
            Some([0xab; REPLICA_DIGEST_LEN]),
        ))
        .expect("the indexer server has a live subscriber");

    // The mismatch ends the session; the follower reconnects and re-bootstraps. The corrected
    // bootstrap carries a transaction the follower converges to.
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(queued_bootstrap_state(7, QueuedStage::AwaitingDownload));

    wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(7)) == Some(QueuedStage::AwaitingDownload)
    })
    .await;

    // The reconnect marked a transient-observation gap.
    wait_for_observation(&mut observation_receiver, |observation| {
        matches!(observation, MempoolObservation::Gap)
    })
    .await;
}

/// End-to-end: a reorg removes a transaction via `Removed{Reorged}`, then the source re-establishes
/// it through `Queued{AwaitingDownload}` / `Queued{AwaitingVerification}` re-verification events, and
/// the follower converges in lock-step with the source's checksums without a re-bootstrap (§5a, §10).
///
/// A `Removed{Reorged}` starts a new generation by dropping the tx entirely, so the subsequent
/// re-queue events re-add it cleanly with no stage dropped by the monotonic guard and no spurious
/// checksum mismatch. Only a single bootstrap is served, so any re-bootstrap would stall and time
/// out this test.
#[tokio::test]
async fn e2e_reorg_requeues_without_rebootstrap() {
    let _init_guard = zebra_test::init();

    let (listen_addr, mut mock_mempool, sender, _server_task, _chain_tip_sender) =
        start_indexer_server(16).await;

    let (mut replica_receiver, _observation_receiver, _task) =
        TrustedMempoolSync::spawn(listen_addr);

    // Bootstrap with a single transaction in the queued set, awaiting verification.
    mock_mempool
        .expect_request(mempool::Request::MempoolBootstrapState)
        .await
        .respond(queued_bootstrap_state(3, QueuedStage::AwaitingVerification));

    wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(3)) == Some(QueuedStage::AwaitingVerification)
    })
    .await;

    // A reorg removes the tx then re-adds it: the source emits `Removed{Reorged}` then re-enters it
    // at `AwaitingDownload`. The settled checksum is over the verified set, which is empty (the tx
    // is queued), so it is the empty-set digest.
    let download_checksum = replica_digest(&HashSet::new());

    sender
        .send(MempoolBatch::new(
            vec![
                MempoolChange::removed_reorged([txid(3)].into_iter().collect()),
                MempoolChange::queued_awaiting_download([txid(3)].into_iter().collect()),
            ],
            Some(download_checksum),
        ))
        .expect("the indexer server has a live subscriber");

    let replica = wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(3)) == Some(QueuedStage::AwaitingDownload)
    })
    .await;
    assert_eq!(replica.digest(), download_checksum);

    // The source then advances the re-verified tx to `AwaitingVerification`; the follower advances
    // in lock-step and its digest (over the empty verified set) still matches the source checksum.
    let verification_checksum = replica_digest(&HashSet::new());

    sender
        .send(MempoolBatch::new(
            vec![MempoolChange::queued_awaiting_verification(
                [txid(3)].into_iter().collect(),
            )],
            Some(verification_checksum),
        ))
        .expect("the indexer server has a live subscriber");

    let replica = wait_for_replica(&mut replica_receiver, |replica| {
        replica.queued_stage(&txid(3)) == Some(QueuedStage::AwaitingVerification)
    })
    .await;
    assert_eq!(replica.digest(), verification_checksum);
}
