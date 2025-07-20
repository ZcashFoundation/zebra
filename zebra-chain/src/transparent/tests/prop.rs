//! Property tests for transparent inputs and outputs.

use zebra_test::prelude::*;

use crate::{block, fmt::SummaryDebug, transaction::arbitrary::MAX_ARBITRARY_ITEMS, LedgerState};

use super::super::{
    serialize::{parse_coinbase_script, write_coinbase_height},
    Input,
};

#[test]
fn coinbase_has_height() -> Result<()> {
    let _init_guard = zebra_test::init();

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
    let _init_guard = zebra_test::init();

    let strategy = LedgerState::coinbase_strategy(None, None, false)
        .prop_flat_map(|ledger_state| Input::vec_strategy(&ledger_state, MAX_ARBITRARY_ITEMS));

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

#[test]
fn coinbase_script_round_trip() -> Result<()> {
    let _init_guard = zebra_test::init();

    proptest!(|(input in any::<block::Height>().prop_flat_map(|h| Input::arbitrary_with(Some(h))))| {
        let (expected_height, expected_data) = match input {
            Input::Coinbase { height, data, .. } => (height, data),
            _ => unreachable!("proptest strategy must generate a coinbase input"),
        };

        let mut script = Vec::new();

        write_coinbase_height(expected_height, &expected_data, &mut script)?;
        script.extend(expected_data.as_ref());

        let (parsed_height, parsed_data) = parse_coinbase_script(&script)?;

        prop_assert_eq!(expected_height, parsed_height);
        prop_assert_eq!(expected_data, parsed_data);
    });

    Ok(())
}
