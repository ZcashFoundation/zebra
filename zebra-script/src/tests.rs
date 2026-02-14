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

/// Build a P2SH scriptPubKey: OP_HASH160 <20-byte-hash> OP_EQUAL
fn p2sh_script_pubkey(script_hash: [u8; 20]) -> transparent::Script {
    let mut bytes = Vec::with_capacity(23);
    bytes.push(0xa9); // OP_HASH160
    bytes.push(0x14); // push 20 bytes
    bytes.extend_from_slice(&script_hash);
    bytes.push(0x87); // OP_EQUAL
    transparent::Script::new(&bytes)
}

/// Build a scriptSig that pushes `redeemed_script` as the last data element.
fn p2sh_script_sig(redeemed_script: &[u8]) -> transparent::Script {
    // Use OP_PUSHDATA1 for the redeemed script push (handles up to 255 bytes).
    let mut bytes = Vec::new();
    if redeemed_script.len() <= 75 {
        bytes.push(redeemed_script.len() as u8);
    } else {
        bytes.push(0x4c); // OP_PUSHDATA1
        bytes.push(redeemed_script.len() as u8);
    }
    bytes.extend_from_slice(redeemed_script);
    transparent::Script::new(&bytes)
}

/// Build a redeemed script with `n` OP_CHECKSIG operations.
fn redeemed_script_with_checksigs(n: u32) -> Vec<u8> {
    // Each OP_CHECKSIG counts as 1 sigop with accurate=true.
    vec![0xac; n as usize]
}

#[test]
fn are_inputs_standard_accepts_within_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Real P2SH transaction from the test above.
    let serialized_tx = "0400008085202f8901c21354bf2305e474ad695382e68efc06e2f8b83c512496f615d153c2e00e688b00000000fdfd0000483045022100d2ab3e6258fe244fa442cfb38f6cef9ac9a18c54e70b2f508e83fa87e20d040502200eead947521de943831d07a350e45af8e36c2166984a8636f0a8811ff03ed09401473044022013e15d865010c257eef133064ef69a780b4bc7ebe6eda367504e806614f940c3022062fdbc8c2d049f91db2042d6c9771de6f1ef0b3b1fea76c1ab5542e44ed29ed8014c69522103b2cc71d23eb30020a4893982a1e2d352da0d20ee657fa02901c432758909ed8f21029d1e9a9354c0d2aee9ffd0f0cea6c39bbf98c4066cf143115ba2279d0ba7dabe2103e32096b63fd57f3308149d238dcbb24d8d28aad95c0e4e74e3e5e6a11b61bcc453aeffffffff0250954903000000001976a914a5a4e1797dac40e8ce66045d1a44c4a63d12142988acccf41c590000000017a9141c973c68b2acc6d6688eff9c7a9dd122ac1346ab8786c72400000000000000000000000000000000";
    let serialized_output = "4065675c0000000017a914c117756dcbe144a12a7c33a77cfa81aa5aeeb38187";
    let tx =
        Transaction::zcash_deserialize(&hex::decode(serialized_tx).unwrap().to_vec()[..]).unwrap();
    let previous_output =
        Output::zcash_deserialize(&hex::decode(serialized_output).unwrap().to_vec()[..]).unwrap();

    // This real P2SH tx has a 2-of-3 multisig redeemed script (3 sigops with accurate counting).
    // That's within the MAX_P2SH_SIGOPS=15 limit.
    assert!(super::are_inputs_standard(&tx, &[previous_output]));

    Ok(())
}

#[test]
fn are_inputs_standard_rejects_over_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Build a redeemed script with 16 OP_CHECKSIG (> MAX_P2SH_SIGOPS=15).
    let redeemed = redeemed_script_with_checksigs(16);

    // Use a dummy hash -- are_inputs_standard doesn't verify the hash matches,
    // it only checks the script format is P2SH and counts sigops in the redeemed script.
    let lock_script = p2sh_script_pubkey([0u8; 20]);
    let unlock_script = p2sh_script_sig(&redeemed);

    // Build a minimal v4 transaction with a single P2SH input.
    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: zebra_chain::transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script,
            sequence: 0xffffffff,
        }],
        outputs: vec![],
        lock_time: zebra_chain::transaction::LockTime::Height(
            zebra_chain::block::Height(0),
        ),
        expiry_height: zebra_chain::block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let spent_output = Output {
        value: 0u64.try_into().unwrap(),
        lock_script,
    };

    assert!(!super::are_inputs_standard(&tx, &[spent_output]));

    Ok(())
}

#[test]
fn are_inputs_standard_accepts_at_limit() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Build a redeemed script with exactly 15 OP_CHECKSIG (== MAX_P2SH_SIGOPS).
    let redeemed = redeemed_script_with_checksigs(15);

    let lock_script = p2sh_script_pubkey([0u8; 20]);
    let unlock_script = p2sh_script_sig(&redeemed);

    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: zebra_chain::transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script,
            sequence: 0xffffffff,
        }],
        outputs: vec![],
        lock_time: zebra_chain::transaction::LockTime::Height(
            zebra_chain::block::Height(0),
        ),
        expiry_height: zebra_chain::block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let spent_output = Output {
        value: 0u64.try_into().unwrap(),
        lock_script,
    };

    assert!(super::are_inputs_standard(&tx, &[spent_output]));

    Ok(())
}

#[test]
fn are_inputs_standard_accepts_non_p2sh() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Non-P2SH standard inputs (P2PKH) should pass.
    let tx =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    let output = Output {
        value: (212 * u64::pow(10, 8)).try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };

    assert!(super::are_inputs_standard(&tx, &[output]));

    Ok(())
}

#[test]
fn are_inputs_standard_rejects_non_standard_script() -> Result<()> {
    let _init_guard = zebra_test::init();

    // A spent output with a non-standard scriptPubKey should be rejected.
    // Use a script that doesn't match any known template (just random opcodes).
    let non_standard_script = transparent::Script::new(&[0x51, 0x52, 0x93]); // OP_1 OP_2 OP_ADD

    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: zebra_chain::transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[0x51]), // OP_TRUE
            sequence: 0xffffffff,
        }],
        outputs: vec![],
        lock_time: zebra_chain::transaction::LockTime::Height(
            zebra_chain::block::Height(0),
        ),
        expiry_height: zebra_chain::block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let spent_output = Output {
        value: 0u64.try_into().unwrap(),
        lock_script: non_standard_script,
    };

    assert!(!super::are_inputs_standard(&tx, &[spent_output]));

    Ok(())
}

#[test]
fn p2sh_sigop_count_sums_across_inputs() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Real P2SH transaction with a 2-of-3 multisig redeemed script (3 sigops).
    let serialized_tx = "0400008085202f8901c21354bf2305e474ad695382e68efc06e2f8b83c512496f615d153c2e00e688b00000000fdfd0000483045022100d2ab3e6258fe244fa442cfb38f6cef9ac9a18c54e70b2f508e83fa87e20d040502200eead947521de943831d07a350e45af8e36c2166984a8636f0a8811ff03ed09401473044022013e15d865010c257eef133064ef69a780b4bc7ebe6eda367504e806614f940c3022062fdbc8c2d049f91db2042d6c9771de6f1ef0b3b1fea76c1ab5542e44ed29ed8014c69522103b2cc71d23eb30020a4893982a1e2d352da0d20ee657fa02901c432758909ed8f21029d1e9a9354c0d2aee9ffd0f0cea6c39bbf98c4066cf143115ba2279d0ba7dabe2103e32096b63fd57f3308149d238dcbb24d8d28aad95c0e4e74e3e5e6a11b61bcc453aeffffffff0250954903000000001976a914a5a4e1797dac40e8ce66045d1a44c4a63d12142988acccf41c590000000017a9141c973c68b2acc6d6688eff9c7a9dd122ac1346ab8786c72400000000000000000000000000000000";
    let serialized_output = "4065675c0000000017a914c117756dcbe144a12a7c33a77cfa81aa5aeeb38187";
    let tx =
        Transaction::zcash_deserialize(&hex::decode(serialized_tx).unwrap().to_vec()[..]).unwrap();
    let previous_output =
        Output::zcash_deserialize(&hex::decode(serialized_output).unwrap().to_vec()[..]).unwrap();

    // The 2-of-3 multisig redeemed script contains 3 OP_CHECKMULTISIG sigops.
    // With accurate counting, CHECKMULTISIG(2,3) counts as 3 sigops (the number of pubkeys).
    let count = super::p2sh_sigop_count(&tx, &[previous_output]);
    assert_eq!(count, 3);

    Ok(())
}

#[test]
fn p2sh_sigop_count_zero_for_non_p2sh() -> Result<()> {
    let _init_guard = zebra_test::init();

    let tx =
        SCRIPT_TX.zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()?;

    let output = Output {
        value: (212 * u64::pow(10, 8)).try_into()?,
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY.clone()),
    };

    // P2PKH inputs should contribute 0 P2SH sigops.
    assert_eq!(super::p2sh_sigop_count(&tx, &[output]), 0);

    Ok(())
}

#[test]
fn are_inputs_standard_rejects_extra_scriptsig_data() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Build a P2PKH spent output. For P2PKH, ScriptSigArgsExpected = 2.
    // OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
    let p2pkh_script = {
        let mut bytes = Vec::new();
        bytes.push(0x76); // OP_DUP
        bytes.push(0xa9); // OP_HASH160
        bytes.push(0x14); // push 20 bytes
        bytes.extend_from_slice(&[0u8; 20]);
        bytes.push(0x88); // OP_EQUALVERIFY
        bytes.push(0xac); // OP_CHECKSIG
        transparent::Script::new(&bytes)
    };

    // Build a scriptSig with 3 push operations (more than expected 2).
    // This simulates extra data stuffed into the scriptSig.
    // Format: [push 1 byte, <sig>, push 1 byte, <pubkey>, push 1 byte, <extra>]
    let scriptsig_extra = transparent::Script::new(&[0x01, 0xaa, 0x01, 0xbb, 0x01, 0xcc]);

    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: zebra_chain::transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: scriptsig_extra,
            sequence: 0xffffffff,
        }],
        outputs: vec![],
        lock_time: zebra_chain::transaction::LockTime::Height(
            zebra_chain::block::Height(0),
        ),
        expiry_height: zebra_chain::block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let spent_output = Output {
        value: 0u64.try_into().unwrap(),
        lock_script: p2pkh_script,
    };

    // Should be rejected: 3 push ops != 2 expected for P2PKH.
    assert!(!super::are_inputs_standard(&tx, &[spent_output]));

    Ok(())
}

#[test]
fn are_inputs_standard_accepts_correct_p2pkh_args() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Build a P2PKH spent output. For P2PKH, ScriptSigArgsExpected = 2.
    let p2pkh_script = {
        let mut bytes = Vec::new();
        bytes.push(0x76); // OP_DUP
        bytes.push(0xa9); // OP_HASH160
        bytes.push(0x14); // push 20 bytes
        bytes.extend_from_slice(&[0u8; 20]);
        bytes.push(0x88); // OP_EQUALVERIFY
        bytes.push(0xac); // OP_CHECKSIG
        transparent::Script::new(&bytes)
    };

    // Build a scriptSig with exactly 2 push operations (correct for P2PKH).
    // Format: [push 1 byte, <sig>, push 1 byte, <pubkey>]
    let scriptsig_correct = transparent::Script::new(&[0x01, 0xaa, 0x01, 0xbb]);

    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: zebra_chain::transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: scriptsig_correct,
            sequence: 0xffffffff,
        }],
        outputs: vec![],
        lock_time: zebra_chain::transaction::LockTime::Height(
            zebra_chain::block::Height(0),
        ),
        expiry_height: zebra_chain::block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let spent_output = Output {
        value: 0u64.try_into().unwrap(),
        lock_script: p2pkh_script,
    };

    // Should pass: 2 push ops == 2 expected for P2PKH.
    assert!(super::are_inputs_standard(&tx, &[spent_output]));

    Ok(())
}
