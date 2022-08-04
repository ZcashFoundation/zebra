//! Property tests for transparent inputs and outputs.

use zebra_test::prelude::*;

use crate::{block, fmt::SummaryDebug, transaction::arbitrary::MAX_ARBITRARY_ITEMS, LedgerState};

use super::super::{
    serialize::{parse_coinbase_height, write_coinbase_height},
    Input,
};

use proptest::collection::vec;

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
        .prop_flat_map(|ledger_state| Input::vec_strategy(ledger_state, MAX_ARBITRARY_ITEMS));

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
fn coinbase_height_round_trip_from_random_input() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy =
        any::<block::Height>().prop_flat_map(|height| Input::arbitrary_with(Some(height)));

    proptest!(|(input in strategy)| {
        let (height, data) = match input {
            Input::Coinbase { height, data, .. } => (height, data),
            _ => unreachable!("all inputs will have coinbase height and data"),
        };
        let mut encoded = Vec::new();
        write_coinbase_height(height, &data, &mut encoded)?;
        let decoded = parse_coinbase_height(encoded)?;

        prop_assert_eq!(height, decoded.0);
    });

    Ok(())
}

proptest! {
    #[test]
    fn coinbase_height_round_trip_from_random_bytes(mut height_bytes in vec(any::<u8>(), 1..5)) {
        let mut encoded1 = vec![height_bytes.len() as u8];
        encoded1.append(&mut height_bytes);

        let decoded = parse_coinbase_height(encoded1.clone()).ok();

        if decoded.is_some() {
            let mut encoded2 = Vec::new();
            write_coinbase_height(decoded.as_ref().unwrap().0, &decoded.unwrap().1, &mut encoded2)?;
            prop_assert_eq!(encoded2, encoded1);
        }
    }

    #[test]
    fn coinbase_height_round_trip_from_random_byte(height_byte in vec(any::<u8>(), 1..2)) {
        let encoded1 = height_byte;
        let decoded = parse_coinbase_height(encoded1.clone()).ok();

        if decoded.is_some() {
            let mut encoded2 = Vec::new();
            write_coinbase_height(decoded.as_ref().unwrap().0, &decoded.unwrap().1, &mut encoded2)?;
            prop_assert_eq!(encoded2, encoded1);
        }
    }
}
