//! Checks for whether cited anchors are previously-computed note commitment
//! tree roots.

use std::collections::HashMap;

use zebra_chain::sprout;

use crate::{
    service::{finalized_state::FinalizedState, non_finalized_state::Chain},
    PreparedBlock, ValidateContextError,
};

/// Checks that the Sprout, Sapling, and Orchard anchors specified by
/// transactions in this block have been computed previously within the context
/// of its parent chain. We do not check any anchors in checkpointed blocks,
/// which avoids JoinSplits<BCTV14Proof>
///
/// Sprout anchors may refer to some earlier block's final treestate (like
 /// Sapling and Orchard do exclusively) _or_ to the interstitial output
/// treestate of any prior `JoinSplit` _within the same transaction_.
#[tracing::instrument(skip(finalized_state, parent_chain, prepared))]
pub(crate) fn anchors_refer_to_earlier_treestates(
    finalized_state: &FinalizedState,
    parent_chain: &Chain,
    prepared: &PreparedBlock,
) -> Result<(), ValidateContextError> {
    for transaction in prepared.block.transactions.iter() {
        // Sprout JoinSplits, with interstitial treestates to check as well.
        if transaction.has_sprout_joinsplit_data() {
            let mut interstitial_trees: HashMap<
                sprout::tree::Root,
                sprout::tree::NoteCommitmentTree,
            > = HashMap::new();

            for joinsplit in transaction.sprout_groth16_joinsplits() {
                // Check all anchor sets, including the one for interstitial
                // anchors.
                //
                // The anchor is checked and the matching tree is obtained,
                // which is used to create the interstitial tree state for this
                // JoinSplit:
                //
                // > For each JoinSplit description in a transaction, an
                // > interstitial output treestate is constructed which adds the
                // > note commitments and nullifiers specified in that JoinSplit
                // > description to the input treestate referred to by its
                // > anchor. This interstitial output treestate is available for
                // > use as the anchor of subsequent JoinSplit descriptions in
                // > the same transaction.
                //
                // <https://zips.z.cash/protocol/protocol.pdf#joinsplit>
                //
                // # Consensus
                //
                // > The anchor of each JoinSplit description in a transaction
                // > MUST refer to either some earlier block’s final Sprout
                // > treestate, or to the interstitial output treestate of any
                // > prior JoinSplit description in the same transaction.
                //
                // > For the first JoinSplit description of a transaction, the
                // > anchor MUST be the output Sprout treestate of a previous
                // > block.
                //
                // <https://zips.z.cash/protocol/protocol.pdf#joinsplit>
                //
                // Note that in order to satisfy the latter consensus rule above,
                // [`interstitial_trees`] is always empty in the first iteration
                // of the loop.
                let input_tree = interstitial_trees
                    .get(&joinsplit.anchor)
                    .cloned()
                    .or_else(|| {
                        parent_chain
                            .sprout_trees_by_anchor
                            .get(&joinsplit.anchor)
                            .cloned()
                            .or_else(|| {
                                finalized_state
                                    .sprout_note_commitment_tree_by_anchor(&joinsplit.anchor)
                            })
                    });

                let mut input_tree = match input_tree {
                    Some(tree) => tree,
                    None => {
                        tracing::warn!(?joinsplit.anchor, ?prepared.height, ?prepared.hash, "failed to find sprout anchor");
                        return Err(ValidateContextError::UnknownSproutAnchor {
                            anchor: joinsplit.anchor,
                        });
                    }
                };

                tracing::debug!(?joinsplit.anchor, "validated sprout anchor");

                // Add new anchors to the interstitial note commitment tree.
                for cm in joinsplit.commitments {
                    input_tree
                        .append(cm)
                        .expect("note commitment should be appendable to the tree");
                }

                interstitial_trees.insert(input_tree.root(), input_tree);

                tracing::debug!(?joinsplit.anchor, "observed sprout anchor");
            }
        }

        // Sapling Spends
        //
        // MUST refer to some earlier block’s final Sapling treestate.
        //
        // # Consensus
        //
        // > The anchor of each Spend description MUST refer to some earlier
        // > block’s final Sapling treestate. The anchor is encoded separately
        // > in each Spend description for v4 transactions, or encoded once and
        // > shared between all Spend descriptions in a v5 transaction.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#spendsandoutputs>
        //
        // This rule is also implemented in
        // [`zebra_chain::sapling::shielded_data`].
        //
        // The "earlier treestate" check is implemented here.
        if transaction.has_sapling_shielded_data() {
            for anchor in transaction.sapling_anchors() {
                tracing::debug!(?anchor, "observed sapling anchor");

                if !parent_chain.sapling_anchors.contains(&anchor)
                    && !finalized_state.contains_sapling_anchor(&anchor)
                {
                    return Err(ValidateContextError::UnknownSaplingAnchor { anchor });
                }

                tracing::debug!(?anchor, "validated sapling anchor");
            }
        }

        // Orchard Actions
        //
        // MUST refer to some earlier block’s final Orchard treestate.
        //
        // # Consensus
        //
        // > The anchorOrchard field of the transaction, whenever it exists
        // > (i.e. when there are any Action descriptions), MUST refer to some
        // > earlier block’s final Orchard treestate.
        //
        // <https://zips.z.cash/protocol/protocol.pdf#actions>
        if let Some(orchard_shielded_data) = transaction.orchard_shielded_data() {
            tracing::debug!(?orchard_shielded_data.shared_anchor, "observed orchard anchor");

            if !parent_chain
                .orchard_anchors
                .contains(&orchard_shielded_data.shared_anchor)
                && !finalized_state.contains_orchard_anchor(&orchard_shielded_data.shared_anchor)
            {
                return Err(ValidateContextError::UnknownOrchardAnchor {
                    anchor: orchard_shielded_data.shared_anchor,
                });
            }

            tracing::debug!(?orchard_shielded_data.shared_anchor, "validated orchard anchor");
        }
    }

    Ok(())
}
