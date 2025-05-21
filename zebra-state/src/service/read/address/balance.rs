//! Reading address balances.
//!
//! In the functions in this module:
//!
//! The block write task commits blocks to the finalized state before updating
//! `chain` with a cached copy of the best non-finalized chain from
//! `NonFinalizedState.chain_set`. Then the block commit task can commit additional blocks to
//! the finalized state after we've cloned the `chain`.
//!
//! This means that some blocks can be in both:
//! - the cached [`Chain`], and
//! - the shared finalized [`ZebraDb`] reference.

use std::{collections::HashSet, sync::Arc};

use zebra_chain::{
    amount::{self, Amount, NegativeAllowed, NonNegative},
    block::Height,
    transparent,
};

use crate::{
    service::{
        finalized_state::ZebraDb, non_finalized_state::Chain, read::FINALIZED_STATE_QUERY_RETRIES,
    },
    BoxError,
};

/// Returns the total transparent balance and received balance for the supplied [`transparent::Address`]es.
///
/// If the addresses do not exist in the non-finalized `chain` or finalized `db`, returns zero.
pub fn transparent_balance(
    chain: Option<Arc<Chain>>,
    db: &ZebraDb,
    addresses: HashSet<transparent::Address>,
) -> Result<(Amount<NonNegative>, u64), BoxError> {
    let mut balance_result = finalized_transparent_balance(db, &addresses);

    // Retry the finalized balance query if it was interrupted by a finalizing block
    //
    // TODO: refactor this into a generic retry(finalized_closure, process_and_check_closure) fn
    for _ in 0..FINALIZED_STATE_QUERY_RETRIES {
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
) -> Result<((Amount<NonNegative>, u64), Option<Height>), BoxError> {
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
) -> (Amount<NegativeAllowed>, u64) {
    // # Correctness
    //
    // Find the balance adjustment that corrects for overlapping finalized and non-finalized blocks.

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
        return (Amount::zero(), 0);
    }

    // Correctness: some balances might have duplicate creates or spends,
    // so we pop root blocks from `chain` until the chain root is a child of the finalized tip.
    while chain.non_finalized_root_height() < required_chain_root {
        // TODO: just revert the transparent balances, to improve performance
        chain.pop_root();
    }

    (
        // TODO: Iterate over transparent transfers once here instead of twice.
        chain.partial_transparent_balance_change(addresses),
        chain.partial_transparent_received_change(addresses),
    )
}

/// Add the supplied finalized and non-finalized balances together,
/// and return the result.
fn apply_balance_change(
    (finalized_balance, finalized_received): (Amount<NonNegative>, u64),
    (chain_balance_change, chain_received_change): (Amount<NegativeAllowed>, u64),
) -> amount::Result<(Amount<NonNegative>, u64)> {
    let balance = finalized_balance.constrain()? + chain_balance_change;
    // Addresses could receive more than the max money supply by sending to themselves,
    // use u64::MAX if the addition overflows.
    let received = finalized_received.saturating_add(chain_received_change);
    Ok((balance?.constrain()?, received))
}
