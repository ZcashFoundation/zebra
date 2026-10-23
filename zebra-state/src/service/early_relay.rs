//! Authenticated, next-tip-only early block retention, independent of mining subscriptions.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use zebra_chain::{
    block::{self, Block},
    chain_tip::ChainTip,
    parameters::NetworkUpgrade,
};

use super::{read, LatestChainTip, ReadStateService};
use crate::BoxError;

#[cfg(test)]
mod tests;

const LIFETIME: Duration = Duration::from_secs(15);
const REJECTION_LIFETIME: Duration = Duration::from_secs(60);
const MAX_REJECTIONS: usize = 8;
// Includes wire contents and decoded metadata, using the shared conservative retention charge.
const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
struct Entry {
    block: Arc<Block>,
    hash: block::Hash,
    alive: watch::Receiver<Option<Arc<OwnedSemaphorePermit>>>,
    deadline: tokio::time::Instant,
}

#[derive(Debug)]
struct Rejections {
    entries: [Option<(block::Hash, tokio::time::Instant)>; MAX_REJECTIONS],
    next: usize,
}

impl Rejections {
    fn contains(&self, hash: block::Hash, now: tokio::time::Instant) -> bool {
        self.entries
            .iter()
            .flatten()
            .any(|(rejected, deadline)| *rejected == hash && now < *deadline)
    }
}

/// Internal cache ownership does not enable publication; only external subscriptions do.
#[derive(Debug)]
pub(super) struct EarlyRelayState {
    tip: Option<LatestChainTip>,
    entry: Mutex<Option<Entry>>,
    rejections: Mutex<Rejections>,
    changes: watch::Sender<Option<block::Hash>>,
    slot: Arc<Semaphore>,
}

impl EarlyRelayState {
    pub(super) fn new(tip: Option<LatestChainTip>) -> Arc<Self> {
        Arc::new(Self {
            tip,
            entry: Mutex::new(None),
            rejections: Mutex::new(Rejections {
                entries: [None; MAX_REJECTIONS],
                next: 0,
            }),
            changes: watch::Sender::new(None),
            slot: Arc::new(Semaphore::new(1)),
        })
    }

    pub(super) fn cache(self: &Arc<Self>) -> EarlyRelayCache {
        EarlyRelayCache {
            state: self.clone(),
            changes: self.changes.subscribe(),
        }
    }

    fn publish(self: &Arc<Self>, block: Arc<Block>, guard: &EarlyRelayGuard, retained: usize) {
        if self.changes.receiver_count() == 0
            || retained > MAX_RETAINED_BYTES
            || guard.alive.borrow().is_none()
            || self.tip.as_ref().and_then(ChainTip::best_tip_hash)
                != Some(block.header.previous_block_hash)
        {
            return;
        }
        let hash = block.hash();
        let parent = block.header.previous_block_hash;
        let alive = guard.alive.subscribe();
        let deadline = tokio::time::Instant::now() + LIFETIME;
        // Keep rejection lookup and publication atomic with rejection-driven withdrawal.
        let rejections = self
            .rejections
            .lock()
            .expect("early relay rejection lock is not poisoned");
        if rejections.contains(hash, tokio::time::Instant::now()) {
            return;
        }
        let mut slot = self
            .entry
            .lock()
            .expect("early relay cache lock is not poisoned");
        if slot.is_some() {
            return;
        }
        *slot = Some(Entry {
            block,
            hash,
            alive: alive.clone(),
            deadline,
        });
        // Store before notifying: an observed advertisement must already be servable.
        self.changes.send_replace(Some(hash));
        drop(slot);
        drop(rejections);
        let state = self.clone();
        tokio::spawn(async move {
            let mut alive = alive;
            tokio::select! {
                _ = alive.changed() => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
            {
                let mut slot = state
                    .entry
                    .lock()
                    .expect("early relay cache lock is not poisoned");
                if slot
                    .as_ref()
                    .is_some_and(|entry| entry.alive.same_channel(&alive))
                {
                    *slot = None;
                    state.changes.send_replace(None);
                }
            }
            // A rejected body must not buy repeated fanout with the same proof of work.
            // Keep the permit, but not the body, until the deadline or a new committed tip.
            let mut tip = state
                .tip
                .clone()
                .expect("published bodies have a committed parent");
            tip.mark_best_tip_seen();
            if tip.best_tip_hash() == Some(parent) {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {}
                    _ = tip.best_tip_changed() => {}
                }
            }
        });
    }
}

/// A bounded cache of authenticated but not fully validated blocks for negotiated early-relay peers.
///
/// Holding a cache or its subscription enables publication, independently of mining listeners.
/// At most one body is retained, for 15 seconds, with a 2 MB wire limit and a 64 MiB conservative
/// wire-content/decoded-metadata charge (not exact allocator RSS). Dropping its verifier guard
/// withdraws the body. A successful verifier holds the guard until state has published its read
/// snapshot, closing the cache-to-state serving gap. Ordinary state queries never read this cache.
/// After withdrawal, the admission slot remains reserved until its deadline or a committed-tip
/// change, preventing repeated early fanout of a rejected body without delaying consecutive tips.
#[derive(Clone, Debug)]
pub struct EarlyRelayCache {
    state: Arc<EarlyRelayState>,
    changes: watch::Receiver<Option<block::Hash>>,
}

impl PartialEq for EarlyRelayCache {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}
impl Eq for EarlyRelayCache {}

impl EarlyRelayCache {
    /// Returns a live body only while its parent is still the committed best tip.
    ///
    /// Only serve this body on a bilaterally negotiated early-relay connection. Expired, cancelled,
    /// or stale entries are removed before returning; callers must fall back to committed state.
    pub fn get(&self, hash: &block::Hash) -> Option<Arc<Block>> {
        let mut slot = self
            .state
            .entry
            .lock()
            .expect("early relay cache lock is not poisoned");
        let entry = slot.as_ref()?;
        if entry.alive.has_changed().is_err()
            || tokio::time::Instant::now() >= entry.deadline
            || self.state.tip.as_ref().and_then(ChainTip::best_tip_hash)
                != Some(entry.block.header.previous_block_hash)
        {
            *slot = None;
            self.state.changes.send_replace(None);
            return None;
        }
        (entry.hash == *hash).then(|| entry.block.clone())
    }

    /// Subscribes to the current candidate hash and withdrawals (`None`).
    ///
    /// Check [`Self::get`] before advertising: a notification may outlive its verifier or parent.
    pub fn subscribe(&self) -> watch::Receiver<Option<block::Hash>> {
        self.changes.clone()
    }
}

/// Proof that state authenticated a NU5-or-later body against a committed parent.
///
/// Keep this guard until verification and commit finish. Dropping its last clone withdraws any
/// cached body. The guard does not imply transaction validity or guarantee cache publication.
#[derive(Clone, Debug)]
pub struct EarlyRelayGuard {
    alive: watch::Sender<Option<Arc<OwnedSemaphorePermit>>>,
    state: Arc<EarlyRelayState>,
    hash: block::Hash,
}

impl EarlyRelayGuard {
    /// Returns whether this authenticated hash has a recent deterministic validation failure.
    pub fn is_rejected(&self) -> bool {
        let now = tokio::time::Instant::now();
        self.state
            .rejections
            .lock()
            .expect("early relay rejection lock is not poisoned")
            .contains(self.hash, now)
    }

    /// Suppresses this authenticated hash for 60 seconds without retaining its body.
    ///
    /// Call only for deterministic validation failures, never time, state availability, lookup,
    /// infrastructure, or cancellation errors. Eight hashes are retained in insertion order;
    /// repeated rejection does not extend the deadline. A matching published body is withdrawn
    /// immediately, including one retained by another verifier.
    pub fn reject(&self) {
        let now = tokio::time::Instant::now();
        let mut rejections = self
            .state
            .rejections
            .lock()
            .expect("early relay rejection lock is not poisoned");
        if !rejections.contains(self.hash, now) {
            let next = rejections.next;
            rejections.entries[next] = Some((self.hash, now + REJECTION_LIFETIME));
            rejections.next = (next + 1) % MAX_REJECTIONS;
        }
        let mut entry = self
            .state
            .entry
            .lock()
            .expect("early relay cache lock is not poisoned");
        if entry.as_ref().is_some_and(|entry| entry.hash == self.hash) {
            *entry = None;
            self.state.changes.send_replace(None);
        }
    }
}

impl PartialEq for EarlyRelayGuard {
    fn eq(&self, other: &Self) -> bool {
        self.alive.same_channel(&other.alive)
    }
}
impl Eq for EarlyRelayGuard {}

impl ReadStateService {
    pub(super) async fn early_relay(
        self,
        block: Arc<Block>,
        required: bool,
    ) -> Result<Option<EarlyRelayGuard>, BoxError> {
        let Some(height) = block.coinbase_height() else {
            return Ok(None);
        };
        // Pre-NU5 final Sapling roots require tree updates. Do not claim authentication there.
        if NetworkUpgrade::current(&self.network, height) < NetworkUpgrade::Nu5 {
            return Ok(None);
        }
        if !required && self.early_relay.changes.receiver_count() == 0 {
            return Ok(None);
        }
        let permit = self
            .early_relay
            .slot
            .clone()
            .try_acquire_owned()
            .ok()
            .map(Arc::new);
        if !required && permit.is_none() {
            return Ok(None);
        }
        let (alive, cancelled) = watch::channel(permit);
        let mut changes = self.non_finalized_state_receiver.clone();
        // Unknown parents can be in the existing bounded commit pipeline. Wait without granting
        // an exemption, and without adding an orphan-body queue or retaining bodies in the cache.
        let authenticate = async {
            loop {
                changes.mark_as_seen();
                let state = self.clone();
                let block = block.clone();
                let cancelled = cancelled.clone();
                let admitted = tokio::task::spawn_blocking(move || {
                    let _permit = cancelled.borrow().clone();
                    read::difficulty::authenticated_candidate_header(
                        &state.latest_non_finalized_state(),
                        &state.db,
                        &state.network,
                        block,
                        !required,
                    )
                    .map(|history| history.is_some())
                })
                .await
                .expect("header admission uses validated state snapshots")?;
                if admitted || !required {
                    return Ok::<_, BoxError>(admitted);
                }
                if changes.changed().await.is_err() {
                    return Ok(false);
                }
            }
        };
        if !tokio::time::timeout(Duration::from_secs(30), authenticate)
            .await
            .unwrap_or(Ok(false))?
        {
            return Ok(None);
        }
        let guard = EarlyRelayGuard {
            alive,
            state: self.early_relay.clone(),
            hash: block.hash(),
        };
        if guard.is_rejected() {
            return Ok(Some(guard));
        }
        if self.early_relay.changes.receiver_count() != 0 && guard.alive.borrow().is_some() {
            let block_for_charge = block.clone();
            let permit = guard.alive.borrow().clone();
            let (wire, retained) = tokio::task::spawn_blocking(move || {
                // Cancellation must not release the slot while this task still retains the body.
                let _permit = permit;
                crate::request::block_wire_and_retained_bytes(&block_for_charge, &[])
            })
            .await
            .expect("block retention accounting uses saturating arithmetic");
            if u64::try_from(wire).unwrap_or(u64::MAX) <= block::MAX_BLOCK_BYTES {
                self.early_relay.publish(block, &guard, retained);
            }
        }
        Ok(Some(guard))
    }
}
