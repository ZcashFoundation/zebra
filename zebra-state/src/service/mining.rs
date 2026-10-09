//! Mining-only private forks and completed proposal verification results.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::watch;
use zebra_chain::{
    block,
    serialization::{sha256d, ZcashSerialize},
    work::equihash,
};

use super::non_finalized_state::NonFinalizedState;
use crate::SemanticallyVerifiedBlock;

#[cfg(test)]
mod tests;

/// Maximum lifetime of an unverified mining parent.
pub(super) const SPECULATIVE_LIFETIME: Duration = Duration::from_secs(30);
pub(super) const MAX_PROPOSALS: usize = 8;

/// Hash every serialized byte except the nonce and Equihash solution.
///
/// In particular this includes authorizing data and the transaction count: a Merkle root alone
/// cannot distinguish duplicated trailing transactions.
pub fn proposal_key(block: &block::Block) -> [u8; 32] {
    let mut writer = sha256d::Writer::default();
    serialize_proposal(block, &mut writer).expect("hash writers cannot fail");
    writer.finish()
}

fn normalized_header(block: &block::Block) -> block::Header {
    block::Header {
        nonce: [0; 32].into(),
        solution: equihash::Solution::Common([0; 1344]),
        ..*block.header
    }
}

fn serialize_proposal(
    block: &block::Block,
    mut writer: impl std::io::Write,
) -> std::io::Result<()> {
    normalized_header(block).zcash_serialize(&mut writer)?;
    block.transactions.zcash_serialize(writer)
}

pub(super) fn proposal_bytes(block: &block::Block) -> Arc<[u8]> {
    let mut bytes = Vec::new();
    serialize_proposal(block, &mut bytes).expect("serializing into a vector cannot fail");
    bytes.into()
}

/// Compare actual serialized bytes, rather than transaction equality (which ignores auth data).
pub(super) fn matches_proposal(block: &block::Block, expected: &[u8]) -> bool {
    struct Compare<'a>(&'a [u8]);
    impl std::io::Write for Compare<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.starts_with(bytes) {
                self.0 = &self.0[bytes.len()..];
                Ok(bytes.len())
            } else {
                Err(std::io::Error::from(std::io::ErrorKind::InvalidData))
            }
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut compare = Compare(expected);
    serialize_proposal(block, &mut compare).is_ok() && compare.0.is_empty()
}

#[derive(Clone, Debug)]
pub(super) struct Proposal {
    pub key: [u8; 32],
    pub parent: block::Hash,
    pub contextual: Arc<crate::ContextuallyVerifiedBlock>,
    pub generation: u64,
    pub serialized: Arc<[u8]>,
}

impl Proposal {
    pub fn new(contextual: Arc<crate::ContextuallyVerifiedBlock>, generation: u64) -> Option<Self> {
        let serialized = proposal_bytes(&contextual.block);
        // Common is the largest current solution encoding. Declining this cache entry bounds any
        // nonce/solution-rebound submission without conflating solution lengths in the cache key.
        if u64::try_from(serialized.len()).expect("block byte lengths fit u64")
            > block::MAX_BLOCK_BYTES
        {
            return None;
        }
        let mut writer = sha256d::Writer::default();
        std::io::Write::write_all(&mut writer, &serialized).expect("hash writers cannot fail");
        Some(Self {
            key: writer.finish(),
            parent: contextual.block.header.previous_block_hash,
            contextual,
            generation,
            serialized,
        })
    }

    pub fn matches(&self, block: &block::Block) -> bool {
        // Header equality also guards in-memory fractional timestamps that wire serialization
        // rounds to seconds; normal deserialized headers have no fractional timestamp.
        normalized_header(&self.contextual.block) == normalized_header(block)
            && matches_proposal(block, &self.serialized)
    }

    pub fn rebind(
        &self,
        block: Arc<block::Block>,
        received_time: Option<Instant>,
    ) -> crate::ContextuallyVerifiedBlock {
        let mut contextual = (*self.contextual).clone();
        contextual.hash = block.hash();
        contextual.block = block;
        contextual.received_time = received_time;
        // Outpoints, transaction hashes, spent outputs and pool deltas depend on body and parent,
        // not nonce/solution. Size is deliberately recomputed by normal Chain::push application.
        contextual
    }
}

#[derive(Clone, Debug)]
pub(super) struct Staged {
    pub parent: block::Hash,
    pub expires: Instant,
    pub state: NonFinalizedState,
}

#[derive(Clone, Debug, Default)]
pub(super) struct MiningState {
    pub validated_tip: Option<(block::Height, block::Hash)>,
    pub generation: u64,
    pub staged: Option<Arc<Staged>>,
    pub proposals: std::collections::VecDeque<Arc<Proposal>>,
    #[cfg(test)]
    pub full_contextual_checks: u64,
    #[cfg(test)]
    pub contextual_commit_hits: u64,
    #[cfg(test)]
    pub contextual_admission_hits: u64,
}

impl MiningState {
    pub fn tip(&self) -> Option<(block::Height, block::Hash)> {
        self.staged
            .as_ref()
            .and_then(|s| s.state.best_tip())
            .or(self.validated_tip)
    }
}

/// Shared mining channels used by the reader and the sole mutation writer.
#[derive(Clone, Debug)]
pub(super) struct MiningChannels {
    pub state: watch::Sender<MiningState>,
    pub tip: watch::Sender<Option<(block::Height, block::Hash)>>,
}

impl MiningChannels {
    pub fn new(validated_tip: Option<(block::Height, block::Hash)>) -> Self {
        let (state, _) = watch::channel(MiningState {
            validated_tip,
            ..Default::default()
        });
        let (tip, _) = watch::channel(validated_tip);
        Self { state, tip }
    }

    /// Retire contextual verdicts at the writer mutation boundary, retaining compatible mining
    /// work if that exact staged block is now being committed.
    pub fn invalidate(&self, committing: Option<block::Hash>) {
        self.state
            .send_modify(|state| self.retire(state, committing));
    }

    fn retire(&self, state: &mut MiningState, committing: Option<block::Hash>) {
        state.generation = state.generation.wrapping_add(1);
        state.proposals.clear();
        let keep = state.staged.as_ref().is_some_and(|stage| {
            stage.state.best_tip().map(|tip| tip.1) == committing && committing.is_some()
        });
        if !keep {
            state.staged = None;
        }
        let _ = self.tip.send_replace(state.tip());
    }

    /// Match outside the watch lock, then consume the verdict and retire its generation atomically.
    pub fn take_for_commit(
        &self,
        block: &block::Block,
        validated_tip: Option<(block::Height, block::Hash)>,
        hash: block::Hash,
    ) -> Option<Arc<Proposal>> {
        let matched = self.matching(block, validated_tip);
        let mut cached = None;
        self.state.send_modify(|state| {
            if let Some(proposal) = matched {
                if proposal.generation == state.generation
                    && state.proposals.iter().any(|p| Arc::ptr_eq(p, &proposal))
                {
                    cached = Some(proposal);
                }
            }
            self.retire(state, Some(hash));
        });
        cached
    }

    /// Force a notification even when a speculative parent becomes the same validated hash.
    pub fn publish_validated_tip(&self, validated_tip: Option<(block::Height, block::Hash)>) {
        self.state.send_modify(|state| {
            state.validated_tip = validated_tip;
            state.staged = None;
            let _ = self.tip.send_replace(validated_tip);
        });
    }

    pub fn matching(
        &self,
        block: &block::Block,
        validated_tip: Option<(block::Height, block::Hash)>,
    ) -> Option<Arc<Proposal>> {
        let parent = block.header.previous_block_hash;
        if validated_tip.map(|tip| tip.1) != Some(parent) {
            return None;
        }
        let (generation, proposals) = {
            let state = self.state.borrow();
            let proposals: [Option<Arc<Proposal>>; MAX_PROPOSALS] =
                std::array::from_fn(|index| state.proposals.get(index).cloned());
            (state.generation, proposals)
        };
        proposals.into_iter().flatten().find(|proposal| {
            proposal.generation == generation
                && proposal.parent == parent
                && proposal.matches(block)
        })
    }

    #[cfg(test)]
    pub fn note_full_contextual_check(&self) {
        self.state
            .send_modify(|state| state.full_contextual_checks += 1);
    }
}

/// A completed semantic proposal whose contextual result must be stamped by the writer.
pub(super) struct ProposalValidation {
    pub prepared: SemanticallyVerifiedBlock,
    pub response: tokio::sync::oneshot::Sender<Result<(), super::BoxError>>,
    pub _permit: tokio::sync::OwnedSemaphorePermit,
}

impl ProposalValidation {
    pub fn validate(
        self,
        channels: &MiningChannels,
        state: &NonFinalizedState,
        db: &super::finalized_state::ZebraDb,
    ) {
        let result = (|| -> Result<(), super::BoxError> {
            if self.response.is_closed() {
                return Ok(());
            }
            let parent = self.prepared.block.header.previous_block_hash;
            let validated_tip = super::read::best_tip(state, db);
            let (generation, staged) = {
                let mining = channels.state.borrow();
                (mining.generation, mining.staged.clone())
            };
            let ordinary_parent = validated_tip.map(|tip| tip.1) == Some(parent);
            let mut fork = if ordinary_parent {
                state.clone()
            } else {
                staged
                    .as_ref()
                    .filter(|stage| {
                        stage.expires > Instant::now()
                            && validated_tip.map(|tip| tip.1) == Some(stage.parent)
                            && stage.state.best_tip().map(|tip| tip.1) == Some(parent)
                    })
                    .ok_or("proposal must extend the current validated or fresh mining tip")?
                    .state
                    .clone()
            };
            fork.disable_metrics();
            #[cfg(test)]
            channels.note_full_contextual_check();
            super::write::validate_and_commit_non_finalized(db, &mut fork, self.prepared)?;
            if self.response.is_closed() {
                return Ok(());
            }
            // A staged parent is not consensus authority. Its child can be published as private
            // mining work, but must be preflighted again after the parent is actually committed.
            if !ordinary_parent {
                let mining = channels.state.borrow();
                if mining.generation != generation
                    || !mining.staged.as_ref().is_some_and(|stage| {
                        stage.expires > Instant::now()
                            && staged
                                .as_ref()
                                .is_some_and(|original| Arc::ptr_eq(stage, original))
                    })
                {
                    return Err("mining parent changed during proposal validation".into());
                }
                return Ok(());
            }
            let contextual = Arc::new(
                fork.best_tip_block()
                    .expect("a direct child of the best tip becomes the private fork tip")
                    .clone(),
            );
            // A short-solution proposal can be valid but too large to cache conservatively.
            if let Some(proposal) = Proposal::new(contextual, generation) {
                let proposal = Arc::new(proposal);
                channels.state.send_modify(|mining| {
                    if mining.generation != generation {
                        return;
                    }
                    mining
                        .proposals
                        .retain(|p| p.parent == parent && p.key != proposal.key);
                    if mining.proposals.len() == MAX_PROPOSALS {
                        mining.proposals.pop_front();
                    }
                    mining.proposals.push_back(proposal);
                });
            }
            Ok(())
        })();
        let _ = self.response.send(result);
    }
}

/// A bounded watch subscription to the mining parent, including fallback to the validated tip.
///
/// Intermediate updates can be coalesced. Call `receiver.changed().await` then
/// `receiver.borrow_and_update()`; discard cached mining work on every change.
#[derive(Clone, Debug)]
pub struct MiningTipChange {
    /// The current mining parent and subsequent changes.
    pub receiver: watch::Receiver<Option<(block::Height, block::Hash)>>,
}
impl PartialEq for MiningTipChange {
    fn eq(&self, other: &Self) -> bool {
        self.receiver.same_channel(&other.receiver)
    }
}
impl Eq for MiningTipChange {}

/// Keeps a speculative mining parent alive only while its full verifier is alive.
///
/// Dropping the last clone cancels staging immediately, including when verification is cancelled.
#[derive(Clone, Debug)]
pub struct MiningStageGuard(Arc<StageCancellation>);
impl PartialEq for MiningStageGuard {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for MiningStageGuard {}

#[derive(Debug)]
struct StageCancellation {
    expires: Instant,
    state: watch::Sender<MiningState>,
    tip: watch::Sender<Option<(block::Height, block::Hash)>>,
}
impl Drop for StageCancellation {
    fn drop(&mut self) {
        cancel(&self.state, &self.tip, self.expires);
    }
}

pub(super) fn cancel(
    state: &watch::Sender<MiningState>,
    tip: &watch::Sender<Option<(block::Height, block::Hash)>>,
    expires: Instant,
) {
    state.send_if_modified(|state| {
        if state.staged.as_ref().is_some_and(|s| s.expires == expires) {
            state.staged = None;
            let _ = tip.send_replace(state.validated_tip);
            true
        } else {
            false
        }
    });
}

pub(super) fn guard(
    state: watch::Sender<MiningState>,
    tip: watch::Sender<Option<(block::Height, block::Hash)>>,
    expires: Instant,
) -> MiningStageGuard {
    let guard = MiningStageGuard(Arc::new(StageCancellation {
        expires,
        state: state.clone(),
        tip: tip.clone(),
    }));
    let mut changes = state.subscribe();
    tokio::spawn(async move {
        loop {
            let active = changes
                .borrow_and_update()
                .staged
                .as_ref()
                .is_some_and(|stage| stage.expires == expires);
            if !active {
                return;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(expires.into()) => {
                    cancel(&state, &tip, expires);
                    return;
                }
                changed = changes.changed() => {
                    if changed.is_err() { return; }
                }
            }
        }
    });
    guard
}

/// At most one queued/in-flight admission exists; the permit is held through writer processing.
pub(super) struct PreparedAdmission {
    pub block: Arc<block::Block>,
    pub state: watch::Sender<MiningState>,
    pub tip: watch::Sender<Option<(block::Height, block::Hash)>>,
    pub runtime: tokio::runtime::Handle,
    pub permit: tokio::sync::OwnedSemaphorePermit,
    pub response: tokio::sync::oneshot::Sender<Option<MiningStageGuard>>,
}

impl PreparedAdmission {
    /// Run only on the serialized writer, linearized with commits and operator mutations.
    pub fn admit(self, validated: &NonFinalizedState, db: &super::finalized_state::ZebraDb) {
        let guard = (|| {
            if self.response.is_closed() {
                return None;
            }
            let parent = self.block.header.previous_block_hash;
            let channels = MiningChannels {
                state: self.state.clone(),
                tip: self.tip.clone(),
            };
            if self.state.borrow().staged.is_some() {
                return None;
            }
            let proposal = channels.matching(&self.block, super::read::best_tip(validated, db))?;
            let generation = proposal.generation;
            let prepared = proposal.rebind(self.block.clone(), Some(Instant::now()));
            let mut fork = validated.clone();
            fork.disable_metrics();
            fork.commit_prevalidated_contextual(prepared, db).ok()?;
            if self.response.is_closed() {
                return None;
            }
            let expires = Instant::now() + SPECULATIVE_LIFETIME;
            let child_tip = fork.best_tip()?;
            let staged = Arc::new(Staged {
                parent,
                expires,
                state: fork,
            });
            let mut installed = false;
            self.state.send_if_modified(|state| {
                if state.generation == generation
                    && state.staged.is_none()
                    && state.proposals.iter().any(|p| Arc::ptr_eq(p, &proposal))
                {
                    state.validated_tip = super::read::best_tip(validated, db);
                    state.staged = Some(staged);
                    #[cfg(test)]
                    {
                        state.contextual_admission_hits += 1;
                    }
                    let _ = self.tip.send_replace(Some(child_tip));
                    installed = true;
                    true
                } else {
                    false
                }
            });
            installed.then(|| {
                let _runtime = self.runtime.enter();
                guard(self.state.clone(), self.tip.clone(), expires)
            })
        })();
        let _ = self.response.send(guard);
        drop(self.permit);
    }
}
