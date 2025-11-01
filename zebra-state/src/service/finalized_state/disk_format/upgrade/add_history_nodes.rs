//! Handle adding history nodes for the latest network upgrade to the database.

use std::{collections::BTreeMap, sync::Arc};

use crossbeam_channel::{Receiver, TryRecvError};

use semver::Version;

use zebra_chain::{
    block::{Block, ChainHistoryBlockTxAuthCommitmentHash, Commitment, Height},
    history_tree::{HistoryTree, NonEmptyHistoryTree},
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::HistoryNodeIndex,
};

use crate::{service::finalized_state::ZebraDb, HashOrHeight};

use super::{super::super::DiskWriteBatch, CancelFormatChange, DiskFormatUpgrade};

/// Implements [`DiskFormatUpgrade`] for adding history tree nodes to the database.
pub struct AddHistoryNodes {
    version: Version,
}

impl AddHistoryNodes {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            version: Version::new(major, minor, patch),
        }
    }
}

impl DiskFormatUpgrade for AddHistoryNodes {
    fn version(&self) -> Version {
        self.version.clone()
    }

    fn description(&self) -> &'static str {
        "add history nodes upgrade"
    }

    /// Runs disk format upgrade for adding history tree nodes to the database.
    ///
    /// The function creates a new history tree for each network upgrade, and
    /// pushes all finalized blocks to it, saving the generated nodes to the
    /// database in append order.
    ///
    /// Returns `Ok` if the upgrade completed, and `Err` if it was cancelled.
    #[allow(clippy::unwrap_in_result)]
    fn run(
        &self,
        initial_tip_height: Height,
        zebra_db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<(), CancelFormatChange> {
        info!("Running history node db upgrade");

        // Clear current data if it exists
        let mut batch_for_delete = DiskWriteBatch::new();
        batch_for_delete.clear_history_nodes(zebra_db);
        zebra_db
            .write_batch(batch_for_delete)
            .expect("unexpected database write failure");

        let history_tree = zebra_db.history_tree();

        if history_tree.is_none() {
            // If the history tree is empty, there is nothing to do
            return Ok(());
        }

        let network = zebra_db.network().clone();
        let upgrades_with_history = upgrades_with_history(&network);

        // Iterate over all network upgrades with history nodes
        for (upgrade, activation_height_option) in upgrades_with_history.iter() {
            info!("Generating history nodes for {}", upgrade);

            // Return early if the upgrade is cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            // We reached a future upgrade, or the upgrade is not available
            if activation_height_option.is_none() {
                break;
            }

            let activation_height = activation_height_option.unwrap();

            // We reached the tip
            if initial_tip_height < activation_height {
                break;
            }

            let mut inner_tree: Option<NonEmptyHistoryTree> = None;

            // Read all blocks before the tip for this network upgrade from db, push them into a new
            // history tree, and store the returned nodes. Finally, write the nodes to the database.
            let last_block_height = upgrade
                .next_upgrade()
                .and_then(|u| u.activation_height(&network).map(|h| h - 1))
                .flatten()
                .unwrap_or(initial_tip_height)
                .min(initial_tip_height);

            let mut batch = DiskWriteBatch::new();
            let mut index = 0u32;
            for h in activation_height.as_usize()..=last_block_height.as_usize() {
                // Return early if the upgrade is cancelled.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                let block = zebra_db
                    .block(HashOrHeight::Height(
                        Height::try_from(h as u32).expect("the value was already a valid height"),
                    ))
                    .unwrap();
                let sapling_root = zebra_db
                    .sapling_tree_by_height(
                        &Height::try_from(h as u32).expect("the value was already a valid height"),
                    )
                    .unwrap()
                    .root();
                let orchard_root = zebra_db
                    .orchard_tree_by_height(
                        &Height::try_from(h as u32).expect("the value was already a valid height"),
                    )
                    .unwrap()
                    .root();

                let nodes = match inner_tree {
                    Some(ref mut t) => t.push(block, &sapling_root, &orchard_root).expect(
                        "pushing already finalized block into new history tree should succeed",
                    ),
                    None => {
                        inner_tree = Some(
                            NonEmptyHistoryTree::from_block(
                                &network,
                                block,
                                &sapling_root,
                                &orchard_root,
                            )
                            .expect("history tree should exist post-Heartwood"),
                        );
                        let t = Option::<NonEmptyHistoryTree>::clone(&inner_tree).unwrap();
                        t.peaks().iter().map(|n| n.1.clone()).collect::<Vec<_>>()
                    }
                };

                nodes.iter().for_each(|node| {
                    batch.write_history_node(
                        zebra_db,
                        HistoryNodeIndex {
                            upgrade: *upgrade,
                            index,
                        },
                        node.clone(),
                    );
                    index += 1;
                });
            }

            info!("Adding {} history nodes for upgrade {:?}", index, upgrade);
            zebra_db
                .write_batch(batch)
                .expect("unexpected database write failure");
        }

        Ok(())
    }

    #[allow(clippy::unwrap_in_result)]
    fn validate(
        &self,
        zebra_db: &ZebraDb,
        cancel_receiver: &Receiver<CancelFormatChange>,
    ) -> Result<Result<(), String>, CancelFormatChange> {
        info!("Checking history nodes database upgrade");

        let network = zebra_db.network().clone();
        let upgrades_with_history = upgrades_with_history(&network);

        let tip_height_option = zebra_db.finalized_tip_height();
        if tip_height_option.is_none() {
            if zebra_db.last_history_node_index().is_some() {
                return Ok(Err(
                    "History nodes must not exist if there is no finalized block".to_owned(),
                ));
            } else {
                // There is nothing to do
                return Ok(Ok(()));
            }
        }

        let height = tip_height_option.expect("Tip height must exist here");

        // Iterate through all blocks from Heartwood activation up to the chain tip.
        // For each block, build the history tree from its peaks, and check the hash
        // against the block header commitment.
        for (upgrade, activation_height_option) in upgrades_with_history.iter() {
            // Return early if the upgrade is cancelled.
            if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                return Err(CancelFormatChange);
            }

            // We reached a future upgrade, or the upgrade is not available
            if activation_height_option.is_none() {
                break;
            }

            let activation_height = activation_height_option.unwrap();

            // We reached the tip
            if height < activation_height {
                break;
            }

            let activation_node = zebra_db
                .history_node(HistoryNodeIndex {
                    upgrade: *upgrade,
                    index: 0,
                })
                .expect("history node should exist");

            let peaks_at_activation = BTreeMap::from([(0, activation_node)]);

            let mut inner_tree: Option<NonEmptyHistoryTree> = Some(
                NonEmptyHistoryTree::from_cache(
                    &network,
                    1,
                    peaks_at_activation,
                    activation_height,
                )
                .expect("activation height history tree should be valid"),
            );

            let last_block_height = upgrade
                .next_upgrade()
                .and_then(|u| u.activation_height(&network).map(|h| h - 1))
                .flatten()
                .unwrap_or(height)
                .min(height);

            for h in activation_height.as_usize()..last_block_height.as_usize() {
                // Return early if the upgrade is cancelled.
                if !matches!(cancel_receiver.try_recv(), Err(TryRecvError::Empty)) {
                    return Err(CancelFormatChange);
                }

                // Get the next block which contains the tree commitment
                let next_block = zebra_db
                    .block(HashOrHeight::Height(
                        Height::try_from(1 + h as u32)
                            .expect("the value was already a valid height"),
                    ))
                    .unwrap();

                // Update the inner tree with the new peaks after activation
                if h > activation_height.as_usize() {
                    inner_tree = update_inner_tree(
                        zebra_db,
                        inner_tree,
                        &network,
                        *upgrade,
                        Height(h as u32),
                    );
                }

                // Compare hashes and return an error if they do not match
                let new_tree = HistoryTree::from(inner_tree.clone());
                if let Err(e) = compare_hashes(
                    &network,
                    *upgrade,
                    Height(h as u32),
                    new_tree.into(),
                    next_block,
                ) {
                    return Ok(Err(e));
                }
            }

            // Compare the next upgrade's activation block commitment with the full
            // history tree root hash of this upgrade. This can only be done if
            // the next network upgrade exists, its height is known and the activation
            // block is available.
            if let Some(next_activation_height) = upgrade
                .next_upgrade()
                .and_then(|upgrade| upgrade.activation_height(&network))
            {
                inner_tree =
                    update_inner_tree(zebra_db, inner_tree, &network, *upgrade, last_block_height);
                let history_tree = HistoryTree::from(inner_tree);
                if let Some(next_activation_block) =
                    zebra_db.block(HashOrHeight::Height(next_activation_height))
                {
                    if let Err(e) = compare_hashes(
                        &network,
                        upgrade
                            .next_upgrade()
                            .expect("next upgrade must exist here"),
                        next_activation_height,
                        history_tree.into(),
                        next_activation_block,
                    ) {
                        return Ok(Err(e));
                    }
                }
            }

            info!("Verified history nodes for {}", upgrade);
        }

        Ok(Ok(()))
    }
}

fn upgrades_with_history(network: &Network) -> [(NetworkUpgrade, Option<Height>); 4] {
    [
        (
            NetworkUpgrade::Heartwood,
            NetworkUpgrade::activation_height(&NetworkUpgrade::Heartwood, network),
        ),
        (
            NetworkUpgrade::Canopy,
            NetworkUpgrade::activation_height(&NetworkUpgrade::Canopy, network),
        ),
        (
            NetworkUpgrade::Nu5,
            NetworkUpgrade::activation_height(&NetworkUpgrade::Nu5, network),
        ),
        (
            NetworkUpgrade::Nu6,
            NetworkUpgrade::activation_height(&NetworkUpgrade::Nu6, network),
        ),
    ]
}

fn update_inner_tree(
    zebra_db: &ZebraDb,
    tree: Option<NonEmptyHistoryTree>,
    network: &Network,
    upgrade: NetworkUpgrade,
    height: Height,
) -> Option<NonEmptyHistoryTree> {
    let history_tree = HistoryTree::from(tree);
    let current_peak_ids = history_tree
        .peaks_at(height)
        .expect("peaks should exist if height is equal or greater than activation");

    let peaks = current_peak_ids
        .iter()
        .map(|id| {
            (
                *id,
                zebra_db
                    .history_node(HistoryNodeIndex {
                        upgrade,
                        index: *id,
                    })
                    .expect("history node should exist"),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let size = history_tree
        .node_count_at(height)
        .expect("height is greater than activation height");

    Some(
        NonEmptyHistoryTree::from_cache(network, size, peaks, height)
            .expect("history tree should be valid"),
    )
}

fn get_hashes(
    network: &Network,
    history_tree: Arc<HistoryTree>,
    block: Arc<Block>,
) -> Result<([u8; 32], [u8; 32]), String> {
    let block_commitment = block
        .commitment(network)
        .expect("block commitment should exist");
    match block_commitment {
        Commitment::ChainHistoryRoot(root_hash) => Ok((
            root_hash.bytes_in_display_order(),
            history_tree
                .hash()
                .expect("tree is not empty")
                .bytes_in_display_order(),
        )),
        Commitment::ChainHistoryBlockTxAuthCommitment(commitment) => {
            let auth_data_root = block.auth_data_root();
            let tree_hash = history_tree.hash().expect("tree is not empty");
            let new_commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
                &tree_hash,
                &auth_data_root,
            );

            Ok((
                commitment.bytes_in_display_order(),
                new_commitment.bytes_in_display_order(),
            ))
        }
        _ => Err("Unexpected commitment type".to_owned()),
    }
}

fn compare_hashes(
    network: &Network,
    upgrade: NetworkUpgrade,
    height: Height,
    history_tree: Arc<HistoryTree>,
    block: Arc<Block>,
) -> Result<(), String> {
    match get_hashes(network, history_tree, block) {
        Ok((expected_hash, new_hash)) => {
            // Compare the hashes
            if expected_hash != new_hash {
                Err(format!(
                    "History tree hash mismatch for {upgrade} with last block at height {height:?}\nExpected: {expected_hash:?}\nCalculated: {new_hash:?}"
                )
                .to_string())
            } else {
                Ok(())
            }
        }
        Err(e) => Err(e),
    }
}
