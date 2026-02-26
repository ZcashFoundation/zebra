#![allow(clippy::unwrap_in_result)]

use hex::FromHex;
use std::sync::Arc;
use zebra_chain::{
    parameters::NetworkUpgrade,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::Transaction,
    transparent::{self, Output},
};
use zebra_test::prelude::*;

use crate::Sigops;

lazy_static::lazy_static! {
    pub static ref SCRIPT_PUBKEY: Vec<u8> = <Vec<u8>>::from_hex("76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac")
        .unwrap();
    pub static ref SCRIPT_TX: Vec<u8> = <Vec<u8>>::from_hex("0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000")
        .expect("Block bytes are in valid hex representation");
}

fn verify_valid_script(nu: NetworkUpgrade, tx: &[u8], amount: u64, pubkey: &[u8]) -> Result<()> {
    let transaction = tx.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(pubkey),
    };
    let input_index = 0;

    let previous_output = Arc::new(vec![output]);
    let verifier = super::CachedFfiTransaction::new(transaction, previous_output, nu)
        .expect("network upgrade should be valid for tx");
    verifier.is_valid(input_index)?;

    Ok(())
}

#[test]
fn verify_valid_script_v4() -> Result<()> {
    let _init_guard = zebra_test::init();

    verify_valid_script(
        NetworkUpgrade::Blossom,
        &SCRIPT_TX,
        212 * u64::pow(10, 8),
        &SCRIPT_PUBKEY,
    )
}

#[test]
fn count_legacy_sigops() -> Result<()> {
    let _init_guard = zebra_test::init();

    let tx = SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    assert_eq!(tx.sigops()?, 1);

    Ok(())
}

#[test]
fn fail_invalid_script() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
    let coin = u64::pow(10, 8);
    let amount = 211 * coin;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()[..]),
    };
    let input_index = 0;
    let verifier = super::CachedFfiTransaction::new(
        transaction,
        Arc::new(vec![output]),
        NetworkUpgrade::Blossom,
    )
    .expect("network upgrade should be valid for tx");
    verifier
        .is_valid(input_index)
        .expect_err("verification should fail");

    Ok(())
}

#[test]
fn reuse_script_verifier_pass_pass() -> Result<()> {
    let _init_guard = zebra_test::init();

    let coin = u64::pow(10, 8);
    let transaction =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;
    let amount = 212 * coin;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };

    let verifier = super::CachedFfiTransaction::new(
        transaction,
        Arc::new(vec![output]),
        NetworkUpgrade::Blossom,
    )
    .expect("network upgrade should be valid for tx");

    let input_index = 0;

    verifier.is_valid(input_index)?;
    verifier.is_valid(input_index)?;

    Ok(())
}

#[test]
fn reuse_script_verifier_pass_fail() -> Result<()> {
    let _init_guard = zebra_test::init();

    let coin = u64::pow(10, 8);
    let amount = 212 * coin;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };
    let transaction =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    let verifier = super::CachedFfiTransaction::new(
        transaction,
        Arc::new(vec![output]),
        NetworkUpgrade::Blossom,
    )
    .expect("network upgrade should be valid for tx");

    let input_index = 0;

    verifier.is_valid(input_index)?;
    verifier
        .is_valid(input_index + 1)
        .expect_err("verification should fail");

    Ok(())
}

#[test]
fn reuse_script_verifier_fail_pass() -> Result<()> {
    let _init_guard = zebra_test::init();

    let coin = u64::pow(10, 8);
    let amount = 212 * coin;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };
    let transaction =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    let verifier = super::CachedFfiTransaction::new(
        transaction,
        Arc::new(vec![output]),
        NetworkUpgrade::Blossom,
    )
    .expect("network upgrade should be valid for tx");

    let input_index = 0;

    verifier
        .is_valid(input_index + 1)
        .expect_err("verification should fail");
    verifier.is_valid(input_index)?;

    Ok(())
}

#[test]
fn reuse_script_verifier_fail_fail() -> Result<()> {
    let _init_guard = zebra_test::init();

    let coin = u64::pow(10, 8);
    let amount = 212 * coin;
    let output = transparent::Output {
        value: amount.try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };
    let transaction =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    let verifier = super::CachedFfiTransaction::new(
        transaction,
        Arc::new(vec![output]),
        NetworkUpgrade::Blossom,
    )
    .expect("network upgrade should be valid for tx");

    let input_index = 0;

    verifier
        .is_valid(input_index + 1)
        .expect_err("verification should fail");

    verifier
        .is_valid(input_index + 1)
        .expect_err("verification should fail");

    Ok(())
}

#[test]
fn p2sh() -> Result<()> {
    let _init_guard = zebra_test::init();

    // real tx with txid 51ded0b026f1ff56639447760bcd673b9f4e44a8afbf3af1dbaa6ca1fd241bea
    let serialized_tx = "0400008085202f8901c21354bf2305e474ad695382e68efc06e2f8b83c512496f615d153c2e00e688b00000000fdfd0000483045022100d2ab3e6258fe244fa442cfb38f6cef9ac9a18c54e70b2f508e83fa87e20d040502200eead947521de943831d07a350e45af8e36c2166984a8636f0a8811ff03ed09401473044022013e15d865010c257eef133064ef69a780b4bc7ebe6eda367504e806614f940c3022062fdbc8c2d049f91db2042d6c9771de6f1ef0b3b1fea76c1ab5542e44ed29ed8014c69522103b2cc71d23eb30020a4893982a1e2d352da0d20ee657fa02901c432758909ed8f21029d1e9a9354c0d2aee9ffd0f0cea6c39bbf98c4066cf143115ba2279d0ba7dabe2103e32096b63fd57f3308149d238dcbb24d8d28aad95c0e4e74e3e5e6a11b61bcc453aeffffffff0250954903000000001976a914a5a4e1797dac40e8ce66045d1a44c4a63d12142988acccf41c590000000017a9141c973c68b2acc6d6688eff9c7a9dd122ac1346ab8786c72400000000000000000000000000000000";
    let serialized_output = "4065675c0000000017a914c117756dcbe144a12a7c33a77cfa81aa5aeeb38187";
    let tx =
        Transaction::zcash_deserialize(&hex::decode(serialized_tx).unwrap().to_vec()[..]).unwrap();

    let previous_output =
        Output::zcash_deserialize(&hex::decode(serialized_output).unwrap().to_vec()[..]).unwrap();

    let verifier = super::CachedFfiTransaction::new(
        Arc::new(tx),
        Arc::new(vec![previous_output]),
        NetworkUpgrade::Nu5,
    )
    .expect("network upgrade should be valid for tx");

    verifier.is_valid(0)?;

    Ok(())
}
