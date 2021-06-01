//! Property tests for transparent inputs and outputs.
//!
//! TODO: Move this module into a `tests` submodule.

use zebra_test::prelude::*;

use crate::{
    block, fmt::SummaryDebug, transaction::arbitrary::MAX_ARBITRARY_TRANSACTIONS, LedgerState,
};

use super::Input;

#[test]
fn coinbase_has_height() -> Result<()> {
    zebra_test::init();

    let strategy =
        any::<block::Height>().prop_flat_map(|height| Input::arbitrary_with(Some(height)));

    proptest!(|(input in strategy)| {
        let is_coinbase = matches!(input, Input::Coinbase { .. });
        prop_assert!(is_coinbase);
    });

    Ok(())
}

#[test]
fn input_coinbase_vecs_only_have_coinbase_input() -> Result<()> {
    zebra_test::init();

    let strategy = LedgerState::coinbase_strategy(None).prop_flat_map(|ledger_state| {
        Input::vec_strategy(ledger_state, MAX_ARBITRARY_TRANSACTIONS)
    });

    proptest!(|(inputs in strategy.prop_map(SummaryDebug))| {
        let len = inputs.len();
        for (ind, input) in inputs.into_iter().enumerate() {
            let is_coinbase = matches!(input, Input::Coinbase { .. });
            if ind == 0 {
                prop_assert!(is_coinbase);
                prop_assert_eq!(1, len);
            } else {
                prop_assert!(!is_coinbase);
            }
        }
    });

    Ok(())
}
