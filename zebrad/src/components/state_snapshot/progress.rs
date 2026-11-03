//! The stages of a state snapshot bootstrap, as logged (design, §10
//! "Structured stage logs").
//!
//! Each stage is logged once, at info, as a line that starts with the stage's
//! [`Display`](fmt::Display) text, so tests and the benchmark harness can
//! match the lines exactly.

use std::fmt;

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
        write!(f, "{} {name}", Self::LOG_PREFIX)
    }
}

#[cfg(test)]
mod tests {
    use super::Stage;

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
}
