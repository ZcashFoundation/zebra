//! Property tests for transparent inputs and outputs.

use std::sync::Arc;

use zcash_transparent::{
    address::{Script as ZcashScript, TransparentAddress},
    bundle::TxOut,
};
use zebra_test::prelude::*;

use crate::{
    amount::{Amount, NonNegative, MAX_MONEY},
    block,
    fmt::SummaryDebug,
    parameters::Network,
    serialization::{ZcashDeserializeInto, ZcashSerialize, MAX_PROTOCOL_MESSAGE_LEN},
    transaction::{self, arbitrary::MAX_ARBITRARY_ITEMS},
    LedgerState,
};

use super::super::{Input, OutPoint, Output, Script};

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

/// The serialize/parse conversion is an independent oracle for direct construction.
fn serialized_txout(output: &Output) -> std::io::Result<TxOut> {
    let bytes = output
        .zcash_serialize_to_vec()
        .expect("zcash_primitives and Zebra transparent output formats must be compatible");
    TxOut::read(&mut bytes.as_slice())
}

proptest! {
    #[test]
    fn output_conversion_matches_serialized(
        output in any::<Output>(),
        hash in any::<[u8; 20]>(),
    ) {
        let _init_guard = zebra_test::init();
        let scripts = [
            output.lock_script,
            Script::new(&[]),
            Script::new(&[0x4c]), // Truncated OP_PUSHDATA1.
            Script::new(&[0x51]), // Nonstandard OP_TRUE.
            ZcashScript::from(TransparentAddress::PublicKeyHash(hash).script()).into(),
            ZcashScript::from(TransparentAddress::ScriptHash(hash).script()).into(),
        ];
        for value in [
            output.value,
            Amount::new(0),
            Amount::new(1),
            Amount::new(MAX_MONEY - 1),
            Amount::new(MAX_MONEY),
        ] {
            for script in &scripts {
                let output = Output::new(value, script.clone());
                let expected = serialized_txout(&output)?;
                for converted in [TxOut::try_from(&output)?, TxOut::try_from(output)?] {
                    prop_assert_eq!(converted.value(), expected.value());
                    prop_assert_eq!(
                        &converted.script_pubkey().0 .0,
                        &expected.script_pubkey().0 .0,
                    );
                    prop_assert_eq!(converted.recipient_address(), expected.recipient_address());
                }
            }
        }
    }

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

    #[test]
    fn ordered_outputs_match_without_temporary_allocations(
        outputs in prop::collection::vec(any::<Output>(), 0..8),
        height in any::<block::Height>(),
    ) {
        let _init_guard = zebra_test::init();
        let mut block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
            .zcash_deserialize_into::<block::Block>()
            .unwrap();
        block.transactions.clear();
        let coinbase = Input::Coinbase {
            height: block::Height(1),
            data: vec![0],
            sequence: u32::MAX,
        };
        let spend = Input::PrevOut {
            outpoint: OutPoint { hash: transaction::Hash([1; 32]), index: 2 },
            unlock_script: Script::new(&[]),
            sequence: u32::MAX,
        };
        let single_output = vec![Output::new(Amount::new(0), Script::new(&[0x51]))];
        let mut multiple_outputs = outputs;
        multiple_outputs.push(Output::new(Amount::new(MAX_MONEY), Script::new(&[])));
        multiple_outputs.extend(single_output.clone());

        for tx in super::ordered_output_transactions() {
            block.transactions.extend([
                tx.clone().with_transparent_inputs(vec![coinbase.clone()])
                    .with_transparent_outputs(single_output.clone()),
                tx.clone(), // No transparent bundle.
                tx.clone().with_transparent_inputs(vec![spend.clone()]), // Empty output list.
                tx.clone().with_transparent_inputs(vec![spend.clone()])
                    .with_transparent_outputs(multiple_outputs.clone()),
                tx.with_transparent_outputs(single_output.clone()),
            ].map(Arc::new));
        }
        // The last occurrence wins, including its non-coinbase position.
        block.transactions.push(block.transactions[0].clone());
        let mut hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        hashes.push(transaction::Hash([2; 32]));

        // Preserve zip pairing for short, exact, and overlong hash lists.
        for count in [0, 1, block.transactions.len() - 1, block.transactions.len(), hashes.len()] {
            super::assert_ordered_outputs(&block, height, &hashes[..count]);
        }
        // Collisions with unequal output counts must retain earlier unmatched indexes.
        hashes.fill(transaction::Hash([3; 32]));
        super::assert_ordered_outputs(&block, height, &hashes);
        // Even a non-coinbase transaction in the first position gets the coinbase flag.
        block.transactions.rotate_left(3);
        let hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
        super::assert_ordered_outputs(&block, height, &hashes);
    }
}

#[test]
fn output_conversion_preserves_oversized_script_panic() {
    let _init_guard = zebra_test::init();
    let output = Output::new(
        Amount::new(0),
        Script::new(&vec![0; MAX_PROTOCOL_MESSAGE_LEN + 1]),
    );

    assert!(std::panic::catch_unwind(|| serialized_txout(&output)).is_err());
    assert!(std::panic::catch_unwind(|| TxOut::try_from(&output)).is_err());
    for network in Network::iter().chain([Network::new_regtest(Default::default())]) {
        assert!(std::panic::catch_unwind(|| output.address(&network)).is_err());
    }
    assert!(std::panic::catch_unwind(|| TxOut::try_from(output)).is_err());
}
