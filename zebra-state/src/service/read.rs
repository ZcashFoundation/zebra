//! Shared state reading code.
//!
//! Used by [`StateService`] and [`ReadStateService`]
//! to read from the best [`Chain`] in the [`NonFinalizedState`],
//! and the database in the [`FinalizedState`].

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block, Height},
    transaction::{self, Transaction},
    transparent,
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    BoxError, HashOrHeight,
};

#[cfg(test)]
mod tests;

/// Returns the [`Block`] with [`block::Hash`](zebra_chain::block::Hash) or
/// [`Height`](zebra_chain::block::Height),
/// if it exists in the non-finalized `chain` or finalized `db`.
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
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // Since blocks are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first.
    // (`chain` is always in memory, but `db` stores blocks on disk, with a memory cache.)
    chain
        .as_ref()
        .and_then(|chain| chain.as_ref().block(hash_or_height))
        .map(|contextual| contextual.block.clone())
        .or_else(|| db.block(hash_or_height))
}

/// Returns the [`Transaction`] with [`transaction::Hash`],
/// if it exists in the non-finalized `chain` or finalized `db`.
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
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // Since transactions are the same in the finalized and non-finalized state,
    // we check the most efficient alternative first.
    // (`chain` is always in memory, but `db` stores transactions on disk, with a memory cache.)
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

/// Returns the total transparent balance for the supplied [`transparent::Address`]es.
///
/// If they do not exist in the non-finalized `chain` or finalized `db`, returns zero.
#[allow(dead_code)]
pub(crate) fn transparent_balance<C>(
    chain: Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<Amount<NonNegative>, BoxError>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.

    // Check if the finalized state changed while we were querying it
    let original_finalized_tip = db.tip();

    let transparent_balance = query_transparent_balance(&chain, db, addresses);
    let finalized_tip = db.tip();

    if original_finalized_tip != finalized_tip {
        // Correctness: Some balances might be from before the block, and some after
        //
        // TODO: retry a few times?
        return Err("unable to get balance: state was committing a block".into());
    }

    // Check if the finalized and non-finalized states match
    if let Some(chain) = chain {
        let required_chain_root = finalized_tip
            .map(|(height, _hash)| (height + 1).unwrap())
            .unwrap_or(Height(0));

        if chain.as_ref().non_finalized_root_height() != required_chain_root {
            // Correctness: some balances might have duplicate creates or spends
            //
            // TODO: pop root blocks from `chain` until the chain root is a child of the finalized tip
            return Err(
                "unable to get balance: finalized state doesn't match partial chain".into(),
            );
        }
    }

    Ok(transparent_balance.expect(
        "unexpected amount overflow: value balances are valid, so partial sum should be valid",
    ))
}

/// Returns the total transparent balance for the supplied [`transparent::Address`]es.
///
/// If they do not exist in the non-finalized `chain` or finalized `db`, returns zero.
fn query_transparent_balance<C>(
    chain: &Option<C>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<Amount<NonNegative>, BoxError>
where
    C: AsRef<Chain>,
{
    let balance_change = chain
        .as_ref()
        .map(|chain| {
            chain
                .as_ref()
                .partial_transparent_balance_change(&addresses)
        })
        .unwrap_or_else(Amount::zero);

    let finalized_balance = db
        .partial_finalized_transparent_balance(&addresses)
        .constrain()?;

    let balance = (finalized_balance + balance_change)?.constrain()?;

    Ok(balance)
}
