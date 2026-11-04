//! Authenticated, next-tip-only early block retention, independent of mining subscriptions.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::sync::{watch, Semaphore};
use zebra_chain::{
    block::{self, Block},
    parameters::NetworkUpgrade,
};

use super::{read, NonFinalizedState, ReadStateService};
use crate::{BoxError, WatchReceiver};

#[cfg(test)]
mod tests;

const LIFETIME: Duration = Duration::from_secs(15);
// Includes wire contents and decoded metadata, using the shared conservative retention charge.
const MAX_RETAINED_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
struct Entry {
    block: Arc<Block>,
    hash: block::Hash,
    alive: watch::Receiver<()>,
    deadline: tokio::time::Instant,
}

/// Internal cache ownership does not enable publication; only external subscriptions do.
#[derive(Debug)]
pub(super) struct EarlyRelayState {
    snapshot: WatchReceiver<NonFinalizedState>,
    entry: Mutex<Option<Entry>>,
    changes: watch::Sender<Option<block::Hash>>,
    slot: Arc<Semaphore>,
    computation: Arc<Semaphore>,
}

impl EarlyRelayState {
    pub(super) fn new(snapshot: WatchReceiver<NonFinalizedState>) -> Arc<Self> {
        Arc::new(Self {
            snapshot,
            entry: Mutex::new(None),
            changes: watch::Sender::new(None),
            slot: Arc::new(Semaphore::new(1)),
            computation: Arc::new(Semaphore::new(1)),
        })
    }

    pub(super) fn cache(self: &Arc<Self>) -> EarlyRelayCache {
        EarlyRelayCache {
            state: self.clone(),
            changes: self.changes.subscribe(),
        }
    }

    pub(super) fn best_tip_hash(&self) -> Option<block::Hash> {
        self.snapshot.borrow_mapped(|state| {
            state
                .best_chain()
                .map(|chain| chain.non_finalized_tip_hash())
        })
    }

    fn rejections(&self) -> Arc<Mutex<super::non_finalized_state::RejectedBlocks>> {
        self.snapshot
            .borrow_mapped(|state| state.rejected_blocks.clone())
    }

    pub(super) fn allows_early_relay(&self, hash: block::Hash) -> bool {
        let rejected = self.rejections();
        let rejected = rejected
            .lock()
            .expect("authenticated rejection lock is not poisoned");
        !rejected.is_full() && !rejected.contains(hash)
    }

    fn reject(&self, hash: block::Hash, height: block::Height) -> bool {
        let rejected = self.rejections();
        let (full, became_full) = {
            let mut rejected = rejected
                .lock()
                .expect("authenticated rejection lock is not poisoned");
            let was_full = rejected.is_full();
            let full = rejected.reject(hash, height);
            (full, full && !was_full)
        };
        if became_full {
            tracing::warn!(
                capacity = super::non_finalized_state::MAX_EARLY_RELAY_REJECTIONS,
                "early relay and speculative mining disabled because rejection tracking is full"
            );
        }
        self.withdraw((!full).then_some(hash));
        full
    }

    fn withdraw(&self, hash: Option<block::Hash>) {
        let mut slot = self
            .entry
            .lock()
            .expect("early relay cache lock is not poisoned");
        if slot
            .as_ref()
            .is_some_and(|entry| hash.is_none_or(|hash| entry.hash == hash))
        {
            *slot = None;
            self.changes.send_replace(None);
        }
    }

    fn publish(self: &Arc<Self>, block: Arc<Block>, guard: &EarlyRelayGuard, retained: usize) {
        if self.changes.receiver_count() == 0
            || retained > MAX_RETAINED_BYTES
            || guard.alive.receiver_count() != 0
            || self.best_tip_hash() != Some(block.header.previous_block_hash)
        {
            return;
        }
        let Ok(permit) = self.slot.clone().try_acquire_owned() else {
            return;
        };
        let hash = block.hash();
        let parent = block.header.previous_block_hash;
        let alive = guard.alive.subscribe();
        let deadline = tokio::time::Instant::now() + LIFETIME;
        if !self.allows_early_relay(hash) {
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
        drop(slot);
        // Recheck after storing without nesting the rejection and cache locks. Notifications
        // remain hints: serving checks rejection again, including a concurrent withdrawal.
        if !self.allows_early_relay(hash) {
            self.withdraw(Some(hash));
            return;
        }
        self.changes.send_replace(Some(hash));
        let state = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut alive = alive;
            let mut snapshot = state.snapshot.clone();
            snapshot.mark_as_seen();
            while state.best_tip_hash() == Some(parent) {
                tokio::select! {
                    _ = alive.changed() => break,
                    _ = tokio::time::sleep_until(deadline) => break,
                    result = snapshot.changed() => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
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
            while state.best_tip_hash() == Some(parent) {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    result = snapshot.changed() => {
                        if result.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }
}

/// A bounded cache of authenticated, not fully validated blocks for negotiated early-relay peers.
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
    /// Returns a live body only while its parent is still the published snapshot's best tip.
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
            || self.state.best_tip_hash() != Some(entry.block.header.previous_block_hash)
        {
            *slot = None;
            self.state.changes.send_replace(None);
            return None;
        }
        let block = (entry.hash == *hash).then(|| entry.block.clone())?;
        drop(slot);
        if !self.state.allows_early_relay(*hash) {
            self.state.withdraw(Some(*hash));
            return None;
        }
        Some(block)
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
    alive: watch::Sender<()>,
    state: Arc<EarlyRelayState>,
    hash: block::Hash,
    height: block::Height,
}

impl EarlyRelayGuard {
    /// Returns whether this authenticated hash was deterministically rejected.
    ///
    /// Capacity alone does not reject an unknown block: ordinary full validation must remain
    /// available so commits and finalization can free rejection capacity.
    pub fn is_rejected(&self) -> bool {
        let rejected = self.state.rejections();
        let rejected = rejected
            .lock()
            .expect("authenticated rejection lock is not poisoned");
        rejected.contains(self.hash)
    }

    /// Suppresses this authenticated hash until its height is finalized, without retaining its body.
    ///
    /// Call only for deterministic validation failures, never time, state availability, lookup,
    /// infrastructure, or cancellation errors. A matching published body is withdrawn immediately,
    /// including one retained by another verifier. Overflow disables all early relay until the
    /// unrecorded rejections' heights are finalized.
    pub fn reject(&self) {
        self.state.reject(self.hash, self.height);
    }
}

impl PartialEq for EarlyRelayGuard {
    fn eq(&self, other: &Self) -> bool {
        self.alive.same_channel(&other.alive)
    }
}
impl Eq for EarlyRelayGuard {}

impl ReadStateService {
    /// Authenticates a verifier-reported deterministic failure without publishing speculative work.
    pub(super) async fn reject_block(self, block: Arc<Block>) -> bool {
        let permit = self
            .early_relay
            .computation
            .clone()
            .acquire_owned()
            .await
            .expect("the authentication semaphore is never closed");
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let Some(height) = block.coinbase_height() else {
                return false;
            };
            let merkle_root: block::merkle::Root =
                block.transactions.iter().map(|tx| tx.hash()).collect();
            if merkle_root != block.header.merkle_root {
                return false;
            }
            let snapshot = self.latest_non_finalized_state();
            let Some(tip) = read::best_tip(&snapshot, &self.db) else {
                return false;
            };
            let hash = block.hash();
            if read::difficulty::mining_candidate_header_context(
                &snapshot,
                &self.db,
                &self.network,
                block,
                tip,
            )
            .is_none()
            {
                return false;
            }
            let full = self.early_relay.reject(hash, height);
            self.mining_candidate.send_if_modified(|candidate| {
                if candidate
                    .as_ref()
                    .is_some_and(|(_, info, _, _)| full || info.tip_hash == hash)
                {
                    *candidate = None;
                    true
                } else {
                    false
                }
            });
            true
        })
        .await
        .expect("rejection authentication uses validated state snapshots")
    }

    pub(super) async fn early_relay(
        self,
        block: Arc<Block>,
        required: bool,
        publish: bool,
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
            .computation
            .clone()
            .acquire_owned()
            .await
            .expect("the authentication semaphore is never closed");
        let state = self.clone();
        let candidate = block.clone();
        let admitted = tokio::task::spawn_blocking(move || {
            // A cancelled request retains computation admission until this closure finishes.
            let _permit = permit;
            read::difficulty::authenticated_candidate_header(
                &state.latest_non_finalized_state(),
                &state.db,
                &state.network,
                candidate.clone(),
                !required,
            )
            .map(|history| {
                history.map(|_| {
                    publish.then(|| crate::request::block_wire_and_retained_bytes(&candidate, &[]))
                })
            })
        })
        .await
        .expect("header admission uses validated state snapshots")?;
        let Some(charge) = admitted else {
            return Ok(None);
        };
        let guard = EarlyRelayGuard {
            alive: watch::Sender::new(()),
            state: self.early_relay.clone(),
            hash: block.hash(),
            height,
        };
        if guard.is_rejected() {
            return Ok(Some(guard));
        }
        if let Some((wire, retained)) = charge {
            if u64::try_from(wire).unwrap_or(u64::MAX) <= block::MAX_BLOCK_BYTES {
                self.early_relay.publish(block, &guard, retained);
            }
        }
        Ok(Some(guard))
    }
}
