//! Tests for [`Transaction::signed_transparent`].
//!
//! Signature validity is not checked here: the regtest integration test in `zebrad` sends the
//! transactions through consensus verification, which is the only oracle that counts.

use crate::{
    amount::Amount,
    block::Height,
    parameters::{testnet::ConfiguredActivationHeights, Network, NetworkKind, NetworkUpgrade},
    serialization::{ZcashDeserializeInto, ZcashSerialize},
    transaction::{self, Transaction, TransparentSigningKey},
    transparent,
};

/// ZIP-317 conventional fee for a transaction with two logical actions.
const TWO_ACTION_FEE: i64 = 10_000;

#[test]
fn signed_transparent_pays_outputs_fee_and_change() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(
        ConfiguredActivationHeights {
            nu5: Some(1),
            ..Default::default()
        }
        .into(),
    );
    let key = TransparentSigningKey::random();
    let recipient = TransparentSigningKey::random().address(NetworkKind::Regtest);

    let outpoint = transparent::OutPoint {
        hash: transaction::Hash([1; 32]),
        index: 3,
    };
    let coin = transparent::Output {
        value: Amount::try_from(100_000_000).expect("valid amount"),
        lock_script: key.address(NetworkKind::Regtest).script(),
    };
    let send_amount = Amount::try_from(30_000_000).expect("valid amount");

    let tx = Transaction::signed_transparent(
        &network,
        Height(10),
        &key,
        &[(outpoint, coin.clone())],
        &[(recipient, send_amount)],
    )
    .expect("a funded transparent spend must build");

    assert_eq!(
        tx.version(),
        5,
        "NU5 is active, so the transaction must be v5"
    );
    assert_eq!(tx.network_upgrade(), Some(NetworkUpgrade::Nu5));
    assert_eq!(
        tx.expiry_height(),
        Some(Height(50)),
        "default expiry delta is 40 blocks"
    );

    let inputs = tx.inputs();
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].outpoint(), Some(outpoint));

    let outputs = tx.outputs();
    assert_eq!(outputs.len(), 2, "one recipient plus change");
    assert_eq!(outputs[0].value, send_amount);
    assert_eq!(outputs[0].lock_script, recipient.script());
    assert_eq!(
        outputs[1].lock_script, coin.lock_script,
        "change returns to the signing key"
    );

    let fee = coin.value.zatoshis() - outputs.iter().map(|o| o.value.zatoshis()).sum::<i64>();
    assert_eq!(
        fee, TWO_ACTION_FEE,
        "exactly the ZIP-317 conventional fee is paid"
    );

    let round_trip: Transaction = tx
        .zcash_serialize_to_vec()
        .expect("serializes")
        .zcash_deserialize_into()
        .expect("deserializes");
    assert_eq!(round_trip, tx);
}

#[test]
fn signed_transparent_rejects_underfunded_spend() {
    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(Default::default());
    let key = TransparentSigningKey::random();
    let address = key.address(NetworkKind::Regtest);

    let coin = transparent::Output {
        value: Amount::try_from(TWO_ACTION_FEE).expect("valid amount"),
        lock_script: address.script(),
    };

    let result = Transaction::signed_transparent(
        &network,
        Height(10),
        &key,
        &[(
            transparent::OutPoint::from_usize(transaction::Hash([2; 32]), 0),
            coin,
        )],
        &[(address, Amount::try_from(1).expect("valid amount"))],
    );

    assert!(
        result.is_err(),
        "inputs that cannot cover outputs plus fee must be rejected"
    );
}
