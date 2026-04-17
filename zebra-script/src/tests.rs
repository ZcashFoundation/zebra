#![allow(clippy::unwrap_in_result)]

use hex::FromHex;
use std::sync::Arc;
use zebra_chain::{
    block,
    parameters::NetworkUpgrade,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{self, HashType, LockTime, SigHasher, Transaction},
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

/// Construct a V5 P2PKH transaction with a given sighash type byte in the signature,
/// then verify it through the full script verification path (CachedFfiTransaction::is_valid).
///
/// This reproduces the regtest experiments from docs/analysis/sighash_consensus_divergence_report.md:
/// - canonical_hash_type: the HashType used to compute the sighash for signing (must be valid)
/// - sig_hash_type_byte: the raw byte appended to the DER signature in the unlock script
///
/// When sig_hash_type_byte differs from the canonical byte but canonicalizes to the same
/// HashType via from_bits(byte, false), Zebra accepts the transaction. zcashd rejects it
/// for v5 transactions because SighashType::parse rejects undefined raw values.
fn build_and_verify_v5_p2pkh(
    canonical_hash_type: HashType,
    sig_hash_type_byte: u8,
) -> std::result::Result<(), crate::Error> {
    use ripemd::{Digest as _, Ripemd160};
    use secp256k1::{Message, Secp256k1, SecretKey};
    use sha2::Sha256;

    let secp = Secp256k1::new();

    // Deterministic keypair (32 bytes, nonzero)
    let secret_key = SecretKey::from_slice(&[0xcd; 32]).expect("valid secret key");
    let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_bytes = public_key.serialize(); // 33 bytes, compressed

    // Derive P2PKH lock script: OP_DUP OP_HASH160 <20-byte-hash> OP_EQUALVERIFY OP_CHECKSIG
    let sha_hash = Sha256::digest(pubkey_bytes);
    let pub_key_hash: [u8; 20] = Ripemd160::digest(sha_hash).into();
    let mut lock_script_bytes = vec![
        0x76, // OP_DUP
        0xa9, // OP_HASH160
        0x14, // Push 20 bytes
    ];
    lock_script_bytes.extend_from_slice(&pub_key_hash);
    lock_script_bytes.push(0x88); // OP_EQUALVERIFY
    lock_script_bytes.push(0xac); // OP_CHECKSIG
    let lock_script = transparent::Script::new(&lock_script_bytes);

    let previous_output = transparent::Output {
        value: 1_0000_0000u64.try_into().expect("valid amount"),
        lock_script: lock_script.clone(),
    };

    // Build a V5 transaction with a placeholder unlock script.
    // For v5/ZIP-244, the sighash does NOT depend on the unlock script contents,
    // so we can compute the sighash with a placeholder, sign, then rebuild.
    let placeholder_tx = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        lock_time: LockTime::unlocked(),
        expiry_height: block::Height(0),
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        outputs: vec![transparent::Output {
            value: 9000_0000u64.try_into().expect("valid amount"),
            lock_script: transparent::Script::new(&[0x00]), // OP_FALSE (burn)
        }],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    // Compute the sighash for the canonical hash type
    let all_previous_outputs = Arc::new(vec![previous_output.clone()]);
    let sighasher = SigHasher::new(&placeholder_tx, NetworkUpgrade::Nu5, all_previous_outputs)
        .expect("sighasher creation should succeed");
    let sighash =
        sighasher.sighash(canonical_hash_type, Some((0, lock_script_bytes.clone())));

    // Sign the sighash with the private key
    let msg = Message::from_digest(*sighash.as_ref());
    let signature = secp.sign_ecdsa(&msg, &secret_key);
    let der_sig = signature.serialize_der();

    // Build the unlock script: <sig_len> <DER_sig || hash_type_byte> <pubkey_len> <pubkey>
    let mut unlock_script_bytes = Vec::new();
    // Push signature + hash_type byte
    let sig_with_hashtype_len = der_sig.len() + 1;
    unlock_script_bytes.push(sig_with_hashtype_len as u8);
    unlock_script_bytes.extend_from_slice(&der_sig);
    unlock_script_bytes.push(sig_hash_type_byte);
    // Push compressed public key (33 bytes)
    unlock_script_bytes.push(pubkey_bytes.len() as u8);
    unlock_script_bytes.extend_from_slice(&pubkey_bytes);

    // Rebuild the V5 transaction with the real unlock script
    let final_tx = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        lock_time: LockTime::unlocked(),
        expiry_height: block::Height(0),
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&unlock_script_bytes),
            sequence: u32::MAX,
        }],
        outputs: vec![transparent::Output {
            value: 9000_0000u64.try_into().expect("valid amount"),
            lock_script: transparent::Script::new(&[0x00]),
        }],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    let verifier = super::CachedFfiTransaction::new(
        Arc::new(final_tx),
        Arc::new(vec![previous_output]),
        NetworkUpgrade::Nu5,
    )
    .expect("network upgrade should be valid for v5 tx");

    verifier.is_valid(0)
}

/// Baseline: a standard V5 P2PKH spend with SIGHASH_ALL (0x01) passes verification.
/// Both Zebra and zcashd accept this.
#[test]
fn sighash_divergence_v5_p2pkh_canonical_sighash_all() {
    let _init_guard = zebra_test::init();

    build_and_verify_v5_p2pkh(HashType::ALL, 0x01)
        .expect("canonical SIGHASH_ALL (0x01) should be accepted");
}

/// Baseline: a standard V5 P2PKH spend with SIGHASH_ALL|ANYONECANPAY (0x81) passes.
/// Both Zebra and zcashd accept this.
#[test]
fn sighash_divergence_v5_p2pkh_canonical_sighash_all_anyonecanpay() {
    let _init_guard = zebra_test::init();

    build_and_verify_v5_p2pkh(HashType::ALL | HashType::ANYONECANPAY, 0x81)
        .expect("canonical SIGHASH_ALL|ANYONECANPAY (0x81) should be accepted");
}

/// V5 P2PKH spend with malformed hash_type 0x84 is now rejected.
///
/// 0x84 has undefined bits set: 0x84 ∉ {0x01, 0x02, 0x03, 0x81, 0x82, 0x83}.
/// The callback now checks raw_bits() and returns None for v5+ transactions
/// with undefined hash_type values, matching zcashd's SighashType::parse behavior.
///
/// Before the fix, Zebra accepted this (canonicalizing 0x84 → 0x81).
#[test]
fn sighash_divergence_v5_p2pkh_malformed_0x84_rejected() {
    let _init_guard = zebra_test::init();

    let result = build_and_verify_v5_p2pkh(HashType::ALL | HashType::ANYONECANPAY, 0x84);

    assert!(
        result.is_err(),
        "Malformed hash_type 0x84 should be rejected for v5 transactions, \
         matching zcashd behavior"
    );
}

/// V5 P2PKH spend with malformed hash_type 0x50 is now rejected.
///
/// 0x50 has undefined bits set: 0x50 ∉ {0x01, 0x02, 0x03, 0x81, 0x82, 0x83}.
/// The callback now checks raw_bits() and returns None for v5+ transactions
/// with undefined hash_type values, matching zcashd's SighashType::parse behavior.
///
/// Before the fix, Zebra accepted this (canonicalizing 0x50 → 0x01).
#[test]
fn sighash_divergence_v5_p2pkh_malformed_0x50_rejected() {
    let _init_guard = zebra_test::init();

    let result = build_and_verify_v5_p2pkh(HashType::ALL, 0x50);

    assert!(
        result.is_err(),
        "Malformed hash_type 0x50 should be rejected for v5 transactions, \
         matching zcashd behavior"
    );
}

/// Negative test: V5 P2PKH spend with malformed hash_type 0x84 but signed with
/// the WRONG canonical type (ALL instead of ALL|ANYONECANPAY).
///
/// This produces a sighash mismatch: the signature was created over ALL's sighash,
/// but Zebra canonicalizes 0x84 to ALL|ANYONECANPAY and computes that sighash.
/// The signature doesn't verify → both Zebra and zcashd reject.
///
/// This confirms the issue is specifically about canonicalization mapping to the
/// correct sighash, not a generic signature bypass.
#[test]
fn sighash_divergence_v5_p2pkh_malformed_0x84_wrong_canonical_type_rejected() {
    let _init_guard = zebra_test::init();

    // Sign with ALL (0x01), but put 0x84 which canonicalizes to ALL|ANYONECANPAY (0x81).
    // The sighash mismatch causes verification failure.
    let result = build_and_verify_v5_p2pkh(HashType::ALL, 0x84);

    assert!(
        result.is_err(),
        "Malformed hash_type 0x84 signed with wrong canonical type should be rejected \
         by both Zebra and zcashd (sighash mismatch)"
    );
}

/// Build a V4 P2PKH transparent spend, sign it under the supplied raw `hash_type`
/// byte using the V4 raw-byte sighash semantics, and run it through the script
/// verifier.
///
/// `zcashd` serializes the full raw `hash_type` byte into the V4 sighash
/// preimage (only masking with 0x1f for selection logic). Before the V4 fix,
/// Zebra canonicalized the byte before computing the sighash, so a tx using
/// e.g. `0x41` would be accepted by `zcashd` but rejected by Zebra (digest
/// mismatch). With the V4 fix, both implementations compute the same digest
/// and Zebra accepts.
fn build_and_verify_v4_p2pkh(
    sig_hash_type_byte: u8,
) -> std::result::Result<(), crate::Error> {
    use ripemd::{Digest as _, Ripemd160};
    use secp256k1::{Message, Secp256k1, SecretKey};
    use sha2::Sha256;

    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&[0xcd; 32]).expect("valid secret key");
    let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_bytes = public_key.serialize();

    let sha_hash = Sha256::digest(pubkey_bytes);
    let pub_key_hash: [u8; 20] = Ripemd160::digest(sha_hash).into();
    let mut lock_script_bytes = vec![0x76, 0xa9, 0x14];
    lock_script_bytes.extend_from_slice(&pub_key_hash);
    lock_script_bytes.push(0x88);
    lock_script_bytes.push(0xac);
    let lock_script = transparent::Script::new(&lock_script_bytes);

    let previous_output = transparent::Output {
        value: 1_0000_0000u64.try_into().expect("valid amount"),
        lock_script: lock_script.clone(),
    };

    let placeholder_tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        outputs: vec![transparent::Output {
            value: 9000_0000u64.try_into().expect("valid amount"),
            lock_script: transparent::Script::new(&[0x00]),
        }],
        lock_time: LockTime::unlocked(),
        expiry_height: block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let all_previous_outputs = Arc::new(vec![previous_output.clone()]);
    let sighasher =
        SigHasher::new(&placeholder_tx, NetworkUpgrade::Canopy, all_previous_outputs)
            .expect("sighasher creation should succeed");

    // V4: use the raw byte to match zcashd's preimage semantics.
    let sighash =
        sighasher.sighash_v4_raw(sig_hash_type_byte, Some((0, lock_script_bytes.clone())));

    let msg = Message::from_digest(*sighash.as_ref());
    let signature = secp.sign_ecdsa(&msg, &secret_key);
    let der_sig = signature.serialize_der();

    let mut unlock_script_bytes = Vec::new();
    let sig_with_hashtype_len = der_sig.len() + 1;
    unlock_script_bytes.push(sig_with_hashtype_len as u8);
    unlock_script_bytes.extend_from_slice(&der_sig);
    unlock_script_bytes.push(sig_hash_type_byte);
    unlock_script_bytes.push(pubkey_bytes.len() as u8);
    unlock_script_bytes.extend_from_slice(&pubkey_bytes);

    let final_tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&unlock_script_bytes),
            sequence: u32::MAX,
        }],
        outputs: vec![transparent::Output {
            value: 9000_0000u64.try_into().expect("valid amount"),
            lock_script: transparent::Script::new(&[0x00]),
        }],
        lock_time: LockTime::unlocked(),
        expiry_height: block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let verifier = super::CachedFfiTransaction::new(
        Arc::new(final_tx),
        Arc::new(vec![previous_output]),
        NetworkUpgrade::Canopy,
    )
    .expect("network upgrade should be valid for v4 tx");

    verifier.is_valid(0)
}

/// Variant A regression: V4 P2PKH spend with non-canonical hash_type 0x41 is accepted.
///
/// `zcashd` serializes the raw 0x41 byte into the V4 sighash preimage. Before the
/// V4 fix, Zebra canonicalized 0x41 → ALL (0x01) before computing the sighash,
/// producing a different digest than the one signed and rejecting the tx — a
/// consensus split where `zcashd` accepted what Zebra rejected.
///
/// With the V4 fix, the callback routes V4 transactions through `sighash_v4_raw`
/// using the raw byte, so the digests match and Zebra accepts.
#[test]
fn sighash_divergence_v4_p2pkh_extra_bits_0x41_accepted() {
    let _init_guard = zebra_test::init();

    build_and_verify_v4_p2pkh(0x41)
        .expect("V4 spend with non-canonical hash_type 0x41 should be accepted, matching zcashd");
}

/// Unit-level: for canonical bytes, `sighash_v4_raw` produces the same digest
/// as the typed `sighash` API. This guards against regressions in the new V4
/// raw-byte path on canonical inputs.
#[test]
fn sighash_divergence_v4_raw_canonical_matches_typed() {
    let _init_guard = zebra_test::init();

    let lock_script = transparent::Script::new(&[0x76, 0xa9, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0xac]);
    let previous_output = transparent::Output {
        value: 1_0000_0000u64.try_into().expect("valid amount"),
        lock_script: lock_script.clone(),
    };

    let tx = Transaction::V4 {
        inputs: vec![transparent::Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: transaction::Hash([0u8; 32]),
                index: 0,
            },
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        outputs: vec![transparent::Output {
            value: 9000_0000u64.try_into().expect("valid amount"),
            lock_script: transparent::Script::new(&[0x00]),
        }],
        lock_time: LockTime::unlocked(),
        expiry_height: block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let sighasher = SigHasher::new(
        &tx,
        NetworkUpgrade::Canopy,
        Arc::new(vec![previous_output]),
    )
    .expect("sighasher creation should succeed");

    let script_code = lock_script.as_raw_bytes().to_vec();

    let canonical_pairs: &[(HashType, u8)] = &[
        (HashType::ALL, 0x01),
        (HashType::NONE, 0x02),
        (HashType::SINGLE, 0x03),
        (HashType::ALL_ANYONECANPAY, 0x81),
        (HashType::NONE_ANYONECANPAY, 0x82),
        (HashType::SINGLE_ANYONECANPAY, 0x83),
    ];

    for &(typed, raw) in canonical_pairs {
        let typed_digest = sighasher.sighash(typed, Some((0, script_code.clone())));
        let raw_digest = sighasher.sighash_v4_raw(raw, Some((0, script_code.clone())));
        let typed_bytes: [u8; 32] = typed_digest.into();
        let raw_bytes: [u8; 32] = raw_digest.into();
        assert_eq!(
            typed_bytes, raw_bytes,
            "sighash_v4_raw({raw:#x}) should equal typed sighash for canonical input"
        );
    }
}

/// Test that `HashType::from_bits` with `is_strict=false` canonicalizes undefined raw hash_type
/// bytes instead of rejecting them. This is the root cause of the sighash consensus divergence:
///
/// - zcashd rejects undefined raw hash_type values for v5 transactions in `SighashType::parse`
/// - Zebra receives a canonicalized `HashType` through the FFI callback (which uses
///   `from_bits(hash_type, false)`) and computes a sighash for the canonical type
///
/// See docs/analysis/sighash_consensus_divergence_report.md for full details.
#[test]
fn sighash_divergence_from_bits_canonicalization() {
    let _init_guard = zebra_test::init();

    // These raw hash_type bytes are NOT valid for v5 transactions.
    // zcashd rejects them in SighashType::parse (transaction_ffi.rs:267-273).
    // Valid v5 values are: {0x01, 0x02, 0x03, 0x81, 0x82, 0x83}.
    let invalid_raw_bytes: &[(i32, &str)] = &[
        (0x84, "0x84: undefined bits set, canonicalizes to ALL|ANYONECANPAY (0x81)"),
        (0x50, "0x50: undefined bits set, canonicalizes to ALL (0x01)"),
        (0x00, "0x00: no signed_outputs bits set, canonicalizes to ALL (0x01)"),
        (0x04, "0x04: undefined lower bits, canonicalizes to ALL (0x01)"),
        (0xFF, "0xFF: all bits set, canonicalizes to SINGLE|ANYONECANPAY (0x83)"),
        (0x85, "0x85: undefined bits set, canonicalizes to ALL|ANYONECANPAY (0x81)"),
    ];

    for &(raw_byte, description) in invalid_raw_bytes {
        // Non-strict mode (what libzcash_script uses in the callback): SUCCEEDS
        // This is the lossy conversion that causes the divergence.
        let non_strict_result =
            zcash_script::signature::HashType::from_bits(raw_byte, false);
        assert!(
            non_strict_result.is_ok(),
            "from_bits({raw_byte:#x}, false) should succeed (canonicalize) but failed: {description}"
        );

        // Strict mode: REJECTS (matching zcashd v5 behavior)
        let strict_result =
            zcash_script::signature::HashType::from_bits(raw_byte, true);
        assert!(
            strict_result.is_err(),
            "from_bits({raw_byte:#x}, true) should reject undefined bits but succeeded: {description}"
        );
    }

    // Verify that all six valid canonical values pass both strict and non-strict.
    let valid_raw_bytes: &[i32] = &[0x01, 0x02, 0x03, 0x81, 0x82, 0x83];
    for &raw_byte in valid_raw_bytes {
        assert!(
            zcash_script::signature::HashType::from_bits(raw_byte, false).is_ok(),
            "valid byte {raw_byte:#x} should pass non-strict"
        );
        assert!(
            zcash_script::signature::HashType::from_bits(raw_byte, true).is_ok(),
            "valid byte {raw_byte:#x} should pass strict"
        );
    }
}


