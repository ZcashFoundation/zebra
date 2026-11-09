//! Property tests for transparent inputs and outputs.

use zcash_transparent::address::{Script as ZcashScript, TransparentAddress};
use zebra_test::prelude::*;

use crate::{
    amount::{Amount, NonNegative, MAX_MONEY},
    block,
    fmt::SummaryDebug,
    parameters::Network,
    serialization::MAX_PROTOCOL_MESSAGE_LEN,
    transaction::arbitrary::MAX_ARBITRARY_ITEMS,
    LedgerState,
};

use super::super::{Input, Output, Script};

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

proptest! {
    #[test]
    fn output_address_matches_solver_without_allocating(
        bytes in prop::collection::vec(any::<u8>(), 0..1024),
        hash in any::<[u8; 20]>(),
        value in any::<Amount<NonNegative>>(),
    ) {
        let _init_guard = zebra_test::init();
        let scripts = [
            Script::new(&bytes),
            ZcashScript::from(TransparentAddress::PublicKeyHash(hash).script()).into(),
            ZcashScript::from(TransparentAddress::ScriptHash(hash).script()).into(),
        ];
        for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
            for value in [value, Amount::new(0), Amount::new(MAX_MONEY)] {
                for script in &scripts {
                    super::assert_output_address(&Output::new(value, script.clone()), &network);
                }
            }
        }
    }
}

#[test]
fn output_address_preserves_oversized_script_panic() {
    let _init_guard = zebra_test::init();
    let output = Output::new(
        Amount::new(0),
        Script::new(&vec![0; MAX_PROTOCOL_MESSAGE_LEN + 1]),
    );

    for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
        assert!(std::panic::catch_unwind(|| output.address(&network)).is_err());
    }
}
