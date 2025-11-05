//! Checks for whether cited anchors are previously-computed note commitment
//! tree roots.

use std::{collections::HashMap, sync::Arc};

use rayon::prelude::*;

use zebra_chain::{
    block::{Block, Height},
    sprout,
    transaction::{Hash as TransactionHash, Transaction, UnminedTx},
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    SemanticallyVerifiedBlock, ValidateContextError,
};

/// Checks the final Sapling and Orchard anchors specified by `transaction`
///
/// This method checks for anchors computed from the final treestate of each block in
/// the `parent_chain` or `finalized_state`.
#[tracing::instrument(skip(finalized_state, parent_chain, transaction))]
fn sapling_orchard_anchors_refer_to_final_treestates(
    finalized_state: &ZebraDb,
    parent_chain: Option<&Arc<Chain>>,
    transaction: &Arc<Transaction>,
    transaction_hash: TransactionHash,
    tx_index_in_block: Option<usize>,
    height: Option<Height>,
) -> Result<(), ValidateContextError> {
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
    for (anchor_index_in_tx, anchor) in transaction.sapling_anchors().enumerate() {
        tracing::debug!(
            ?anchor,
            ?anchor_index_in_tx,
            ?tx_index_in_block,
            ?height,
            "observed sapling anchor",
        );

        if !parent_chain
            .map(|chain| chain.sapling_anchors.contains(&anchor))
            .unwrap_or(false)
            && !finalized_state.contains_sapling_anchor(&anchor)
        {
            return Err(ValidateContextError::UnknownSaplingAnchor {
                anchor,
                height,
                tx_index_in_block,
                transaction_hash,
            });
        }

        tracing::debug!(
            ?anchor,
            ?anchor_index_in_tx,
            ?tx_index_in_block,
            ?height,
            "validated sapling anchor",
        );
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
        tracing::debug!(
            ?orchard_shielded_data.shared_anchor,
            ?tx_index_in_block,
            ?height,
            "observed orchard anchor",
        );

        if !parent_chain
            .map(|chain| {
                chain
                    .orchard_anchors
                    .contains(&orchard_shielded_data.shared_anchor)
            })
            .unwrap_or(false)
            && !finalized_state.contains_orchard_anchor(&orchard_shielded_data.shared_anchor)
        {
            return Err(ValidateContextError::UnknownOrchardAnchor {
                anchor: orchard_shielded_data.shared_anchor,
                height,
                tx_index_in_block,
                transaction_hash,
            });
        }

        tracing::debug!(
            ?orchard_shielded_data.shared_anchor,
            ?tx_index_in_block,
            ?height,
            "validated orchard anchor",
        );
    }

    Ok(())
}

/// This function fetches and returns the Sprout final treestates from the state,
/// so [`sprout_anchors_refer_to_treestates()`] can check Sprout final and interstitial treestates,
/// without accessing the disk.
///
/// Sprout anchors may also refer to the interstitial output treestate of any prior
/// `JoinSplit` _within the same transaction_; these are created on the fly
/// in [`sprout_anchors_refer_to_treestates()`].
#[tracing::instrument(skip(sprout_final_treestates, finalized_state, parent_chain, transaction))]
fn fetch_sprout_final_treestates(
    sprout_final_treestates: &mut HashMap<
        sprout::tree::Root,
        Arc<sprout::tree::NoteCommitmentTree>,
    >,
    finalized_state: &ZebraDb,
    parent_chain: Option<&Arc<Chain>>,
    transaction: &Arc<Transaction>,
    tx_index_in_block: Option<usize>,
    height: Option<Height>,
) {
    // Fetch and return Sprout JoinSplit final treestates
    for (joinsplit_index_in_tx, joinsplit) in transaction.sprout_groth16_joinsplits().enumerate() {
        // Avoid duplicate fetches
        if sprout_final_treestates.contains_key(&joinsplit.anchor) {
            continue;
        }

        let input_tree = parent_chain
            .and_then(|chain| chain.sprout_trees_by_anchor.get(&joinsplit.anchor).cloned())
            .or_else(|| finalized_state.sprout_tree_by_anchor(&joinsplit.anchor));

        if let Some(input_tree) = input_tree {
            sprout_final_treestates.insert(joinsplit.anchor, input_tree);

            /* TODO:
               - fix tests that generate incorrect root data
               - assert that joinsplit.anchor matches input_tree.root() during tests,
                 but don't assert in production, because the check is CPU-intensive,
                 and sprout_anchors_refer_to_treestates() constructs the map correctly
            */

            tracing::debug!(
                sprout_final_treestate_count = ?sprout_final_treestates.len(),
                ?joinsplit.anchor,
                ?joinsplit_index_in_tx,
                ?tx_index_in_block,
                ?height,
                "observed sprout final treestate anchor",
            );
        }
    }

    tracing::trace!(
        sprout_final_treestate_count = ?sprout_final_treestates.len(),
        ?sprout_final_treestates,
        ?height,
        "returning sprout final treestate anchors",
    );
}

/// Checks the Sprout anchors specified by `transactions`.
///
/// Sprout anchors may refer to some earlier block's final treestate (like
/// Sapling and Orchard do exclusively) _or_ to the interstitial output
/// treestate of any prior `JoinSplit` _within the same transaction_.
///
/// This method searches for anchors in the supplied `sprout_final_treestates`
/// (which must be populated with all treestates pointed to in the `semantically_verified` block;
/// see [`fetch_sprout_final_treestates()`]); or in the interstitial
/// treestates which are computed on the fly in this function.
#[tracing::instrument(skip(sprout_final_treestates, transaction))]
fn sprout_anchors_refer_to_treestates(
    sprout_final_treestates: &HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
    transaction: &Arc<Transaction>,
    transaction_hash: TransactionHash,
    tx_index_in_block: Option<usize>,
    height: Option<Height>,
) -> Result<(), ValidateContextError> {
    // Sprout JoinSplits, with interstitial treestates to check as well.
    let mut interstitial_trees: HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>> =
        HashMap::new();

    let joinsplit_count = transaction.sprout_groth16_joinsplits().count();

    for (joinsplit_index_in_tx, joinsplit) in transaction.sprout_groth16_joinsplits().enumerate() {
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
            .or_else(|| sprout_final_treestates.get(&joinsplit.anchor).cloned());

        tracing::trace!(
            ?input_tree,
            final_lookup = ?sprout_final_treestates.get(&joinsplit.anchor),
            interstitial_lookup = ?interstitial_trees.get(&joinsplit.anchor),
            interstitial_tree_count = ?interstitial_trees.len(),
            ?interstitial_trees,
            ?height,
            "looked up sprout treestate anchor",
        );

        let mut input_tree = match input_tree {
            Some(tree) => tree,
            None => {
                tracing::debug!(
                    ?joinsplit.anchor,
                    ?joinsplit_index_in_tx,
                    ?tx_index_in_block,
                    ?height,
                    "failed to find sprout anchor",
                );
                return Err(ValidateContextError::UnknownSproutAnchor {
                    anchor: joinsplit.anchor,
                    height,
                    tx_index_in_block,
                    transaction_hash,
                });
            }
        };

        tracing::debug!(
            ?joinsplit.anchor,
            ?joinsplit_index_in_tx,
            ?tx_index_in_block,
            ?height,
            "validated sprout anchor",
        );

        // The last interstitial treestate in a transaction can never be used,
        // so we avoid generating it.
        if joinsplit_index_in_tx == joinsplit_count - 1 {
            continue;
        }

        let input_tree_inner = Arc::make_mut(&mut input_tree);

        // Add new anchors to the interstitial note commitment tree.
        for cm in joinsplit.commitments {
            input_tree_inner.append(cm)?;
        }

        interstitial_trees.insert(input_tree.root(), input_tree);

        tracing::debug!(
            ?joinsplit.anchor,
            ?joinsplit_index_in_tx,
            ?tx_index_in_block,
            ?height,
            "observed sprout interstitial anchor",
        );
    }

    Ok(())
}

/// Accepts a [`ZebraDb`], [`Chain`], and [`SemanticallyVerifiedBlock`].
///
/// Iterates over the transactions in the [`SemanticallyVerifiedBlock`] checking the final Sapling and Orchard anchors.
///
/// This method checks for anchors computed from the final treestate of each block in
/// the `parent_chain` or `finalized_state`.
#[tracing::instrument(skip_all)]
pub(crate) fn block_sapling_orchard_anchors_refer_to_final_treestates(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> Result<(), ValidateContextError> {
    semantically_verified
        .block
        .transactions
        .iter()
        .enumerate()
        .try_for_each(|(tx_index_in_block, transaction)| {
            sapling_orchard_anchors_refer_to_final_treestates(
                finalized_state,
                Some(parent_chain),
                transaction,
                semantically_verified.transaction_hashes[tx_index_in_block],
                Some(tx_index_in_block),
                Some(semantically_verified.height),
            )
        })
}

/// Accepts a [`ZebraDb`], [`Arc<Chain>`](Chain), and [`SemanticallyVerifiedBlock`].
///
/// Iterates over the transactions in the [`SemanticallyVerifiedBlock`], and fetches the Sprout final treestates
/// from the state.
///
/// Returns a `HashMap` of the Sprout final treestates from the state for [`sprout_anchors_refer_to_treestates()`]
/// to check Sprout final and interstitial treestates without accessing the disk.
///
/// Sprout anchors may also refer to the interstitial output treestate of any prior
/// `JoinSplit` _within the same transaction_; these are created on the fly
/// in [`sprout_anchors_refer_to_treestates()`].
#[tracing::instrument(skip_all)]
pub(crate) fn block_fetch_sprout_final_treestates(
    finalized_state: &ZebraDb,
    parent_chain: &Arc<Chain>,
    semantically_verified: &SemanticallyVerifiedBlock,
) -> HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>> {
    let mut sprout_final_treestates = HashMap::new();

    for (tx_index_in_block, transaction) in
        semantically_verified.block.transactions.iter().enumerate()
    {
        fetch_sprout_final_treestates(
            &mut sprout_final_treestates,
            finalized_state,
            Some(parent_chain),
            transaction,
            Some(tx_index_in_block),
            Some(semantically_verified.height),
        );
    }

    sprout_final_treestates
}

/// Accepts a [`ZebraDb`], [`Arc<Chain>`](Chain), [`Arc<Block>`](Block), and an
/// [`Arc<[transaction::Hash]>`](TransactionHash) of hashes corresponding to the transactions in [`Block`]
///
/// Iterates over the transactions in the [`Block`] checking the final Sprout anchors.
///
/// Sprout anchors may refer to some earlier block's final treestate (like
/// Sapling and Orchard do exclusively) _or_ to the interstitial output
/// treestate of any prior `JoinSplit` _within the same transaction_.
///
/// This method searches for anchors in the supplied `sprout_final_treestates`
/// (which must be populated with all treestates pointed to in the `semantically_verified` block;
/// see [`fetch_sprout_final_treestates()`]); or in the interstitial
/// treestates which are computed on the fly in this function.
#[tracing::instrument(skip(sprout_final_treestates, block, transaction_hashes))]
pub(crate) fn block_sprout_anchors_refer_to_treestates(
    sprout_final_treestates: HashMap<sprout::tree::Root, Arc<sprout::tree::NoteCommitmentTree>>,
    block: Arc<Block>,
    // Only used for debugging
    transaction_hashes: Arc<[TransactionHash]>,
    height: Height,
) -> Result<(), ValidateContextError> {
    tracing::trace!(
        sprout_final_treestate_count = ?sprout_final_treestates.len(),
        ?sprout_final_treestates,
        ?height,
        "received sprout final treestate anchors",
    );

    let check_tx_sprout_anchors = |(tx_index_in_block, transaction)| {
        sprout_anchors_refer_to_treestates(
            &sprout_final_treestates,
            transaction,
            transaction_hashes[tx_index_in_block],
            Some(tx_index_in_block),
            Some(height),
        )?;

        Ok(())
    };

    // The overhead for a parallel iterator is unwarranted if sprout_final_treestates is empty
    // because it will either return an error for the first transaction or only check that `joinsplit_data`
    // is `None` for each transaction.
    if sprout_final_treestates.is_empty() {
        // The block has no valid sprout anchors
        block
            .transactions
            .iter()
            .enumerate()
            .try_for_each(check_tx_sprout_anchors)
    } else {
        block
            .transactions
            .par_iter()
            .enumerate()
            .try_for_each(check_tx_sprout_anchors)
    }
}

/// Accepts a [`ZebraDb`], an optional [`Option<Arc<Chain>>`](Chain), and an [`UnminedTx`].
///
/// Checks the final Sprout, Sapling and Orchard anchors specified in the [`UnminedTx`].
///
/// This method checks for anchors computed from the final treestate of each block in
/// the `parent_chain` or `finalized_state`.
#[tracing::instrument(skip_all)]
pub(crate) fn tx_anchors_refer_to_final_treestates(
    finalized_state: &ZebraDb,
    parent_chain: Option<&Arc<Chain>>,
    unmined_tx: &UnminedTx,
) -> Result<(), ValidateContextError> {
    sapling_orchard_anchors_refer_to_final_treestates(
        finalized_state,
        parent_chain,
        &unmined_tx.transaction,
        unmined_tx.id.mined_id(),
        None,
        None,
    )?;

    // If there are no sprout transactions in the block, avoid running a rayon scope
    if unmined_tx.transaction.has_sprout_joinsplit_data() {
        let mut sprout_final_treestates = HashMap::new();

        fetch_sprout_final_treestates(
            &mut sprout_final_treestates,
            finalized_state,
            parent_chain,
            &unmined_tx.transaction,
            None,
            None,
        );

        let mut sprout_anchors_result = None;
        rayon::in_place_scope_fifo(|s| {
            // This check is expensive, because it updates a note commitment tree for each sprout JoinSplit.
            // Since we could be processing attacker-controlled mempool transactions, we need to run each one
            // in its own thread, separately from tokio's blocking I/O threads. And if we are under heavy load,
            // we want verification to finish in order, so that later transactions can't delay earlier ones.
            s.spawn_fifo(|_s| {
                tracing::trace!(
                    sprout_final_treestate_count = ?sprout_final_treestates.len(),
                    ?sprout_final_treestates,
                    "received sprout final treestate anchors",
                );

                sprout_anchors_result = Some(sprout_anchors_refer_to_treestates(
                    &sprout_final_treestates,
                    &unmined_tx.transaction,
                    unmined_tx.id.mined_id(),
                    None,
                    None,
                ));
            });
        });

        sprout_anchors_result.expect("scope has finished")?;
    }

    Ok(())
}
