//! Pluggable watchdog checks.
//!
//! Each check implements [`Check`] and is registered in [`registry`]. New
//! checks only need a new module and a registry entry; the runner and the
//! Sentry reporting layer work on [`CheckOutcome`] values and do not need
//! to change.

use std::collections::BTreeMap;

use crate::config::Config;

pub mod zcashd_compat;

/// Whether a single check cycle passed or failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckStatus {
    /// All of the check's predicates passed.
    Pass,
    /// At least one predicate failed.
    Fail,
}

/// The result of running a check once.
#[derive(Clone, Debug)]
pub struct CheckOutcome {
    /// Pass/fail status of this cycle.
    pub status: CheckStatus,
    /// One-line human-readable summary (the failing predicate when failed).
    pub summary: String,
    /// Structured details attached to logs and Sentry events.
    pub details: BTreeMap<String, String>,
}

impl CheckOutcome {
    /// Returns a passing outcome with the given summary and details.
    pub fn pass(summary: impl Into<String>, details: BTreeMap<String, String>) -> Self {
        Self {
            status: CheckStatus::Pass,
            summary: summary.into(),
            details,
        }
    }

    /// Returns a failing outcome with the given summary and details.
    pub fn fail(summary: impl Into<String>, details: BTreeMap<String, String>) -> Self {
        Self {
            status: CheckStatus::Fail,
            summary: summary.into(),
            details,
        }
    }
}

/// A pluggable watchdog check.
pub trait Check: Send {
    /// A stable, unique name for this check, used in logs and Sentry tags.
    fn name(&self) -> &'static str;

    /// Runs the check once and reports the outcome.
    ///
    /// Checks must bound all of their external waits (RPC requests, process
    /// queries) so a single cycle always terminates.
    fn run_once(&self) -> CheckOutcome;
}

/// Builds the set of enabled checks for this watchdog instance.
///
/// To add a new check: implement [`Check`] in a new module under
/// `src/checks/` and push it onto the returned list here.
pub fn registry(config: &Config) -> Vec<Box<dyn Check>> {
    vec![Box::new(zcashd_compat::ZcashdCompatSyncCheck::new(config))]
}
