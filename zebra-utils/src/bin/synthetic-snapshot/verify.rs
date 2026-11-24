//! Standalone installed-state verification.

use std::{io, path::Path};

use zebra_chain::state_snapshot::StateSnapshotCommitment;
use zebra_state::{
    state_snapshot::{
        compare::{compare_states, ComparisonSummary},
        format::window_start,
        SnapshotBase,
    },
    BoxError, Config, MAX_BLOCK_REORG_HEIGHT,
};

use super::config::Prepared;

pub(super) fn expected_snapshot_base(commitment: StateSnapshotCommitment) -> SnapshotBase {
    SnapshotBase {
        window_start: window_start(commitment.height),
        height: commitment.height,
        block_hash: commitment.block_hash,
        manifest: commitment.manifest,
        // H is a checkpoint and the serving fixture lacks the body at H + 1.
        deferred_checks: SnapshotBase::DEFERRED_SUCCESSOR_COMMITMENTS,
    }
}

/// Compares prepared A against a stopped state installed at `state`.
///
/// Both entry points read the installed cache the same way: closed and under
/// nothing but the state lock. Sharing this adapter keeps `run` and `verify`
/// from drifting apart.
pub(crate) fn compare_installed(
    prepared: &Prepared,
    state: &Path,
) -> Result<ComparisonSummary, BoxError> {
    let imported = Config {
        cache_dir: state.to_path_buf(),
        prune_blocks_older_than: Some(MAX_BLOCK_REORG_HEIGHT + 1),
        ..Config::default()
    };
    let resolved = prepared
        .config
        .resolved
        .as_ref()
        .expect("admission requires a resolved commitment");
    compare_states(
        &prepared.source,
        &imported,
        &prepared.network,
        expected_snapshot_base(resolved.commitment),
    )
    .map_err(|error| format!("installed-state comparison: {error}").into())
}

/// Reads B independently under its primary database lock; A can remain serving.
///
/// Callers must first shut B down and wait for its process to exit. The database
/// lock additionally rejects an accidentally live or reopened B. No bootstrap
/// subprocess is launched by this function.
pub(crate) fn command(config: &Path, state: &Path) -> Result<(), BoxError> {
    let prepared = Prepared::load(config)?;
    let comparison = compare_installed(&prepared, state)?;
    serde_json::to_writer_pretty(io::stdout().lock(), &comparison)?;
    println!();
    eprintln!(
        "verified stopped state {} at height {} against prepared A",
        state.display(),
        prepared.config.height.0
    );
    Ok(())
}
