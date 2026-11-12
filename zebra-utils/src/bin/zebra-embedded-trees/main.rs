//! Writes the Sapling, Orchard, and Ironwood note commitment trees after a block, and the subtrees
//! they completed, from a Zebra state database, in the format Zebra embeds for fast checkpoint
//! sync.
//!
//! For usage please refer to the program help: `zebra-embedded-trees --help`

use std::{collections::BTreeMap, path::PathBuf};

use clap::Parser;
use color_eyre::eyre::{bail, OptionExt, Result};

use zebra_chain::{
    block::Height,
    parameters::Network,
    subtree::{
        NoteCommitmentSubtree, NoteCommitmentSubtreeData, NoteCommitmentSubtreeIndex,
        TRACKED_SUBTREE_HEIGHT,
    },
};
use zebra_state::{EmbeddedTrees, ZebraDb};
use zebra_utils::init_tracing;

#[cfg(test)]
mod tests;

/// zebra-embedded-trees arguments
#[derive(Clone, Debug, Parser)]
#[command(version)]
struct Args {
    /// The zebrad state cache directory, which contains `state/v<N>/<network>`.
    #[arg(long)]
    cache_dir: PathBuf,

    /// The network of the state: `mainnet` or `testnet`.
    #[arg(long)]
    network: Network,

    /// The height of the block after which to take the trees.
    #[arg(long)]
    height: u32,

    /// The file to write the trees to.
    #[arg(long)]
    output: PathBuf,
}

/// Process entry point for `zebra-embedded-trees`
#[allow(clippy::print_stdout)]
fn main() -> Result<()> {
    init_tracing();
    color_eyre::install()?;

    let args = Args::parse();
    let config = zebra_state::Config {
        cache_dir: args.cache_dir,
        ..Default::default()
    };

    // A read-only secondary instance doesn't write to the source database.
    let (_, db, _) = zebra_state::init_read_only(config, &args.network)?;
    let trees = embedded_trees(&db, Height(args.height))?;

    std::fs::write(&args.output, trees.to_bytes())?;

    println!(
        "height {}: Sapling size {}, {} subtrees, {:?}; Orchard size {}, {} subtrees, {:?}; \
         Ironwood size {}, {} subtrees, {:?}",
        trees.height.0,
        trees.sapling.count(),
        trees.sapling_subtrees.len(),
        trees.sapling.root(),
        trees.orchard.count(),
        trees.orchard_subtrees.len(),
        trees.orchard.root(),
        trees.ironwood.count(),
        trees.ironwood_subtrees.len(),
        trees.ironwood.root(),
    );

    Ok(())
}

/// Returns the note commitment trees after the finalized block at `height` in `db`, and the
/// subtrees they completed.
fn embedded_trees(db: &ZebraDb, height: Height) -> Result<EmbeddedTrees> {
    let tip = db.finalized_tip_height().ok_or_eyre("the state is empty")?;
    if height > tip {
        bail!("height {} is above the finalized tip {}", height.0, tip.0);
    }

    // The database has no trees for these heights, and reading them would return older trees.
    if let Some(heights) = db.fast_sync_tree_heights() {
        if heights.contains(&height) {
            bail!(
                "height {} was committed by fast sync without its trees, from {} to {}",
                height.0,
                heights.start().0,
                heights.end().0,
            );
        }
    }

    let tree_exists = "finalized heights have trees";
    let sapling = db.sapling_tree_by_height(&height).expect(tree_exists);
    let orchard = db.orchard_tree_by_height(&height).expect(tree_exists);
    let ironwood = db.ironwood_tree_by_height(&height).expect(tree_exists);

    Ok(EmbeddedTrees {
        height,
        sapling_subtrees: completed_subtrees(
            "Sapling",
            db.sapling_subtree_list_by_index_range(..),
            height,
            sapling.count(),
        )?,
        orchard_subtrees: completed_subtrees(
            "Orchard",
            db.orchard_subtree_list_by_index_range(..),
            height,
            orchard.count(),
        )?,
        ironwood_subtrees: completed_subtrees(
            "Ironwood",
            db.ironwood_subtree_list_by_index_range(..),
            height,
            ironwood.count(),
        )?,
        sapling,
        orchard,
        ironwood,
    })
}

/// Returns the `subtrees` completed at or below `height`, and checks that they are the subtrees a
/// tree with `size` leaves completed: one for each full subtree, in index order.
fn completed_subtrees<Node>(
    pool: &str,
    subtrees: BTreeMap<NoteCommitmentSubtreeIndex, NoteCommitmentSubtreeData<Node>>,
    height: Height,
    size: u64,
) -> Result<Vec<NoteCommitmentSubtree<Node>>> {
    let subtrees: Vec<_> = subtrees
        .into_iter()
        .filter(|(_, data)| data.end_height <= height)
        .map(|(index, data)| data.with_index(index))
        .collect();

    let completed = size >> TRACKED_SUBTREE_HEIGHT;
    if !subtrees
        .iter()
        .map(|subtree| u64::from(subtree.index.0))
        .eq(0..completed)
    {
        bail!(
            "the {pool} tree at height {} has {size} leaves, so it completed {completed} \
             subtrees, but the database has {} subtrees up to that height, or they are not \
             indexed in order",
            height.0,
            subtrees.len(),
        );
    }

    Ok(subtrees)
}
