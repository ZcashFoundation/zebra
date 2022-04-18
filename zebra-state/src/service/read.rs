//! Shared state reading code.
//!
//! Used by [`StateService`](crate::StateService) and
//! [`ReadStateService`](crate::ReadStateService) to read from the best
//! [`Chain`] in the
//! [`NonFinalizedState`](crate::service::non_finalized_state::NonFinalizedState),
//! and the database in the
//! [`FinalizedState`](crate::service::finalized_state::FinalizedState).

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    block::{self, Block, Height},
    orchard, sapling,
    transaction::{self, Transaction},
    transparent,
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    BoxError, HashOrHeight,
};

#[cfg(test)]
mod tests;

/// If the transparent address index queries are interrupted by a new finalized block,
/// retry this many times.
///
/// Once we're at the tip, we expect up to 2 blocks to arrive at the same time.
/// If any more arrive, the client should wait until we're synchronised with our peers.
const FINALIZED_ADDRESS_INDEX_RETRIES: usize = 3;

/// Returns the [`Block`] with [`block::Hash`](zebra_chain::block::Hash) or
/// [`Height`](zebra_chain::block::Height), if it exists in the non-finalized
/// `chain` or finalized `db`.
pub(crate) fn block<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<Block>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since blocks are the same in the finalized and non-finalized state, we
    // check the most efficient alternative first. (`chain` is always in memory,
    // but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}

/// Returns the [`Transaction`] with [`transaction::Hash`], if it exists in the
/// non-finalized `chain` or finalized `db`.
pub(crate) fn transaction<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash: transaction::Hash,
) -> Option<(Arc<Transaction>, block::Height)>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since transactions are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first. (`chain` is always in
    // memory, but `db` stores transactions on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| {
            chain
                .as_ref()
                .transaction(hash)
                .map(|(tx, height)| (tx.clone(), height))
        })
        .or_else(|| db.transaction(hash))
}

/// Returns the Sapling
/// [`NoteCommitmentTree`](sapling::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn sapling_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<sapling::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since sapling treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().sapling_tree(hash_or_height).cloned())
        .map(Arc::new)
        .or_else(|| db.sapling_tree(hash_or_height))
}

/// Returns the Orchard
/// [`NoteCommitmentTree`](orchard::tree::NoteCommitmentTree) specified by a
/// hash or height, if it exists in the non-finalized `chain` or finalized `db`.
pub(crate) fn orchard_tree<C>(
    chain: Option<C>,
    db: &ZebraDb,
    hash_or_height: HashOrHeight,
) -> Option<Arc<orchard::tree::NoteCommitmentTree>>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating
    // the latest chain, and it can commit additional blocks after we've cloned
    // this `chain` variable.
    //
    // Since orchard treestates are the same in the finalized and non-finalized
    // state, we check the most efficient alternative first. (`chain` is always
    // in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().orchard_tree(hash_or_height).cloned())
        .map(Arc::new)
        .or_else(|| db.orchard_tree(hash_or_height))
}

/// Returns the total transparent balance for the supplied [`transparent::Address`]es.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`, returns zero.
#[allow(dead_code)]
pub(crate) fn transparent_balance(
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<Amount<NonNegative>, BoxError> {
    let mut balance_result = finalized_transparent_balance(db, &addresses);

    // Retry the finalized balance query if it was interruped by a finalizing block
    for _ in 0..FINALIZED_ADDRESS_INDEX_RETRIES {
        if balance_result.is_ok() {
            break;
        }

        balance_result = finalized_transparent_balance(db, &addresses);
    }

    let (mut balance, finalized_tip) = balance_result?;

    // Apply the non-finalized balance changes
    if let Some(chain) = chain {
        let chain_balance_change =
            chain_transparent_balance_change(chain, &addresses, finalized_tip);

        balance = apply_balance_change(balance, chain_balance_change).expect(
            "unexpected amount overflow: value balances are valid, so partial sum should be valid",
        );
    }

    Ok(balance)
}

/// Returns the total transparent balance for `addresses` in the finalized chain,
/// and the finalized tip height the balances were queried at.
///
/// If the addresses do not exist in the finalized `db`, returns zero.
//
// TODO: turn the return type into a struct?
fn finalized_transparent_balance(
    db: &ZebraDb,
    addresses: &HashSet<transparent::Address>,
) -> Result<(Amount<NonNegative>, Option<Height>), BoxError> {
    // # Correctness
    //
    // The StateService can commit additional blocks while we are querying address balances.

    // Check if the finalized state changed while we were querying it
    let original_finalized_tip = db.tip();

    let finalized_balance = db.partial_finalized_transparent_balance(addresses);

    let finalized_tip = db.tip();

    if original_finalized_tip != finalized_tip {
        // Correctness: Some balances might be from before the block, and some after
        return Err("unable to get balance: state was committing a block".into());
    }

    let finalized_tip = finalized_tip.map(|(height, _hash)| height);

    Ok((finalized_balance, finalized_tip))
}

/// Returns the total transparent balance change for `addresses` in the non-finalized chain,
/// matching the balance for the `finalized_tip`.
///
/// If the addresses do not exist in the non-finalized `chain`, returns zero.
fn chain_transparent_balance_change(
    mut chain: Arc<Chain>,
    addresses: &HashSet<transparent::Address>,
    finalized_tip: Option<Height>,
) -> Amount<NegativeAllowed> {
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.

    // Check if the finalized and non-finalized states match
    let required_chain_root = finalized_tip
        .map(|tip| (tip + 1).unwrap())
        .unwrap_or(Height(0));

    let chain = Arc::make_mut(&mut chain);

    assert!(
        chain.non_finalized_root_height() <= required_chain_root,
        "unexpected chain gap: the best chain is updated after its previous root is finalized"
    );

    let chain_tip = chain.non_finalized_tip_height();

    // If we've already committed this entire chain, ignore its balance changes.
    // This is more likely if the non-finalized state is just getting started.
    if chain_tip < required_chain_root {
        return Amount::zero();
    }

    // Correctness: some balances might have duplicate creates or spends,
    // so we pop root blocks from `chain` until the chain root is a child of the finalized tip.
    while chain.non_finalized_root_height() < required_chain_root {
        // TODO: just revert the transparent balances, to improve performance
        chain.pop_root();
    }

    chain.partial_transparent_balance_change(addresses)
}

/// Add the supplied finalized and non-finalized balances together,
/// and return the result.
fn apply_balance_change(
    finalized_balance: Amount<NonNegative>,
    chain_balance_change: Amount<NegativeAllowed>,
) -> amount::Result<Amount<NonNegative>> {
    let balance = finalized_balance.constrain()? + chain_balance_change;

    balance?.constrain()
}
