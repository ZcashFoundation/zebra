//! Shared state reading code.
//!
//! Used by [`StateService`] and [`ReadStateService`]
//! to read from the best [`Chain`] in the [`NonFinalizedState`],
//! and the database in the [`FinalizedState`].

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    amount::{Amount, NonNegative},
    block::{self, Block},
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
    _db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<Amount<NonNegative>, BoxError>
where
    C: AsRef<Chain>,
{
    // # Correctness
    //
    // The StateService commits blocks to the finalized state before updating the latest chain,
    // and it can commit additional blocks after we've cloned this `chain` variable.
    //
    // TODO: pop root blocks from `chain` until the chain root is a child of the finalized tip

    let _partial_transparent_balance_change = chain
        .as_ref()
        .map(|chain| chain.as_ref().partial_transparent_balance_change(addresses))
        .unwrap_or_else(Amount::zero);

    //.or_else(|| db.finalized_transparent_balance(addresses))

    Err("unimplemented".into())
}
