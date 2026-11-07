//! Building a finalized database from verified snapshot pieces.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use zebra_chain::{
    block,
    parameters::Network,
    state_snapshot::{ObjectHash, StateSnapshotCommitment},
};

pub use zebra_db::state_snapshot::import::*;

use crate::{service::check::check_header_context, Config};

use super::{format::Manifest, SnapshotBase, SnapshotError};

#[cfg(test)]
mod tests;

/// Imports a state snapshot, checking the headers above the snapshot height
/// with the state's contextual header check.
///
/// See [`zebra_db::state_snapshot::import::import_state_snapshot`].
#[allow(clippy::too_many_arguments)]
pub fn import_state_snapshot(
    temp_root: &std::path::Path,
    manifest: &Manifest,
    commitment: StateSnapshotCommitment,
    deferred_checks: u8,
    headers_above: &[Arc<block::Header>],
    network: &Network,
    config: &Config,
    pieces: &HashMap<ObjectHash, PathBuf>,
) -> Result<SnapshotBase, SnapshotError> {
    zebra_db::state_snapshot::import::import_state_snapshot(
        temp_root,
        manifest,
        commitment,
        deferred_checks,
        headers_above,
        check_header_context,
        network,
        config,
        pieces,
    )
}
