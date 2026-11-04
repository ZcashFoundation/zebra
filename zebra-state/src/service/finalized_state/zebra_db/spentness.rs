//! Spentness hints in the ordinary finalized writer (SwiftSync; ported from
//! `zakura/feat/spentness-writer` a909419a59).
//!
//! Below the artifact's terminal height `H`, each new transparent output's
//! bit decides where it goes:
//! - survivor (`1`): written as a UTXO and address-UTXO row, as today;
//! - omitted (`0`): kept in a writer-owned live map, and journaled in the
//!   `spentness_journal` CF in its block's batch, unless its own block
//!   spends it. The map is a bounded cache (256 MiB) in front of the journal.
//!
//! Spends resolve from the same block, then the live map, then the journal:
//! no UTXO CF reads or deletes. Address indexes, balances, value pools and
//! `BlockInfo` stay exact (decision D4). At `H` the writer asserts
//! `resolved == omitted`, records `Complete`, and deletes the journal; a
//! mismatch is a fatal artifact defect.
//!
//! Only omitted outputs enter the map and the journal, so each resolved spend
//! proves that the artifact omits its output. A valid chain never spends an
//! output twice, so when the counts match at `H`, the omitted outputs are
//! exactly the spent outputs, and the survivors are exactly the UTXO set.
//!
//! # Ownership
//!
//! `FinalizedState` is cloned and shared, so the live map and cursor must not
//! live in it. [`SpentnessWriter`] is owned by the block write task (the
//! thread that commits finalized blocks), built there from the hints receiver
//! passed to [`init_with_spentness`](crate::init_with_spentness). No locks.
//!
//! # Recovery
//!
//! The cursor and counts are re-read from the durable record for each block,
//! and journal rows are only deleted at `H`. A block whose batch was not
//! written can be committed again, and a restart resumes from the record,
//! resolving spends of outputs created before the restart from the journal.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::watch;

use zebra_chain::{block, parameters::spentness_hints::VerifiedArtifact, transparent};

use zebra_db::FinalizedBlock;

use crate::service::finalized_state::{
    disk_db::DiskWriteBatch, disk_format::transparent::OutputLocation, ZebraDb,
};

mod apply;
mod live;
mod record;

#[cfg(test)]
mod tests;

use live::LiveOutputs;
use record::{read_progress, Phase};

/// The omitted-output journal: `OutputLocation` → `transparent::Output` (the
/// `utxo_by_out_loc` value format), for every omitted output that its own
/// block does not spend. Rows are written once, never deleted when spent, and
/// range-deleted when construction completes at `H`.
pub const SPENTNESS_JOURNAL: &str = "spentness_journal";

/// The hinted-construction record, keyed by the artifact's SHA-256:
/// `Applying { height, next_ordinal, omitted, resolved }`, then `Complete`.
/// The artifact *bytes* are not stored here: the serving copy is a file
/// registered in the network's `ObjectIndex` by `zebrad`.
pub const SPENTNESS_ARTIFACT: &str = "spentness_artifact";

/// How long a resumed writer waits for its artifact before failing.
///
/// After a restart, the state is opened before the network, and `zebrad`
/// loads the artifact file, or fetches it from peers, after network init. The
/// first block committed after the restart can arrive before that.
pub const RESUME_ARTIFACT_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How often a resumed writer checks the receiver while it waits.
const RESUME_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The receiver `init_with_spentness` takes: `Some` once `zebrad` has a
/// verified artifact. The writer adopts it only while the finalized tip is
/// at most genesis (the state started empty), and only if its terminal
/// height is at most the max checkpoint height; after the first non-genesis
/// commit without hints, later values are ignored. After a restart with an
/// incomplete hinted sync, the writer waits for the same artifact; see
/// [`ReadStateService::has_incomplete_spentness_hints`](crate::ReadStateService::has_incomplete_spentness_hints).
pub type SpentnessHintsReceiver = watch::Receiver<Option<Arc<VerifiedArtifact>>>;

/// The writer-owned hint state: the artifact and the live omitted-output map.
/// The cursor itself is the durable record.
pub struct SpentnessWriter {
    hints: SpentnessHintsReceiver,
    state: WriterState,
    live: LiveOutputs,
    resume_timeout: Duration,
    /// Every hinted block must be checkpoint-verified: the semantic verifier
    /// reads spent outputs from the UTXO set, which omits hinted outputs.
    max_checkpoint_height: block::Height,
}

/// Where the writer is in its lifecycle.
enum WriterState {
    /// The state is empty or holds only genesis, and nothing was hinted yet:
    /// adopt the first artifact sent before block 1 is committed.
    Waiting,

    /// An earlier run left an `Applying` record for the artifact with this
    /// SHA-256: the receiver must deliver that same artifact, within
    /// [`RESUME_ARTIFACT_TIMEOUT`] of the first resumed block.
    Resuming { commitment: [u8; 32] },

    /// The record could not be read: every hinted commit fails with this error.
    Broken(SpentnessError),

    /// Hints apply through the artifact's terminal height.
    Applying(Arc<VerifiedArtifact>),
}

/// The output of [`SpentnessWriter::apply_block`] for one block below `H`,
/// which `write_block` uses instead of reading spent UTXOs from the UTXO CF.
#[derive(Debug, Default)]
pub struct HintedOutputs {
    /// The block's surviving new outputs: the only new outputs written to the
    /// UTXO and address-UTXO CFs.
    pub survivors: BTreeMap<OutputLocation, transparent::Utxo>,

    /// Every transparent input of the block, resolved to its output, in the
    /// same shape `write_block` builds from `read_spent_utxo`.
    pub spent: Vec<(transparent::OutPoint, OutputLocation, transparent::Utxo)>,
}

pub use crate::error::SpentnessError;

impl SpentnessWriter {
    /// Builds the writer state on the write task: resumes an `Applying`
    /// record from `db` (spends of earlier outputs then resolve from the
    /// journal), or waits to adopt the artifact from `hints` while the state
    /// is empty, or returns `None` when hints do not apply (already
    /// `Complete`, or a non-empty state without a record).
    ///
    /// Hints only apply below `max_checkpoint_height`, the height up to which
    /// the node checkpoint-verifies blocks.
    pub fn new(
        db: &ZebraDb,
        hints: SpentnessHintsReceiver,
        max_checkpoint_height: block::Height,
    ) -> Option<Self> {
        let state = match read_progress(db) {
            Err(error) => WriterState::Broken(error),
            Ok(Some((_, progress))) if progress.phase == Phase::Complete => return None,
            Ok(Some((commitment, _))) => WriterState::Resuming { commitment },
            Ok(None)
                if db
                    .finalized_tip_height()
                    .unwrap_or(block::Height(0))
                    .is_min() =>
            {
                WriterState::Waiting
            }
            Ok(None) => return None,
        };

        Some(Self {
            hints,
            state,
            live: LiveOutputs::new(live::LIVE_OUTPUT_BYTES),
            resume_timeout: RESUME_ARTIFACT_TIMEOUT,
            max_checkpoint_height,
        })
    }

    /// Sets how long a resumed writer waits for its artifact.
    #[cfg(test)]
    pub(crate) fn with_resume_timeout(mut self, timeout: Duration) -> Self {
        self.resume_timeout = timeout;
        self
    }

    /// Whether `height` is hinted: at or below the artifact's `H`, and not
    /// yet `Complete`.
    ///
    /// An unadopted artifact applies only to genesis and block 1, and never
    /// when its `H` is above the max checkpoint height. A resumed or broken
    /// record applies to every height, so its block fails with the reason
    /// instead of being written without hints.
    pub fn applies_to(&self, height: block::Height) -> bool {
        match &self.state {
            WriterState::Waiting => {
                height <= block::Height(1)
                    && self.hints.borrow().as_ref().is_some_and(|artifact| {
                        let terminal_height = artifact.header().terminal_height;
                        if terminal_height > self.max_checkpoint_height {
                            tracing::warn!(
                                ?terminal_height,
                                max_checkpoint_height = ?self.max_checkpoint_height,
                                "ignoring spentness hints above the max checkpoint height: \
                                 syncing without hints",
                            );
                            return false;
                        }
                        height <= terminal_height
                    })
            }
            WriterState::Resuming { .. } | WriterState::Broken(_) => true,
            WriterState::Applying(artifact) => height <= artifact.header().terminal_height,
        }
    }

    /// The `write_block` hook, called for each block where
    /// [`applies_to`](Self::applies_to) is true, before the block's batch is
    /// built.
    ///
    /// `new_outputs` is `write_block`'s `new_outputs_by_out_loc`; their
    /// ordinals start at the cursor and follow `OutputLocation` order.
    /// Journal writes and the record update go into `batch`, so they commit
    /// atomically with the block. At `H` the terminal check runs, `Complete`
    /// is written, and the journal is deleted.
    pub fn apply_block(
        &mut self,
        db: &ZebraDb,
        finalized: &FinalizedBlock,
        new_outputs: &BTreeMap<OutputLocation, transparent::Utxo>,
        batch: &mut DiskWriteBatch,
    ) -> Result<HintedOutputs, SpentnessError> {
        let artifact = self.artifact()?;
        apply::apply_block(&artifact, &mut self.live, db, finalized, new_outputs, batch)
    }

    /// The artifact to apply, adopting or resuming it from the receiver.
    fn artifact(&mut self) -> Result<Arc<VerifiedArtifact>, SpentnessError> {
        let artifact = match &self.state {
            WriterState::Applying(artifact) => return Ok(artifact.clone()),
            WriterState::Broken(error) => return Err(error.clone()),
            WriterState::Waiting => {
                self.hints
                    .borrow()
                    .clone()
                    .ok_or(SpentnessError::Inconsistent(
                        "the hints were withdrawn before they were applied",
                    ))?
            }
            WriterState::Resuming { commitment } => {
                let commitment = *commitment;
                self.wait_for_resumed_artifact()
                    .filter(|artifact| artifact.pin().spentness_hash == commitment)
                    .ok_or(SpentnessError::MissingArtifact)?
            }
        };

        let terminal_height = artifact.header().terminal_height;
        if terminal_height > self.max_checkpoint_height {
            let error = SpentnessError::TerminalAboveMaxCheckpoint {
                terminal_height,
                max_checkpoint_height: self.max_checkpoint_height,
            };
            self.state = WriterState::Broken(error.clone());
            return Err(error);
        }

        tracing::info!(
            ?terminal_height,
            output_count = artifact.header().output_count,
            "applying spentness hints in the finalized writer",
        );
        self.state = WriterState::Applying(artifact.clone());

        Ok(artifact)
    }

    /// Waits on this blocking write thread until the receiver delivers an
    /// artifact, its sender is dropped, or the resume timeout expires.
    ///
    /// Polls, because the write task is a plain thread without a runtime, and
    /// `watch` has no blocking receive.
    fn wait_for_resumed_artifact(&self) -> Option<Arc<VerifiedArtifact>> {
        let deadline = Instant::now() + self.resume_timeout;
        let mut logged = false;

        loop {
            if let Some(artifact) = self.hints.borrow().clone() {
                return Some(artifact);
            }
            // A dropped sender can never deliver the artifact.
            if self.hints.has_changed().is_err() || Instant::now() >= deadline {
                return None;
            }
            if !logged {
                tracing::info!(
                    timeout = ?self.resume_timeout,
                    "waiting for the spentness hint artifact to resume an incomplete hinted sync",
                );
                logged = true;
            }
            std::thread::sleep(RESUME_POLL_INTERVAL);
        }
    }
}

/// Returns `true` if `db` has an incomplete hinted construction, or a
/// construction record that cannot be read: the node must then deliver the
/// artifact to [`init_with_spentness`](crate::init_with_spentness) again.
pub fn has_incomplete_spentness_hints(db: &ZebraDb) -> bool {
    match read_progress(db) {
        Ok(None) => false,
        Ok(Some((_, progress))) => progress.phase != Phase::Complete,
        Err(_) => true,
    }
}
