//! The stages of a state snapshot bootstrap, as structured log lines.
//!
//! Each stage is logged once, at info, as a line that starts with the stage's
//! [`Display`](fmt::Display) text, so tests and the benchmark harness can
//! match the lines exactly. The bootstrap logs the stages up to the renamed
//! state; `zebrad start` logs the state initialization; and
//! [`log_runtime_stages`] logs the stages the sync reaches afterwards.

use std::fmt;

use zebra_chain::block::Height;
use zebra_state::{ChainTipChange, TipAction};

/// A stage of a state snapshot bootstrap, in the order the stages happen.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Stage {
    /// The decision rule chose to bootstrap.
    BootstrapStarted,
    /// The bootstrap peers are connected.
    PeersReady,
    /// The manifest hashes to the commitment and matches it.
    ManifestVerified,
    /// The first piece hashed to its manifest entry.
    FirstPieceVerified,
    /// Every piece hashed to its manifest entry.
    LastPieceVerified,
    /// Every piece has been built into an SST file.
    SstsBuilt,
    /// Every SST file has been ingested into the new database.
    SstsIngested,
    /// The self-consistency and chain checks passed.
    ChecksDone,
    /// The new database was renamed into place.
    StateRenamed,
    /// The state service started on the imported database.
    StateInitialized,
    /// The first block above the snapshot height was committed.
    FirstCommitAboveSnapshot,
    /// Block `H + 1` was committed, so the snapshot's history tree and tree
    /// roots are verified against the chain.
    SuccessorCommitted,
    /// The last checkpoint was committed.
    LastCheckpointReached,
    /// The configured sync target was reached.
    TargetReached,
}

impl Stage {
    /// Every stage, in order.
    pub const ALL: [Stage; 14] = [
        Stage::BootstrapStarted,
        Stage::PeersReady,
        Stage::ManifestVerified,
        Stage::FirstPieceVerified,
        Stage::LastPieceVerified,
        Stage::SstsBuilt,
        Stage::SstsIngested,
        Stage::ChecksDone,
        Stage::StateRenamed,
        Stage::StateInitialized,
        Stage::FirstCommitAboveSnapshot,
        Stage::SuccessorCommitted,
        Stage::LastCheckpointReached,
        Stage::TargetReached,
    ];

    /// The prefix every stage's log line starts with.
    pub const LOG_PREFIX: &'static str = "state snapshot stage:";
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Stage::BootstrapStarted => "bootstrap started",
            Stage::PeersReady => "bootstrap peers ready",
            Stage::ManifestVerified => "manifest verified",
            Stage::FirstPieceVerified => "first piece verified",
            Stage::LastPieceVerified => "last piece verified",
            Stage::SstsBuilt => "SST files built",
            Stage::SstsIngested => "SST files ingested",
            Stage::ChecksDone => "import checks done",
            Stage::StateRenamed => "state renamed into place",
            Stage::StateInitialized => "state initialized",
            Stage::FirstCommitAboveSnapshot => "first commit above the snapshot height",
            Stage::SuccessorCommitted => "block after the snapshot height committed",
            Stage::LastCheckpointReached => "last checkpoint reached",
            Stage::TargetReached => "sync target reached",
        };
        // Every production use of a stage's display text reports that stage. Keeping the counter
        // here covers the bootstrap, start, and runtime paths without duplicating the metric name.
        metrics::counter!("sync.state_snapshot.stages.total", "stage" => name).increment(1);
        write!(f, "{} {name}", Self::LOG_PREFIX)
    }
}

/// The stages a node reaches after a snapshot was installed, each logged once
/// when the chain tip first reaches its height.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeStages {
    /// The stages not reached yet, with the tip height that reaches each.
    pending: Vec<(Stage, Height)>,
}

impl RuntimeStages {
    /// Returns the stages above the snapshot height `snapshot_height`: the
    /// first commit above it and block `H + 1` (the same height), the last
    /// checkpoint `max_checkpoint_height`, and the `target_height` from
    /// `state.debug_stop_at_height`, if set.
    pub fn new(
        snapshot_height: Height,
        max_checkpoint_height: Height,
        target_height: Option<Height>,
    ) -> Self {
        let mut pending = Vec::new();
        if let Some(successor) = snapshot_height + 1 {
            pending.push((Stage::FirstCommitAboveSnapshot, successor));
            pending.push((Stage::SuccessorCommitted, successor));
        }
        if max_checkpoint_height > snapshot_height {
            pending.push((Stage::LastCheckpointReached, max_checkpoint_height));
        }
        if let Some(target) = target_height.filter(|target| *target > snapshot_height) {
            pending.push((Stage::TargetReached, target));
        }
        RuntimeStages { pending }
    }

    /// Records that the chain tip reached `height`, and returns the stages that
    /// height reaches for the first time, in order.
    pub fn reached(&mut self, height: Height) -> Vec<Stage> {
        let mut reached = Vec::new();
        self.pending.retain(|(stage, at)| {
            if height >= *at {
                reached.push(*stage);
                false
            } else {
                true
            }
        });
        reached
    }

    /// Returns whether every stage has been reached.
    pub fn is_done(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Logs each of `stages` once, when the chain tip first reaches its height,
/// and returns when every stage is logged or the chain tip channel closes.
pub async fn log_runtime_stages(mut stages: RuntimeStages, mut chain_tip_change: ChainTipChange) {
    while !stages.is_done() {
        let tip_height = match chain_tip_change.wait_for_tip_change().await {
            Ok(TipAction::Grow { block }) => block.height,
            Ok(TipAction::Reset { height, .. }) => height,
            Err(_closed) => return,
        };
        for stage in stages.reached(tip_height) {
            info!(tip_height = tip_height.0, "{stage}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    use zebra_chain::block::Height;

    use super::{RuntimeStages, Stage};

    #[derive(Default)]
    struct StageRecorder {
        value: Arc<AtomicU64>,
    }

    impl metrics::Recorder for StageRecorder {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }

        fn register_counter(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            assert_eq!(key.name(), "sync.state_snapshot.stages.total");
            let labels: Vec<&metrics::Label> = key.labels().collect();
            assert_eq!(labels.len(), 1);
            assert_eq!(labels[0].key(), "stage");
            assert_eq!(labels[0].value(), "bootstrap started");
            metrics::Counter::from_arc(self.value.clone())
        }

        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }

        fn register_histogram(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::noop()
        }
    }

    /// Each runtime stage is reported once, when the tip first reaches its
    /// height, in stage order, even when one tip change passes several.
    #[test]
    fn runtime_stages_are_reached_once_in_order() {
        let _init_guard = zebra_test::init();

        let mut stages = RuntimeStages::new(Height(10), Height(20), Some(Height(30)));
        assert!(!stages.is_done());
        assert_eq!(stages.reached(Height(10)), vec![]);
        assert_eq!(
            stages.reached(Height(11)),
            vec![Stage::FirstCommitAboveSnapshot, Stage::SuccessorCommitted]
        );
        assert_eq!(stages.reached(Height(19)), vec![]);
        assert_eq!(
            stages.reached(Height(20)),
            vec![Stage::LastCheckpointReached]
        );
        assert_eq!(stages.reached(Height(20)), vec![]);
        assert_eq!(stages.reached(Height(35)), vec![Stage::TargetReached]);
        assert!(stages.is_done());

        let mut stages = RuntimeStages::new(Height(10), Height(20), None);
        assert_eq!(
            stages.reached(Height(25)),
            vec![
                Stage::FirstCommitAboveSnapshot,
                Stage::SuccessorCommitted,
                Stage::LastCheckpointReached,
            ]
        );
        assert!(stages.is_done());

        // A target at or below the snapshot height is already reached, and a last checkpoint
        // at the snapshot height is not a stage above it.
        let stages = RuntimeStages::new(Height(10), Height(10), Some(Height(10)));
        assert_eq!(
            stages,
            RuntimeStages::new(Height(10), Height(10), None),
            "the target is dropped"
        );
        let mut stages = stages;
        assert_eq!(
            stages.reached(Height(11)),
            vec![Stage::FirstCommitAboveSnapshot, Stage::SuccessorCommitted]
        );
        assert!(stages.is_done());
    }

    /// The log lines are distinct and share the prefix the tests match on.
    #[test]
    fn stage_log_lines_are_distinct_and_prefixed() {
        let _init_guard = zebra_test::init();

        let lines: Vec<String> = Stage::ALL.iter().map(ToString::to_string).collect();
        for line in &lines {
            assert!(line.starts_with(Stage::LOG_PREFIX), "{line}");
        }
        let distinct: std::collections::HashSet<&String> = lines.iter().collect();
        assert_eq!(distinct.len(), lines.len(), "{lines:?}");
        assert_eq!(
            Stage::BootstrapStarted.to_string(),
            "state snapshot stage: bootstrap started"
        );
    }

    #[test]
    fn stage_reporting_records_its_metric() {
        let _init_guard = zebra_test::init();
        let recorder = StageRecorder::default();

        metrics::with_local_recorder(&recorder, || {
            assert_eq!(
                Stage::BootstrapStarted.to_string(),
                "state snapshot stage: bootstrap started"
            );
        });

        assert_eq!(recorder.value.load(Ordering::Relaxed), 1);
    }
}
