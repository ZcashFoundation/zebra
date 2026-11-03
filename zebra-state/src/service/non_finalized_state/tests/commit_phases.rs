//! Tests for the block writer's commit sub-phase timers.

use std::{
    collections::BTreeMap,
    sync::{mpsc, Arc},
};

use metrics::{
    Counter, Gauge, Histogram, HistogramFn, Key, KeyName, Metadata, Recorder, SharedString, Unit,
};

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto};

use crate::{
    arbitrary::Prepare,
    tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
};

/// The phases that the non-finalized state records for each committed block.
pub(super) const COMMIT_PHASES: [&str; 7] = [
    "transparent_spend",
    "anchor_reads",
    "pickup",
    "chain_clone",
    "trees",
    "chain_updates",
    "history",
];

/// Sends each block writer phase sample to a channel, with its phase label.
pub(super) struct PhaseRecorder(mpsc::Sender<(String, f64)>);

impl PhaseRecorder {
    /// Returns a recorder, and the receiver for its phase samples.
    pub(super) fn new() -> (Self, mpsc::Receiver<(String, f64)>) {
        let (sender, receiver) = mpsc::channel();
        (Self(sender), receiver)
    }
}

/// One phase label's histogram handle.
struct PhaseHistogram {
    phase: String,
    samples: mpsc::Sender<(String, f64)>,
}

impl HistogramFn for PhaseHistogram {
    fn record(&self, value: f64) {
        // The receiver outlives every recording, so a send error is a test bug.
        self.samples
            .send((self.phase.clone(), value))
            .expect("the test keeps the receiver until recording finishes");
    }
}

impl Recorder for PhaseRecorder {
    fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
    fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

    fn register_counter(&self, _: &Key, _: &Metadata<'_>) -> Counter {
        Counter::noop()
    }

    fn register_gauge(&self, _: &Key, _: &Metadata<'_>) -> Gauge {
        Gauge::noop()
    }

    fn register_histogram(&self, key: &Key, _: &Metadata<'_>) -> Histogram {
        if key.name() != "state.block_writer.phase.duration_seconds" {
            return Histogram::noop();
        }

        let phase = key
            .labels()
            .find(|label| label.key() == "phase")
            .map(|label| label.value().to_string())
            .expect("every block writer phase sample has a phase label");

        Histogram::from_arc(Arc::new(PhaseHistogram {
            phase,
            samples: self.0.clone(),
        }))
    }
}

/// Returns the samples received so far, in recording order, grouped by phase.
pub(super) fn samples_by_phase(
    receiver: &mpsc::Receiver<(String, f64)>,
) -> BTreeMap<String, Vec<f64>> {
    let mut samples: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for (phase, value) in receiver.try_iter() {
        samples.entry(phase).or_default().push(value);
    }
    samples
}

/// Each block commit records every sub-phase once, on the committing thread, and a state that
/// doesn't count metrics records none.
#[test]
fn commit_records_each_sub_phase_once_per_committed_block() {
    let _init_guard = zebra_test::init();

    let (finalized, mut state, _genesis) = new_state_with_mainnet_genesis();
    let block1: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .expect("block 1 test vector is valid");

    let (recorder, samples) = PhaseRecorder::new();
    let committed_blocks = 5;

    // The jobs that measure the clone, trees and chain updates run on rayon threads, so this only
    // passes if their durations are recorded on the committing thread.
    metrics::with_local_recorder(&recorder, || {
        state
            .commit_new_chain(block1.clone().prepare(), &finalized.db)
            .expect("block 1 is a valid child of genesis");

        let mut parent = block1;
        for _ in 1..committed_blocks {
            parent = parent.make_fake_child();
            state
                .commit_block(parent.clone().prepare(), &finalized.db)
                .expect("fake children of block 1 are valid");
        }
    });

    let recorded = samples_by_phase(&samples);
    for phase in COMMIT_PHASES {
        assert_eq!(
            recorded.get(phase).map(Vec::len),
            Some(committed_blocks),
            "phase {phase} must be recorded once per committed block: {recorded:?}",
        );
    }
    assert_eq!(
        recorded.len(),
        COMMIT_PHASES.len(),
        "the non-finalized state records only the commit sub-phases: {recorded:?}",
    );

    // Block proposals are checked against a clone that doesn't count metrics.
    let mut proposal_state = state.clone();
    proposal_state.disable_metrics();
    let proposal = state
        .best_tip_block()
        .expect("the state has a tip")
        .block
        .make_fake_child();

    metrics::with_local_recorder(&recorder, || {
        proposal_state
            .commit_block(proposal.prepare(), &finalized.db)
            .expect("a fake child of the tip is valid");
    });

    assert_eq!(
        samples_by_phase(&samples),
        BTreeMap::new(),
        "a state that doesn't count metrics must not record commit phases",
    );
}
