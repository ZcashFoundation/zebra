//! Property tests for transparent inputs and outputs.
//!
//! TODO: Move this module into a `tests` submodule.

use zebra_test::prelude::*;

use crate::{block, LedgerState};

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

    let max_size = 100;
    let strategy = LedgerState::coinbase_strategy(None)
        .prop_flat_map(|ledger_state| Input::vec_strategy(ledger_state, max_size));

    proptest!(|(inputs in strategy)| {
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
