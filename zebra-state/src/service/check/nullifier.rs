//! Checks for nullifier uniqueness.

use std::{collections::HashSet, sync::Arc};

use tracing::trace;
use zebra_chain::transaction::Transaction;

use crate::{
    error::DuplicateNullifierError,
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    PreparedBlock, ValidateContextError,
};

// Tidy up some doc links
#[allow(unused_imports)]
use crate::service;

/// Reject double-spends of nullifers:
/// - one from this [`PreparedBlock`], and the other already committed to the
///   [`FinalizedState`](service::FinalizedState).
///
/// (Duplicate non-finalized nullifiers are rejected during the chain update,
/// see [`add_to_non_finalized_chain_unique`] for details.)
///
/// # Consensus
///
/// > A nullifier MUST NOT repeat either within a transaction,
/// > or across transactions in a valid blockchain.
/// > Sprout and Sapling and Orchard nullifiers are considered disjoint,
/// > even if they have the same bit pattern.
///
/// <https://zips.z.cash/protocol/protocol.pdf#nullifierset>
#[tracing::instrument(skip(prepared, finalized_state))]
pub(crate) fn no_duplicates_in_finalized_chain(
    prepared: &PreparedBlock,
    finalized_state: &ZebraDb,
) -> Result<(), ValidateContextError> {
    for nullifier in prepared.block.sprout_nullifiers() {
        if finalized_state.contains_sprout_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    for nullifier in prepared.block.sapling_nullifiers() {
        if finalized_state.contains_sapling_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    for nullifier in prepared.block.orchard_nullifiers() {
        if finalized_state.contains_orchard_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    Ok(())
}

/// Reject double-spends of nullifiers:
/// - one from this [`Transaction`], and the other already committed to the
///   provided [`ZebraDb`].
///
/// # Consensus
///
/// > A nullifier MUST NOT repeat either within a transaction,
/// > or across transactions in a valid blockchain.
/// > Sprout and Sapling and Orchard nullifiers are considered disjoint,
/// > even if they have the same bit pattern.
///
/// <https://zips.z.cash/protocol/protocol.pdf#nullifierset>
// TODO: impl nullifier getters for a trait and use a generic type to unify
//       this fn with the one for `PreparedBlock`
#[tracing::instrument(skip_all)]
fn tx_no_duplicates_in_finalized_chain(
    finalized_chain: &ZebraDb,
    transaction: &Arc<Transaction>,
) -> Result<(), ValidateContextError> {
    for nullifier in transaction.sprout_nullifiers() {
        if finalized_chain.contains_sprout_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    for nullifier in transaction.sapling_nullifiers() {
        if finalized_chain.contains_sapling_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    for nullifier in transaction.orchard_nullifiers() {
        if finalized_chain.contains_orchard_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(true))?;
        }
    }

    Ok(())
}

/// Reject double-spends of nullifiers:
/// - one from this [`Transaction`], and the other already committed to the
///   provided non-finalized [`Chain`] or [`ZebraDb`].
///
/// # Consensus
///
/// > A nullifier MUST NOT repeat either within a transaction,
/// > or across transactions in a valid blockchain.
/// > Sprout and Sapling and Orchard nullifiers are considered disjoint,
/// > even if they have the same bit pattern.
///
/// <https://zips.z.cash/protocol/protocol.pdf#nullifierset>
#[tracing::instrument(skip_all)]
pub(crate) fn tx_no_duplicates_in_chain(
    non_finalized_chain: Option<&Arc<Chain>>,
    finalized_chain: &ZebraDb,
    transaction: &Arc<Transaction>,
) -> Result<(), ValidateContextError> {
    let non_finalized_chain = match non_finalized_chain {
        None => return tx_no_duplicates_in_finalized_chain(finalized_chain, transaction),
        Some(non_finalized_chain) => non_finalized_chain,
    };

    for nullifier in transaction.sprout_nullifiers() {
        let is_in_non_finalized_state = non_finalized_chain.sprout_nullifiers.contains(nullifier);
        if is_in_non_finalized_state || finalized_chain.contains_sprout_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(!is_in_non_finalized_state))?;
        }
    }

    for nullifier in transaction.sapling_nullifiers() {
        let is_in_non_finalized_state = non_finalized_chain.sapling_nullifiers.contains(nullifier);
        if is_in_non_finalized_state || finalized_chain.contains_sapling_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(!is_in_non_finalized_state))?;
        }
    }

    for nullifier in transaction.orchard_nullifiers() {
        let is_in_non_finalized_state = non_finalized_chain.orchard_nullifiers.contains(nullifier);
        if is_in_non_finalized_state || finalized_chain.contains_orchard_nullifier(nullifier) {
            Err(nullifier.duplicate_nullifier_error(!is_in_non_finalized_state))?;
        }
    }

    Ok(())
}

/// Reject double-spends of nullifers:
/// - both within the same `JoinSplit` (sprout only),
/// - from different `JoinSplit`s, [`sapling::Spend`][2]s or
///   [`orchard::Action`][3]s in this [`Transaction`][1]'s shielded data, or
/// - one from this shielded data, and another from:
///   - a previous transaction in this [`Block`][4], or
///   - a previous block in this non-finalized [`Chain`][5].
///
/// (Duplicate finalized nullifiers are rejected during service contextual validation,
/// see [`no_duplicates_in_finalized_chain`] for details.)
///
/// # Consensus
///
/// > A nullifier MUST NOT repeat either within a transaction,
/// > or across transactions in a valid blockchain.
/// > Sprout and Sapling and Orchard nullifiers are considered disjoint,
/// > even if they have the same bit pattern.
///
/// <https://zips.z.cash/protocol/protocol.pdf#nullifierset>
///
/// We comply with the "disjoint" rule by storing the nullifiers for each
/// pool in separate sets (also with different types), so that even if
/// different pools have nullifiers with same bit pattern, they won't be
/// considered the same when determining uniqueness. This is enforced by the
/// callers of this function.
///
/// [1]: zebra_chain::transaction::Transaction
/// [2]: zebra_chain::sapling::Spend
/// [3]: zebra_chain::orchard::Action
/// [4]: zebra_chain::block::Block
/// [5]: service::non_finalized_state::Chain
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

    Ok(())
}

/// Remove nullifiers that were previously added to this non-finalized
/// [`Chain`][1] by this shielded data.
///
/// "A note can change from being unspent to spent as a node’s view
/// of the best valid block chain is extended by new transactions.
///
/// Also, block chain reorganizations can cause a node to switch
/// to a different best valid block chain that does not contain
/// the transaction in which a note was output"
///
/// <https://zips.z.cash/protocol/nu5.pdf#decryptivk>
///
/// Note: reorganizations can also change the best chain to one
/// where a note was unspent, rather than spent.
///
/// # Panics
///
/// Panics if any nullifier is missing from the chain when we try to remove it.
///
/// Blocks with duplicate nullifiers are rejected by
/// [`add_to_non_finalized_chain_unique`], so this shielded data should be the
/// only shielded data that added this nullifier to this [`Chain`][1].
///
/// [1]: service::non_finalized_state::Chain
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
