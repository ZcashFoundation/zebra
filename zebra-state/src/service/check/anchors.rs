//! Checks for whether cited anchors are previously-computed note commitment
//! tree roots.

use std::collections::HashSet;

use zebra_chain::sprout;

use crate::{
    service::{finalized_state::FinalizedState, non_finalized_state::Chain},
    PreparedBlock, ValidateContextError,
};

/// Check that the Sprout, Sapling, and Orchard anchors specified by
/// transactions in this block have been computed previously within the context
/// of its parent chain. We do not check any anchors in `JoinSplit`s on BCTV14.
///
/// Sprout anchors may refer to some earlier block's final treestate (like
/// Sapling and Orchard do exclusively) _or_ to the interstisial output
/// treestate of any prior `JoinSplit` _within the same transaction_.
///
/// > For the first JoinSplit description of a transaction, the anchor MUST be
/// > the output Sprout treestate of a previous block.[^sprout]
///
/// > The anchor of each JoinSplit description in a transaction MUST refer to
/// > either some earlier block’s final Sprout treestate, or to the interstitial
/// > output treestate of any prior JoinSplit description in the same transaction.[^sprout]
///
/// > The anchor of each Spend description MUST refer to some earlier
/// > block’s final Sapling treestate. The anchor is encoded separately in
/// > each Spend description for v4 transactions, or encoded once and
/// > shared between all Spend descriptions in a v5 transaction.[^sapling]
///
/// > The anchorOrchard field of the transaction, whenever it exists (i.e. when
/// > there are any Action descriptions), MUST refer to some earlier block’s
/// > final Orchard treestate.[^orchard]
///
/// [^sprout]: <https://zips.z.cash/protocol/protocol.pdf#joinsplit>
/// [^sapling]: <https://zips.z.cash/protocol/protocol.pdf#spendsandoutputs>
/// [^orchard]: <https://zips.z.cash/protocol/protocol.pdf#actions>
#[tracing::instrument(skip(finalized_state, parent_chain, prepared))]
pub(crate) fn anchors_refer_to_earlier_treestates(
    finalized_state: &FinalizedState,
    parent_chain: &Chain,
    prepared: &PreparedBlock,
) -> Result<(), ValidateContextError> {
    for transaction in prepared.block.transactions.iter() {
        // Sprout JoinSplits, with interstitial treestates to check as well.
        //
        // The FIRST JOINSPLIT in a transaction MUST refer to the output treestate
        // of a previous block.
        if transaction.has_sprout_joinsplit_data() {
            // > The anchor of each JoinSplit description in a transaction MUST refer to
            // > either some earlier block’s final Sprout treestate, or to the interstitial
            // > output treestate of any prior JoinSplit description in the same transaction.
            //
            // https://zips.z.cash/protocol/protocol.pdf#joinsplit
            let mut interstitial_roots: HashSet<sprout::tree::Root> = HashSet::new();

            let mut interstitial_note_commitment_tree = parent_chain.sprout_note_commitment_tree();

            for joinsplit in transaction.sprout_groth16_joinsplits() {
                // Check all anchor sets, including the one for interstitial anchors.
                //
                // Note that [`interstitial_roots`] is always empty in the first
                // iteration of the loop. This is because:
                //
                // > "The anchor of each JoinSplit description in a transaction
                // > MUST refer to [...] to the interstitial output treestate of
                // > any **prior** JoinSplit description in the same transaction."
                if !parent_chain.sprout_anchors.contains(&joinsplit.anchor)
                    && !finalized_state.contains_sprout_anchor(&joinsplit.anchor)
                    && (!interstitial_roots.contains(&joinsplit.anchor))
                {
                    return Err(ValidateContextError::UnknownSproutAnchor {
                        anchor: joinsplit.anchor,
                    });
                }

                tracing::debug!(?joinsplit.anchor, "validated sprout anchor");

                // Add new anchors to the interstitial note commitment tree.
                for cm in joinsplit.commitments {
                    interstitial_note_commitment_tree
                        .append(cm)
                        .expect("note commitment should be appendable to the tree");
                }

                interstitial_roots.insert(interstitial_note_commitment_tree.root());

                tracing::debug!(?joinsplit.anchor, "observed sprout anchor");
            }
        }

        // Sapling Spends
        //
        // MUST refer to some earlier block’s final Sapling treestate.
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
