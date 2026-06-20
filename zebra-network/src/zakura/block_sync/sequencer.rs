//! The serial commit pipeline for Zakura block sync.
//!
//! The [`Sequencer`] owns the consensus-critical reorder → applying →
//! `SubmitBlock` → apply-finished machinery and nothing else. It deliberately
//! never touches download-side state — the byte budget, the work scheduler,
//! peers, emitted actions, or state queries. Two rules keep that boundary clean:
//!
//! - every method that frees reserved bytes *returns* the freed count, so the
//!   reactor releases it against the budget (the budget stays in the reactor for
//!   S1; S2 makes it shareable), and
//! - every download-side consequence (mark a height covered, clear covered,
//!   re-query, attribute misbehavior) is expressed as a value the reactor acts
//!   on, not performed here.
//!
//! The logic is preserved verbatim from the reactor; only its boundary changes.
//! That boundary is what lets a later stage move the Sequencer onto its own task.

use super::{events::BlockApplyToken, reorder::*, state::*, *};

/// A received body draining contiguously toward the verified tip, awaiting (or
/// undergoing) verifier submission.
#[derive(Clone, Debug)]
pub(super) struct ApplyingBlock {
    pub(super) token: BlockApplyToken,
    pub(super) hash: block::Hash,
    pub(super) block: Arc<block::Block>,
    pub(super) bytes: u64,
    pub(super) submitted: bool,
    /// The peer that delivered this body, used to attribute an apply rejection
    /// for misbehavior scoring.
    pub(super) source_peer: ZakuraPeerId,
}

/// Outcome of offering a received body to the commit pipeline.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum AcceptOutcome {
    /// The body was buffered and now owns its byte reservation. The reactor must
    /// mark `covered` covered in the download scheduler so the retry path stops
    /// re-requesting it.
    Buffered { covered: block::Height },
    /// The body was not buffered (already at/below the floor, held elsewhere in
    /// the commit pipeline, or a duplicate). The reactor must release the
    /// `release_bytes` the body still reserved.
    Redundant { release_bytes: u64 },
}

/// A body the Sequencer has assigned a token and marked submitted; the reactor
/// dispatches the matching `SubmitBlock` action.
#[derive(Clone, Debug)]
pub(super) struct SubmitItem {
    pub(super) height: block::Height,
    pub(super) hash: block::Hash,
    pub(super) token: BlockApplyToken,
    pub(super) block: Arc<block::Block>,
}

/// Sequencer half of a verified-tip advance (frontier growth/commit).
#[derive(Copy, Clone, Debug)]
pub(super) struct AdvanceOutcome {
    /// Bytes freed from the reorder/applying buffers that the reactor releases.
    pub(super) release_bytes: u64,
    /// Whether the verified tip actually moved. The reactor drops download state
    /// (scheduler/outstanding) and re-drains only when it did.
    pub(super) changed: bool,
}

/// The reorder → applying → submit → apply-finished commit pipeline.
#[derive(Clone, Debug)]
pub(super) struct Sequencer {
    reorder: ReorderBuffer,
    applying: BTreeMap<block::Height, ApplyingBlock>,
    submitted_applies: BTreeMap<block::Height, Vec<(block::Hash, usize)>>,
    next_apply_token: BlockApplyToken,
    body_download_floor: block::Height,
    verified_block_tip: block::Height,
    submitted_apply_limit: usize,
}

impl Sequencer {
    pub(super) fn new(verified_block_tip: block::Height, submitted_apply_limit: usize) -> Self {
        Self {
            reorder: ReorderBuffer::new(),
            applying: BTreeMap::new(),
            submitted_applies: BTreeMap::new(),
            next_apply_token: 1,
            body_download_floor: verified_block_tip,
            verified_block_tip,
            submitted_apply_limit,
        }
    }

    // ---- reads (download side queries the commit pipeline through these) ----

    pub(super) fn floor(&self) -> block::Height {
        self.body_download_floor
    }

    pub(super) fn verified_tip(&self) -> block::Height {
        self.verified_block_tip
    }

    pub(super) fn reorder_contains(&self, height: block::Height) -> bool {
        self.reorder.contains(height)
    }

    pub(super) fn applying_contains(&self, height: block::Height) -> bool {
        self.applying.contains_key(&height)
    }

    pub(super) fn submitted_contains(&self, height: block::Height) -> bool {
        self.submitted_applies.contains_key(&height)
    }

    pub(super) fn reorder_len(&self) -> usize {
        self.reorder.len()
    }

    pub(super) fn applying_len(&self) -> usize {
        self.applying.len()
    }

    pub(super) fn reorder_buffered_bytes(&self) -> u64 {
        self.reorder.buffered_bytes()
    }

    /// Number of `applying` bodies already submitted to the verifier.
    pub(super) fn submitted_applying_count(&self) -> usize {
        self.applying
            .values()
            .filter(|applying| applying.submitted)
            .count()
    }

    pub(super) fn has_submitted_apply(&self, height: block::Height, hash: block::Hash) -> bool {
        self.submitted_applies
            .get(&height)
            .is_some_and(|entries| entries.iter().any(|(entry_hash, _)| *entry_hash == hash))
    }

    /// A response for `height` is stale when the height is already at/below the
    /// download floor or held in a commit-pipeline buffer.
    pub(super) fn is_stale_response_height(&self, height: block::Height) -> bool {
        height <= self.body_download_floor
            || self.reorder.contains(height)
            || self.applying.contains_key(&height)
    }

    /// Whether any reorder/applying/submitted body sits at or above `height`,
    /// used by the reactor to decide whether a reset is anchored by active
    /// successor work.
    pub(super) fn has_buffered_at_or_above(&self, height: block::Height) -> bool {
        self.reorder.contains_at_or_above(height)
            || self.applying.range(height..).next().is_some()
            || self.submitted_applies.range(height..).next().is_some()
    }

    /// `previous_block_hash` of a held `applying` body, for deciding whether a
    /// reset orphans an already-submitted successor.
    pub(super) fn applying_previous_block_hash(
        &self,
        height: block::Height,
    ) -> Option<block::Hash> {
        self.applying
            .get(&height)
            .map(|applying| applying.block.header.previous_block_hash)
    }

    pub(super) fn reorder_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.reorder.hash(height)
    }

    pub(super) fn applying_hash(&self, height: block::Height) -> Option<block::Hash> {
        self.applying.get(&height).map(|applying| applying.hash)
    }

    /// True when `height` has submitted applies and *none* of them is `hash`
    /// (a reset to `hash` would conflict with our submitted work).
    pub(super) fn submitted_has_only_other_hashes(
        &self,
        height: block::Height,
        hash: block::Hash,
    ) -> bool {
        self.submitted_applies
            .get(&height)
            .is_some_and(|entries| entries.iter().all(|(entry_hash, _)| *entry_hash != hash))
    }

    // ---- body acceptance ----

    /// Offer a received body to the commit pipeline. Runs the redundancy checks
    /// and (when not redundant) buffers it in the reorder buffer, which takes
    /// ownership of the body's existing `bytes` reservation.
    pub(super) fn accept_body(
        &mut self,
        height: block::Height,
        hash: block::Hash,
        block: Arc<block::Block>,
        bytes: u64,
        source_peer: ZakuraPeerId,
    ) -> AcceptOutcome {
        if height <= self.body_download_floor
            || self.reorder.contains(height)
            || self.applying.contains_key(&height)
            || self.has_submitted_apply(height, hash)
        {
            return AcceptOutcome::Redundant {
                release_bytes: bytes,
            };
        }

        match self.reorder.insert(height, block, bytes, source_peer) {
            ReorderInsertResult::Inserted => AcceptOutcome::Buffered { covered: height },
            ReorderInsertResult::Duplicate => AcceptOutcome::Redundant {
                release_bytes: bytes,
            },
        }
    }

    // ---- drain reorder → applying ----

    /// Drain the contiguous reorder prefix above the floor into `applying`,
    /// advancing the floor. Returns the newly-covered heights so the reactor
    /// marks them covered in the download scheduler.
    pub(super) fn drain_ready_into_applying(&mut self) -> Vec<block::Height> {
        let released = self
            .reorder
            .drain_contiguous_prefix(self.body_download_floor);
        let mut covered = Vec::with_capacity(released.len());
        for (height, block, bytes, source_peer) in released {
            let hash = block.hash();
            self.body_download_floor = height;
            covered.push(height);
            self.applying.insert(
                height,
                ApplyingBlock {
                    token: 0,
                    hash,
                    block,
                    bytes,
                    submitted: false,
                    source_peer,
                },
            );
        }
        covered
    }

    // ---- submission ----

    /// The unsubmitted `applying` heights eligible for verifier submission,
    /// bounded by the remaining submission window.
    pub(super) fn submittable_heights(&self) -> Vec<block::Height> {
        let available = self
            .submitted_apply_limit
            .saturating_sub(self.submitted_applying_count());
        if available == 0 {
            return Vec::new();
        }
        self.applying
            .iter()
            .filter_map(|(height, applying)| (!applying.submitted).then_some(*height))
            .take(available)
            .collect()
    }

    /// Assign a token to `height`, mark it submitted, and return the dispatch
    /// item. `None` if the height is no longer applying (the token counter is
    /// not consumed in that case).
    pub(super) fn prepare_submit(&mut self, height: block::Height) -> Option<SubmitItem> {
        let block = self
            .applying
            .get(&height)
            .map(|applying| applying.block.clone())?;
        let token = self.next_apply_token();
        let applying = self.applying.get_mut(&height)?;
        applying.token = token;
        applying.submitted = true;
        Some(SubmitItem {
            height,
            hash: applying.hash,
            token,
            block,
        })
    }

    /// Roll back a submit whose dispatch failed (only if the token still matches,
    /// so a stale rollback cannot clobber a newer submission).
    pub(super) fn unsubmit(&mut self, height: block::Height, token: BlockApplyToken) {
        if let Some(applying) = self.applying.get_mut(&height) {
            if applying.token == token {
                applying.token = 0;
                applying.submitted = false;
            }
        }
    }

    fn next_apply_token(&mut self) -> BlockApplyToken {
        let token = self.next_apply_token;
        self.next_apply_token = self.next_apply_token.checked_add(1).unwrap_or(1);
        token
    }

    pub(super) fn record_submitted_apply(&mut self, height: block::Height, hash: block::Hash) {
        let entries = self.submitted_applies.entry(height).or_default();
        if let Some((_, count)) = entries
            .iter_mut()
            .find(|(entry_hash, _)| *entry_hash == hash)
        {
            *count = count.saturating_add(1);
        } else {
            entries.push((hash, 1));
        }
    }

    pub(super) fn decrement_submitted_apply(&mut self, height: block::Height, hash: block::Hash) {
        let Some(entries) = self.submitted_applies.get_mut(&height) else {
            return;
        };
        if let Some(index) = entries
            .iter()
            .position(|(entry_hash, _)| *entry_hash == hash)
        {
            let (_, count) = &mut entries[index];
            *count = count.saturating_sub(1);
            if *count == 0 {
                entries.remove(index);
            }
        }
        if entries.is_empty() {
            self.submitted_applies.remove(&height);
        }
    }

    fn clear_submitted_applies_from(&mut self, from: block::Height) {
        let heights: Vec<_> = self
            .submitted_applies
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        for height in heights {
            self.submitted_applies.remove(&height);
        }
    }

    // ---- apply finished ----

    /// The `(token, hash)` of the body currently applying at `height`, for
    /// validating an apply-finished completion against the live submission.
    pub(super) fn applying_token_hash(
        &self,
        height: block::Height,
    ) -> Option<(BlockApplyToken, block::Hash)> {
        self.applying
            .get(&height)
            .map(|applying| (applying.token, applying.hash))
    }

    pub(super) fn remove_applying(&mut self, height: block::Height) -> Option<ApplyingBlock> {
        self.applying.remove(&height)
    }

    /// After a rejected/timed-out apply at `height`, roll the download floor back
    /// below it — never below the verified tip — so the height is re-requestable.
    pub(super) fn reset_floor_below(&mut self, height: block::Height) {
        self.body_download_floor = previous_height(height)
            .unwrap_or(block::Height::MIN)
            .max(self.verified_block_tip);
    }

    /// Drop buffered reorder bodies at or above `from`; returns the freed bytes.
    pub(super) fn drop_reorder_from(&mut self, from: block::Height) -> u64 {
        self.reorder.drop_from(from)
    }

    /// Remove `applying` bodies at or above `from` and clear their submitted
    /// applies; returns the freed bytes.
    pub(super) fn release_applying_blocks_from(&mut self, from: block::Height) -> u64 {
        let heights: Vec<_> = self
            .applying
            .range(from..)
            .map(|(height, _)| *height)
            .collect();
        let mut released = 0u64;
        for height in heights {
            if let Some(applying) = self.applying.remove(&height) {
                released = released.saturating_add(applying.bytes);
            }
        }
        self.clear_submitted_applies_from(from);
        released
    }

    /// Remove committed `applying` bodies at or below `tip`; returns freed bytes.
    pub(super) fn release_applied_through(&mut self, tip: block::Height) -> u64 {
        let applied: Vec<_> = self
            .applying
            .range(..=tip)
            .map(|(height, _)| *height)
            .collect();
        let mut released = 0u64;
        for height in applied {
            if let Some(applying) = self.applying.remove(&height) {
                released = released.saturating_add(applying.bytes);
            }
        }
        released
    }

    // ---- frontier advance / reset ----

    /// Advance the verified tip to `new_tip` (frontier growth/commit). Bumps the
    /// floor unconditionally, drops superseded reorder bodies (and, when
    /// `release_applied`, committed applying bodies), and moves the verified tip.
    /// Returns the freed bytes and whether the tip moved.
    pub(super) fn advance_verified_tip(
        &mut self,
        new_tip: block::Height,
        release_applied: bool,
    ) -> AdvanceOutcome {
        self.body_download_floor = self.body_download_floor.max(new_tip);
        if new_tip == self.verified_block_tip {
            return AdvanceOutcome {
                release_bytes: 0,
                changed: false,
            };
        }
        let mut released = self.reorder.drop_through(new_tip);
        if release_applied {
            released = released.saturating_add(self.release_applied_through(new_tip));
        }
        self.verified_block_tip = new_tip;
        AdvanceOutcome {
            release_bytes: released,
            changed: true,
        }
    }

    /// Destructively reset the commit pipeline to `new_tip` (reorg/rollback):
    /// clear the reorder buffer and all applying bodies (optionally preserving
    /// submitted-apply records), and pin the floor and verified tip to `new_tip`.
    /// Returns the freed bytes.
    pub(super) fn reset_to(&mut self, new_tip: block::Height, keep_submitted_applies: bool) -> u64 {
        self.verified_block_tip = new_tip;
        self.body_download_floor = new_tip;
        let mut released = self.reorder.clear();
        released =
            released.saturating_add(self.release_all_applying_for_reset(keep_submitted_applies));
        released
    }

    fn release_all_applying_for_reset(&mut self, keep_submitted_applies: bool) -> u64 {
        let released = self.applying.values().map(|applying| applying.bytes).sum();
        if !keep_submitted_applies {
            self.submitted_applies.clear();
        }
        self.applying.clear();
        released
    }
}
