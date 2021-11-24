//! Checks for whether cited anchors are previously-computed note commitment
//! tree roots.

use std::ops::Deref;

use zebra_chain::transaction::Transaction::*;

use crate::{
    service::{finalized_state::FinalizedState, non_finalized_state::Chain},
    PreparedBlock, ValidateContextError,
};

/// Check that all the Sprout, Sapling, and Orchard anchors specified by
/// transactions in this block have been computed previously within the context
/// of its parent chain.
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
        let (
            _joinsplit_data,
            orchard_shielded_data,
        ) = match transaction.deref() {
            V4 {
                joinsplit_data,
                ..
            } => (joinsplit_data, &None),
            V5 {
                orchard_shielded_data,
                ..
            } => (
                &None,
                orchard_shielded_data,
            ),
            V1 { .. } | V2 { .. } | V3 { .. } => unreachable!(
                "older transaction versions only exist in finalized blocks, because of the mandatory canopy checkpoint",
            ),
        };

        // Sprout JoinSplits, with interstitial treestates to check as well
        //
        // The FIRST JOINSPLIT in a transaction MUST refer to the output treestate
        // of a previous block.

        // if let Some(sprout_shielded_data) = joinsplit_data {
        //     for joinsplit in transaction.sprout_groth16_joinsplits() {
        //         if !parent_chain.sprout_anchors.contains(joinsplit.anchor)
        //             && !finalized_state.contains_sprout_anchor(&joinsplit.anchor)
        //         {
        //             if !(joinsplit == &sprout_shielded_data.first) {
        //                 // TODO: check interstitial treestates of the earlier JoinSplits
        //                 // in this transaction against this anchor
        //                 unimplemented!()
        //             } else {
        //                 return Err(ValidateContextError::UnknownSproutAnchor {
        //                     anchor: joinsplit.anchor,
        //                 });
        //             }
        //         }
        //     }
        // }

        // Sapling Spends
        //
        // MUST refer to some earlier block’s final Sapling treestate.
        if let Some(orchard_shielded_data) = transaction.orchard_shielded_data() {
            for spend in transaction.sapling_spends_per_anchor() {
                tracing::debug!(?spend.per_spend_anchor, "observed sapling anchor");

                if !parent_chain
                    .sapling_anchors
                    .contains(&spend.per_spend_anchor)
                    && !finalized_state.contains_sapling_anchor(&spend.per_spend_anchor)
                {
                    return Err(ValidateContextError::UnknownSaplingAnchor {
                        anchor: spend.per_spend_anchor,
                    });
                }

                tracing::debug!(?spend.per_spend_anchor, "validated sapling anchor");
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
