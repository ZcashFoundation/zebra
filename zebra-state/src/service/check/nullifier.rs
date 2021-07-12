//! Checks for nullifier uniqueness.
//!
//! "A nullifier MUST NOT repeat either within a transaction,
//! or across transactions in a valid blockchain.
//! Sprout and Sapling and Orchard nullifiers are considered disjoint,
//! even if they have the same bit pattern."
//!
//! https://zips.z.cash/protocol/protocol.pdf#nullifierset

use std::collections::HashSet;

use tracing::trace;

use crate::{
    error::DuplicateNullifierError, service::finalized_state::FinalizedState, PreparedBlock,
    ValidateContextError,
};

/// Reject double-spends of nullifers:
/// - one from this [`Block`], and the other already committed to the [`FinalizedState`].
///
/// (Duplicate non-finalized nullifiers are rejected during the chain update,
/// see [`add_to_non_finalized_chain_unique`] for details.)
///
/// "A transaction is not valid if it would have added a nullifier
/// to the nullifier set that already exists in the set"
///
/// https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
#[tracing::instrument(skip(prepared, finalized_state))]
pub(crate) fn no_duplicates_in_finalized_chain(
    prepared: &PreparedBlock,
    finalized_state: &FinalizedState,
) -> Result<(), ValidateContextError> {
    for nullifier in prepared.block.sprout_nullifiers() {
        if finalized_state.contains_sprout_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    // TODO: sapling and orchard nullifiers (#2231)

    Ok(())
}

/// Reject double-spends of nullifers:
/// - both within the same [`JoinSplit`] (sprout only),
/// - from different [`JoinSplit`]s, [`sapling::Spend`]s or [`Action`]s
///   in this [`Transaction`]'s shielded data, or
/// - one from this shielded data, and another from:
///   - a previous transaction in this [`Block`], or
///   - a previous block in this non-finalized [`Chain`].
///
/// (Duplicate finalized nullifiers are rejected during service contextual validation,
/// see [`no_duplicates_in_finalized_chain`] for details.)
///
/// "A transaction is not valid if it would have added a nullifier
/// to the nullifier set that already exists in the set"
///
/// https://zips.z.cash/protocol/protocol.pdf#commitmentsandnullifiers
#[tracing::instrument(skip(chain_nullifiers, shielded_data_nullifiers))]
pub(crate) fn add_to_non_finalized_chain_unique<'block, NullifierT>(
    chain_nullifiers: &mut HashSet<NullifierT>,
    shielded_data_nullifiers: impl IntoIterator<Item = &'block NullifierT>,
) -> Result<(), ValidateContextError>
where
    NullifierT: DuplicateNullifierError + Copy + std::fmt::Debug + Eq + std::hash::Hash + 'block,
{
    for nullifier in shielded_data_nullifiers.into_iter() {
        trace!(?nullifier, "adding nullifier");

        // reject the nullifier if it is already present in this non-finalized chain
        if !chain_nullifiers.insert(*nullifier) {
            Err(nullifier.duplicate_nullifier_error(false))?;
        }
    }

    // TODO:  test that the chain's nullifiers are not modified on error (this PR)

    Ok(())
}

/// Remove nullifiers that were previously added to this non-finalized [`Chain`]
/// by this shielded data.
///
/// "A note can change from being unspent to spent as a node’s view
/// of the best valid block chain is extended by new transactions.
///
/// Also, block chain reorganizations can cause a node to switch
/// to a different best valid block chain that does not contain
/// the transaction in which a note was output"
///
/// https://zips.z.cash/protocol/nu5.pdf#decryptivk
///
/// Note: reorganizations can also change the best chain to one
/// where a note was unspent, rather than spent.
///
/// # Panics
///
/// Panics if any nullifier is missing from the chain when we try to remove it.
///
/// Blocks with duplicate nullifiers are rejected by
/// [`add_to_non_finalized_chain_unique`], so this shielded data should
/// be the only shielded data that added this nullifier to this [`Chain`].
#[tracing::instrument(skip(chain_nullifiers, shielded_data_nullifiers))]
pub(crate) fn remove_from_non_finalized_chain<'block, NullifierT>(
    chain_nullifiers: &mut HashSet<NullifierT>,
    shielded_data_nullifiers: impl IntoIterator<Item = &'block NullifierT>,
) where
    NullifierT: std::fmt::Debug + Eq + std::hash::Hash + 'block,
{
    for nullifier in shielded_data_nullifiers.into_iter() {
        trace!(?nullifier, "removing nullifier");

        assert!(
            chain_nullifiers.remove(nullifier),
            "nullifier must be present if block was added to chain"
        );
    }
}
