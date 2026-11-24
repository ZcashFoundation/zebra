//! Direct run output. Harness durations are monotonic; node intervals use log timestamps.

use std::{
    fmt::Display,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use zebra_chain::parameters::Network;
use zebra_state::{
    constants::STATE_DATABASE_KIND, state_database_format_version_in_code,
    state_snapshot::compare::ComparisonSummary, BoxError, Config,
};

use super::{config::TrialConfig, stages::StageTimings};

#[derive(Serialize)]
pub(crate) struct Failure {
    pub stage: &'static str,
    pub cause: String,
}

impl Failure {
    pub(crate) fn new(stage: &'static str, error: impl Display) -> Self {
        Self {
            stage,
            cause: error.to_string(),
        }
    }
}

#[derive(Default, Serialize)]
pub(crate) struct RunTimings {
    pub admission: f64,
    pub output_setup: Option<f64>,
    pub prepared_sizes: Option<f64>,
    pub source_fixture: Option<f64>,
    pub source_startup: Option<f64>,
    pub source_shutdown: Option<f64>,
    pub serving_size: Option<f64>,
    pub cleanup: Option<f64>,
    pub total: f64,
}

#[derive(Default, Serialize)]
pub(crate) struct TrialTimings {
    pub bootstrap: Option<f64>,
    pub liveness: Option<f64>,
    pub shutdown: Option<f64>,
    pub comparison: Option<f64>,
    pub comparison_output: Option<f64>,
    pub state_size: Option<f64>,
    pub total: f64,
}

#[derive(Serialize)]
pub(crate) struct TrialReport {
    pub trial: u32,
    pub state: PathBuf,
    pub passed: bool,
    pub failures: Vec<Failure>,
    pub timings_seconds: TrialTimings,
    pub bootstrap_stages_seconds: StageTimings,
    pub state_bytes: Option<u64>,
    pub comparison: Option<ComparisonSummary>,
}

#[derive(Serialize)]
pub(crate) struct RunReport<'a> {
    pub config_path: &'a Path,
    pub configuration: Option<&'a TrialConfig>,
    pub prepared_state: Option<&'a Path>,
    pub zebrad: &'a Path,
    pub source_zebrad: &'a Path,
    pub output: &'a Path,
    pub passed: bool,
    pub failures: Vec<Failure>,
    pub timings_seconds: RunTimings,
    pub snapshot_bytes: Option<u64>,
    pub prepared_state_bytes: Option<u64>,
    pub serving_state_bytes: Option<u64>,
    pub trials: Vec<TrialReport>,
}

/// Measure only the finalized database, excluding snapshots, logs and secondary caches.
/// A database not created by a failed startup is unavailable, rather than zero bytes.
pub(crate) fn state_bytes(config: &Config, network: &Network) -> Result<Option<u64>, BoxError> {
    let path = config.db_path(
        STATE_DATABASE_KIND,
        state_database_format_version_in_code().major,
        network,
    );
    if !path.try_exists()? {
        return Ok(None);
    }
    let mut total = 0u64;
    let mut directories = vec![path];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if kind.is_file() {
                total = total
                    .checked_add(entry.metadata()?.len())
                    .ok_or("state size overflow")?;
            } else {
                return Err(format!(
                    "cannot measure non-regular state entry {}",
                    entry.path().display()
                )
                .into());
            }
        }
    }
    Ok(Some(total))
}
