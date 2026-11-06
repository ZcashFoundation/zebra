//! Copies a fully validated archive state into a separate state that ends exactly at H.
//!
//! The source must have validated its chain. This copy does not repeat signature,
//! proof, or contextual consensus checks: it only rebuilds the indexes, so that the
//! destination's UTXO set is the UTXO set at H.

use std::{fs, path::Path};

use color_eyre::eyre::{ensure, eyre, Result};
use zebra_chain::{
    block::{self, Height},
    parameters::Network,
};
use zebra_state::FinalizedState;

use super::{exact_boundary, for_each_block, open, state_config};

/// Commits the source's blocks into `destination` until its tip is `height`/`hash`.
///
/// A destination that already holds a prefix of the source chain resumes after its tip.
/// The source is never modified.
pub(super) fn replay(
    network: &Network,
    source: &Path,
    destination: &Path,
    height: Height,
    hash: block::Hash,
) -> Result<()> {
    fs::create_dir_all(destination)?;
    ensure!(
        fs::canonicalize(source)? != fs::canonicalize(destination)?,
        "replay destination must differ from its source"
    );
    let source = open(source, network)?;
    ensure!(
        source.hash(height) == Some(hash),
        "source does not contain the selected height and hash"
    );

    let mut state = FinalizedState::new(
        &state_config(destination),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )?;
    let start = match state.db.tip() {
        None => 0,
        Some((tip, tip_hash)) => {
            ensure!(
                tip <= height && source.hash(tip) == Some(tip_hash),
                "replay destination has an incompatible tip; choose a fresh destination"
            );
            tip.0
                .checked_add(1)
                .ok_or_else(|| eyre!("replay height overflow"))?
        }
    };

    let mut trees = None;
    for_each_block(&source, start..=height.0, "replayed", |canonical| {
        let (_, next_trees) = state.commit_finalized_direct(
            canonical.block.into(),
            trees.take(),
            "spentness archive replay",
        )?;
        trees = Some(next_trees);
        Ok(())
    })?;
    exact_boundary(&state.db, network, height, hash)?;
    Ok(())
}
