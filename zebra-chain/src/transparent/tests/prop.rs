//! Property tests for transparent inputs and outputs.

use proptest::collection::vec;
use zebra_test::prelude::*;

use crate::{
    block,
    fmt::SummaryDebug,
    serialization::{ZcashDeserialize, ZcashSerialize},
    transaction::arbitrary::MAX_ARBITRARY_ITEMS,
    LedgerState,
};

use super::super::Input;

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

/// Returns a serialized [`Input::Coinbase`] with the given `script_sig` bytes
/// and a zero sequence, for feeding to [`Input::zcash_deserialize`].
fn coinbase_input_bytes(script_sig: &[u8]) -> Vec<u8> {
    assert!(
        script_sig.len() < 253,
        "script_sig length needs longer compact-size encoding than this helper writes",
    );

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0u8; 32]);
    bytes.extend_from_slice(&0xffff_ffff_u32.to_le_bytes());
    bytes.push(script_sig.len() as u8);
    bytes.extend_from_slice(script_sig);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

#[test]
fn coinbase_input_round_trip_from_random_input() -> Result<()> {
    let _init_guard = zebra_test::init();

    let strategy =
        any::<block::Height>().prop_flat_map(|height| Input::arbitrary_with(Some(height)));

    proptest!(|(input in strategy)| {
        let mut bytes = Vec::new();
        input.zcash_serialize(&mut bytes)?;
        let decoded = Input::zcash_deserialize(&bytes[..])?;

        prop_assert_eq!(input, decoded);
    });

    Ok(())
}

proptest! {
    /// If a random script_sig parses successfully as a coinbase input, it must
    /// re-serialize to the same bytes. Catches non-minimal height encodings or
    /// other ambiguity in the parser: a different re-serialization implies the
    /// parser accepted bytes that the serializer would never produce.
    #[test]
    fn coinbase_input_round_trip_from_random_script_sig(
        // 2..=100 satisfies the consensus-rule coinbase script length.
        script_sig in vec(any::<u8>(), 2..=100),
    ) {
        let bytes = coinbase_input_bytes(&script_sig);

        if let Ok(input) = Input::zcash_deserialize(&bytes[..]) {
            let mut reserialized = Vec::new();
            input.zcash_serialize(&mut reserialized)?;
            prop_assert_eq!(bytes, reserialized);
        }
    }
}
