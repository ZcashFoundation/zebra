//! Dandelion++ gossip task: stem-phase unicast + fluff-phase broadcast.
//!
//! This module provides [`gossip_dandelion`], a drop-in replacement for the
//! existing [`gossip_mempool_transaction_id`] task that routes transactions
//! through Dandelion++ stem/fluff rather than immediately broadcasting to all
//! peers.
//!
//! # Architecture
//!
//! ```text
//! mempool change channel ──► dandelion_gossip task
//!                                   │
//!                    ┌──────────────┴──────────────┐
//!                    │ stem phase                  │ fluff phase
//!                    ▼                             ▼
//!         AdvertiseTransactionIdsToPeer    AdvertiseTransactionIds
//!         (PeerSet::route_to_peer)         (existing broadcast path)
//!
//! DandelionEpochManager (background task)
//!   rotates stem peer every ~10 min
//! ```
//!
//! # Integration status
//!
//! Per-peer routing (Phase 2) is done: `PeerSet` routes
//! `Request::AdvertiseTransactionIdsToPeer` to exactly the named peer via
//! `route_to_peer`, failing with `PeerError::NoReadyPeers` if that peer isn't
//! ready — this task falls back to fluff immediately when that happens.
//!
//! **`MempoolChangeKind::StemAdded` vs `Added`**: the mempool emits `StemAdded`
//! for locally-submitted transactions (via RPC) and `Added` for peer-relayed
//! ones.  This task routes `StemAdded` through stem phase and routes `Added`
//! directly to fluff broadcast (the relaying peer handles its own stem routing).
//!
//! See `draft-y4ssi-dandelion-direct-submission.rst` for the full specification.

use std::sync::Arc;

use tokio::{
    sync::{broadcast, Mutex},
    time::{interval, Duration},
};
use tower::{timeout::Timeout, Service, ServiceExt};

use zebra_network::{
    dandelion::{DandelionEpochManager, PropagationStateMap},
    PeerSocketAddr, MAX_TX_INV_IN_SENT_MESSAGE,
};
use zebra_network as zn;
use zebra_node_services::mempool::MempoolChange;

use crate::{
    components::sync::{PEER_GOSSIP_DELAY, TIPS_RESPONSE_TIMEOUT},
    BoxError,
};

/// How often the stem-phase timeout checker runs.
const STEM_TIMEOUT_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Shared epoch manager — the gossip task and the epoch rotation task both
/// access it.
pub type SharedEpochManager = Arc<Mutex<DandelionEpochManager>>;

/// Shared propagation state map.
pub type SharedPropagationState = Arc<Mutex<PropagationStateMap>>;

/// Spawns two background tasks:
///
/// 1. **Epoch rotation task**: rotates the stem peer every `~EPOCH_DURATION`.
/// 2. **Gossip task**: routes incoming `Added` transactions through
///    stem/fluff according to the current epoch state.
///
/// Returns `(epoch_manager, propagation_state)` for inspection in tests.
///
/// # Caller responsibilities
///
/// The caller MUST provide a `get_outbound_peers` closure that returns the
/// current outbound peer addresses.  This decouples the epoch manager from
/// the `PeerSet` internals.
///
/// # Current limitations
///
/// Stem-phase transactions are now filtered from `mempool` P2P responses and
/// `getrawmempool`/`getrawtransaction` RPC responses (Phase 4).  Promotion to
/// fluff still happens only on timeout or stem-peer failure — observing the
/// transaction via normal relay does not yet trigger the transition
/// (unadvertised-`tx` convention, see `dandelion-TODO.md`).
pub fn spawn_dandelion_gossip<ZN, F>(
    receiver: broadcast::Receiver<MempoolChange>,
    broadcast_network: ZN,
    get_outbound_peers: F,
) -> (SharedEpochManager, SharedPropagationState)
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError>
        + Send
        + Clone
        + 'static,
    ZN::Future: Send,
    F: Fn() -> Vec<PeerSocketAddr> + Send + Sync + 'static,
{
    let epoch_manager = Arc::new(Mutex::new(DandelionEpochManager::new()));
    let prop_state = Arc::new(Mutex::new(PropagationStateMap::new()));

    // Epoch rotation task
    let epoch_manager_clone = epoch_manager.clone();
    tokio::spawn(async move {
        loop {
            let wait_fut = {
                let mgr = epoch_manager_clone.lock().await;
                // We can't hold the lock across .await, so check expiry first.
                let expired = mgr.is_expired();
                drop(mgr);
                expired
            };
            if wait_fut {
                // Rotate immediately.
            } else {
                // Sleep until epoch end.  We poll every second to avoid holding
                // the lock across a long sleep.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let peers = get_outbound_peers();
            let mut mgr = epoch_manager_clone.lock().await;
            mgr.rotate(&peers);
        }
    });

    // Gossip task
    let epoch_manager_gossip = epoch_manager.clone();
    let prop_state_gossip = prop_state.clone();
    tokio::spawn(gossip_dandelion(
        receiver,
        broadcast_network,
        epoch_manager_gossip,
        prop_state_gossip,
    ));

    (epoch_manager, prop_state)
}

/// Main Dandelion++ gossip loop.
///
/// Receives `MempoolChange::Added` events and routes each transaction:
///
/// - If the epoch manager has a stem peer: record the transaction as
///   `PropagationState::Stem` and unicast it to that peer via
///   `Request::AdvertiseTransactionIdsToPeer`, falling back to fluff if the
///   peer is not ready.
/// - Otherwise (fluff mode): broadcast immediately via
///   `AdvertiseTransactionIds`.
///
/// Additionally, on a [`STEM_TIMEOUT_CHECK_INTERVAL`] tick, any transactions
/// whose stem timeout has elapsed are promoted to fluff and broadcast.
pub async fn gossip_dandelion<ZN>(
    mut receiver: broadcast::Receiver<MempoolChange>,
    broadcast_network: ZN,
    epoch_manager: SharedEpochManager,
    prop_state: SharedPropagationState,
) -> Result<(), BoxError>
where
    ZN: Service<zn::Request, Response = zn::Response, Error = BoxError> + Send + Clone + 'static,
    ZN::Future: Send,
{
    let max_tx_inv: usize = MAX_TX_INV_IN_SENT_MESSAGE
        .try_into()
        .expect("constant fits in usize");

    info!("initializing dandelion++ gossip task");

    let mut broadcast_network = Timeout::new(broadcast_network, TIPS_RESPONSE_TIMEOUT);
    let mut stem_check_ticker = interval(STEM_TIMEOUT_CHECK_INTERVAL);

    loop {
        tokio::select! {
            // ── New transaction from the mempool ──────────────────────────
            change_result = receiver.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                use zebra_node_services::mempool::MempoolChangeKind;

                let change = match change_result {
                    Ok(c) if c.is_any_added() => c,
                    Ok(c) => {
                        // Mined or Invalidated: the transaction has left the mempool,
                        // so drop its propagation-state entry to bound map growth.
                        // (Without this, PropagationStateMap grows without limit —
                        // entries are otherwise never removed.)
                        let txids = c.into_tx_ids();
                        if !txids.is_empty() {
                            let mut ps = prop_state.lock().await;
                            for txid in &txids {
                                ps.remove(txid);
                            }
                        }
                        continue;
                    }
                    Err(RecvError::Lagged(n)) => {
                        info!(skipped = n, "dandelion++: dropped changes due to channel lag");
                        continue;
                    }
                    Err(RecvError::Closed) => return Err("mempool change channel closed".into()),
                };

                // `StemAdded` = locally-submitted transaction → route through stem.
                // `Added`     = peer-relayed transaction → already in fluff by convention.
                let is_stem_candidate = change.kind() == MempoolChangeKind::StemAdded;
                let txids = change.into_tx_ids();

                // Peer-relayed transactions skip stem and go straight to fluff.
                if !is_stem_candidate {
                    {
                        let mut ps = prop_state.lock().await;
                        for txid in &txids {
                            ps.insert_fluff(*txid);
                        }
                    }
                    let req = zn::Request::AdvertiseTransactionIds(txids, None);
                    match broadcast_network.ready().await {
                        Ok(svc) => {
                            let _ = svc.call(req).await;
                        }
                        // Fail open: a transient PeerSet readiness error must not
                        // kill the gossip task (which would stop all future
                        // stem/fluff routing for the process lifetime).
                        Err(err) => warn!(%err, "dandelion++: broadcast service not ready, skipping fluff"),
                    }
                    tokio::time::sleep(PEER_GOSSIP_DELAY).await;
                    continue;
                }

                let stem_peer_opt = epoch_manager.lock().await.stem_peer();

                match stem_peer_opt {
                    Some(stem_peer) => {
                        // Record as stem-phase; unicast to stem_peer.
                        {
                            let mut ps = prop_state.lock().await;
                            for txid in &txids {
                                ps.insert_stem(*txid, stem_peer);
                            }
                        }

                        let req = zn::Request::AdvertiseTransactionIdsToPeer(txids.clone(), stem_peer);
                        let stem_result = match broadcast_network.ready().await {
                            Ok(svc) => svc.call(req).await,
                            Err(err) => {
                                warn!(%err, "dandelion++: broadcast service not ready for stem unicast");
                                Err(err)
                            }
                        };
                        match stem_result {
                            Ok(_) => {
                                // Stem forwarding succeeded; the transaction stays
                                // in Stem state until fluff-observed or timed out
                                // (see the stem-timeout sweep below).
                            }
                            Err(err) => {
                                // Stem peer wasn't ready (e.g. dropped mid-epoch).
                                // Fall back to fluff immediately rather than
                                // silently dropping the transaction.
                                info!(%err, ?stem_peer, "dandelion++: stem peer unavailable, falling back to fluff");
                                {
                                    let mut ps = prop_state.lock().await;
                                    for txid in &txids {
                                        ps.promote_to_fluff(txid);
                                    }
                                }
                                let req = zn::Request::AdvertiseTransactionIds(txids, None);
                                if let Ok(svc) = broadcast_network.ready().await {
                                    let _ = svc.call(req).await;
                                }
                            }
                        }
                    }
                    None => {
                        // Fluff mode: no outbound peers for stem, broadcast directly.
                        {
                            let mut ps = prop_state.lock().await;
                            for txid in &txids {
                                ps.insert_fluff(*txid);
                            }
                        }
                        let req = zn::Request::AdvertiseTransactionIds(txids, None);
                        if let Ok(svc) = broadcast_network.ready().await {
                            let _ = svc.call(req).await;
                        }
                    }
                }

                tokio::time::sleep(PEER_GOSSIP_DELAY).await;
            }

            // ── Stem-timeout sweep ─────────────────────────────────────────
            _ = stem_check_ticker.tick() => {
                let expired = prop_state.lock().await.expired_stem_txids();
                if expired.is_empty() {
                    continue;
                }

                info!(count = expired.len(), "dandelion++: promoting timed-out stem txs to fluff");

                // Remove the entries now that they are fluffing: the exposure
                // filters treat "not in map" identically to "Fluff", so removal
                // both unblocks exposure and bounds map growth (the tx will be
                // removed from the mempool later via a Mined/Invalidated event,
                // which we also clean up above).
                {
                    let mut ps = prop_state.lock().await;
                    for txid in &expired {
                        ps.remove(txid);
                    }
                }

                // Note on indexer visibility: we deliberately do NOT re-emit a
                // `MempoolChange::added` on the shared channel here.  The gossip
                // task is itself a subscriber, so emitting would cause it to
                // re-consume the event and double-broadcast.  Downstream
                // consumers (the indexer gRPC stream used by Zaino/lightwalletd)
                // pick up the now-fluffed transaction on their next
                // `getrawmempool` / mempool poll — the exposure filters no
                // longer hide it once it is out of the stem state.

                // Chunk into messages respecting the per-message limit.
                for chunk in expired.chunks(max_tx_inv) {
                    let txids: std::collections::HashSet<_> = chunk.iter().copied().collect();
                    let req = zn::Request::AdvertiseTransactionIds(txids, None);
                    if let Ok(svc) = broadcast_network.ready().await {
                        let _ = svc.call(req).await;
                    }
                }
            }
        }
    }
}
