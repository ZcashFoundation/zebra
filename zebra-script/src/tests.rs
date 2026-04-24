#![allow(clippy::unwrap_in_result)]

use hex::FromHex;
use std::sync::Arc;
use zebra_chain::{
    block::{self, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{self, HashType, LockTime, SigHasher, Transaction},
    transparent::{self, Output},
};
use zebra_test::prelude::*;

use crate::{p2sh_sigop_count, Sigops};

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
    let sighash = sighasher.sighash(canonical_hash_type, Some((0, lock_script_bytes.clone())));

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
fn build_and_verify_v4_p2pkh(sig_hash_type_byte: u8) -> std::result::Result<(), crate::Error> {
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
    let sighasher = SigHasher::new(
        &placeholder_tx,
        NetworkUpgrade::Canopy,
        all_previous_outputs,
    )
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

    let lock_script = transparent::Script::new(&[
        0x76, 0xa9, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x88, 0xac,
    ]);
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

    let sighasher = SigHasher::new(&tx, NetworkUpgrade::Canopy, Arc::new(vec![previous_output]))
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
        (
            0x84,
            "0x84: undefined bits set, canonicalizes to ALL|ANYONECANPAY (0x81)",
        ),
        (
            0x50,
            "0x50: undefined bits set, canonicalizes to ALL (0x01)",
        ),
        (
            0x00,
            "0x00: no signed_outputs bits set, canonicalizes to ALL (0x01)",
        ),
        (
            0x04,
            "0x04: undefined lower bits, canonicalizes to ALL (0x01)",
        ),
        (
            0xFF,
            "0xFF: all bits set, canonicalizes to SINGLE|ANYONECANPAY (0x83)",
        ),
        (
            0x85,
            "0x85: undefined bits set, canonicalizes to ALL|ANYONECANPAY (0x81)",
        ),
    ];

    for &(raw_byte, description) in invalid_raw_bytes {
        // Non-strict mode (what libzcash_script uses in the callback): SUCCEEDS
        // This is the lossy conversion that causes the divergence.
        let non_strict_result = zcash_script::signature::HashType::from_bits(raw_byte, false);
        assert!(
            non_strict_result.is_ok(),
            "from_bits({raw_byte:#x}, false) should succeed (canonicalize) but failed: {description}"
        );

        // Strict mode: REJECTS (matching zcashd v5 behavior)
        let strict_result = zcash_script::signature::HashType::from_bits(raw_byte, true);
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

/// Regression test for [GHSA-jv4h-j224-23cc] (Trigger A, "Coinbase Hidden Legacy Sigops").
///
/// zcashd's `GetLegacySigOpCount()` counts sigops in the coinbase input's `scriptSig`. Zebra
/// previously skipped the coinbase input entirely, so a miner could hide up to ~98 sigops (the
/// coinbase script length limit is 100 bytes) inside the coinbase `scriptSig` and avoid them being
/// charged against `MAX_BLOCK_SIGOPS`.
///
/// This test builds a v5 coinbase transaction whose `miner_data` consists entirely of `OP_CHECKSIG`
/// (`0xac`) bytes and asserts that `tx.sigops()` now returns the expected count covering every
/// `OP_CHECKSIG`.
///
/// [GHSA-jv4h-j224-23cc]: https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jv4h-j224-23cc
#[test]
fn count_coinbase_legacy_sigops_includes_coinbase_script() -> Result<()> {
    let _init_guard = zebra_test::init();

    // 80 bytes of OP_CHECKSIG fits within the 100-byte coinbase script limit
    // even after the height prefix.
    const OP_CHECKSIG: u8 = 0xac;
    let miner_data = vec![OP_CHECKSIG; 80];

    let dummy_output_script = transparent::Script::new(&[0x51]); // OP_TRUE
    let output_amount = zebra_chain::amount::Amount::try_from(1_000_000)?;

    // Use a height after NU5 activation on Mainnet so v5 is the effective version.
    let network = Network::Mainnet;
    let height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 has a Mainnet activation height");

    let tx = Transaction::new_v5_coinbase(
        &network,
        height,
        vec![(output_amount, dummy_output_script)],
        miner_data,
    );

    // Before the fix, Zebra's `Sigops` impl skipped the coinbase input and returned 0 for a
    // coinbase with no OP_CHECKSIG in its outputs. After the fix, every OP_CHECKSIG in the coinbase
    // `scriptSig` must be counted.
    let sigops = tx.sigops().expect("sigop count is finite");
    assert_eq!(
        sigops, 80,
        "coinbase scriptSig OP_CHECKSIG bytes must be counted against \
         MAX_BLOCK_SIGOPS (zcashd parity, GHSA-jv4h-j224-23cc)"
    );

    Ok(())
}

/// Regression test for [GHSA-jv4h-j224-23cc] (Trigger B, "Aggregate P2SH Sigops").
///
/// zcashd's `GetP2SHSigOpCount()` parses the redeem script (the last push in each P2SH input's
/// `scriptSig`) with `accurate=true` and sums the sigops across all inputs. Previously Zebra only
/// did this in the mempool policy, so a block containing P2SH inputs whose aggregate redeem-script
/// sigops exceeded `MAX_BLOCK_SIGOPS` could be accepted by Zebra but rejected by zcashd as
/// `bad-blk-sigops`.
///
/// This test exercises the free function `zebra_script::p2sh_sigop_count` directly on a synthetic
/// transaction with one P2SH input whose redeem script is 15 x OP_CHECKSIG.
///
/// [GHSA-jv4h-j224-23cc]: https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jv4h-j224-23cc
#[test]
fn p2sh_sigop_count_counts_redeem_script() -> Result<()> {
    let _init_guard = zebra_test::init();

    const OP_CHECKSIG: u8 = 0xac;
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUAL: u8 = 0x87;

    // Redeem script: 15 x OP_CHECKSIG. Each OP_CHECKSIG counts as 1.
    let redeem_script = vec![OP_CHECKSIG; 15];

    // scriptSig consists solely of a direct push of the redeem script. For a 15-byte payload we can
    // use the literal-length push opcode (0x01..=0x4b), which is just the length byte followed by
    // the data.
    let mut unlock_bytes = Vec::new();
    unlock_bytes.push(redeem_script.len() as u8);
    unlock_bytes.extend_from_slice(&redeem_script);
    let unlock_script = transparent::Script::new(&unlock_bytes);

    // scriptPubKey: OP_HASH160 <20 bytes> OP_EQUAL. The 20-byte payload value is irrelevant here:
    // `is_pay_to_script_hash()` only checks the length (23) and the surrounding opcodes.
    let mut lock_bytes = Vec::with_capacity(23);
    lock_bytes.push(OP_HASH160);
    lock_bytes.push(0x14);
    lock_bytes.extend_from_slice(&[0u8; 20]);
    lock_bytes.push(OP_EQUAL);
    let lock_script = transparent::Script::new(&lock_bytes);

    // Build a minimal non-coinbase transaction with a single P2SH input.
    let input = transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: zebra_chain::transaction::Hash([0u8; 32]),
            index: 0,
        },
        unlock_script,
        sequence: u32::MAX,
    };

    let spent_output = transparent::Output {
        value: zebra_chain::amount::Amount::try_from(1_000_000)?,
        lock_script,
    };

    let tx = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        inputs: vec![input],
        outputs: vec![spent_output.clone()],
        lock_time: LockTime::unlocked(),
        expiry_height: Height(0),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    assert_eq!(
        p2sh_sigop_count(&tx, std::slice::from_ref(&spent_output)),
        15,
        "P2SH redeem-script sigops must be counted against block-path \
         MAX_BLOCK_SIGOPS (zcashd parity, GHSA-jv4h-j224-23cc)"
    );

    // The `CachedFfiTransaction::p2sh_sigops()` method must delegate to the same counter, so the
    // block-verifier path yields the same count.
    let cached = super::CachedFfiTransaction::new(
        Arc::new(tx),
        Arc::new(vec![spent_output]),
        NetworkUpgrade::Nu5,
    )
    .expect("network upgrade should be valid for tx");
    assert_eq!(cached.p2sh_sigops(), 15);

    Ok(())
}

/// Non-P2SH inputs, and coinbase inputs, must contribute zero P2SH sigops regardless of what bytes
/// appear in their `scriptSig`. zcashd skips the coinbase input in [`GetP2SHSigOpCount()`].
///
/// [`GetP2SHSigOpCount()`]: https://github.com/zcash/zcash/blob/bad7f7eadbbb3466bebe3354266c7f69f607fcfd/src/main.cpp#L770-L772
#[test]
fn p2sh_sigop_count_is_zero_for_non_p2sh_and_coinbase() -> Result<()> {
    let _init_guard = zebra_test::init();

    const OP_CHECKSIG: u8 = 0xac;

    // Build a coinbase tx with OP_CHECKSIG-filled `miner_data`: legacy counting sees these (Trigger
    // A), but P2SH counting must not.
    let network = Network::Mainnet;
    let nu5_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 has a Mainnet activation height");
    let dummy_output_script = transparent::Script::new(&[0x51]);
    let output_amount = zebra_chain::amount::Amount::try_from(1_000_000)?;
    let coinbase_tx = Transaction::new_v5_coinbase(
        &network,
        nu5_height,
        vec![(output_amount, dummy_output_script.clone())],
        vec![OP_CHECKSIG; 80],
    );

    // Coinbase inputs have no spent output; zcashd passes an empty vector.
    assert_eq!(p2sh_sigop_count(&coinbase_tx, &[]), 0);

    // A non-P2SH (p2pkh-shaped) lock script must also yield 0 P2SH sigops, even if the scriptSig's
    // last push happens to contain OP_CHECKSIG.
    let p2pkh_lock = transparent::Script::new(&hex::decode(
        "76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac",
    )?);
    let mut unlock_bytes = vec![0x01_u8]; // single-byte push
    unlock_bytes.push(OP_CHECKSIG);
    let input = transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: zebra_chain::transaction::Hash([0u8; 32]),
            index: 0,
        },
        unlock_script: transparent::Script::new(&unlock_bytes),
        sequence: u32::MAX,
    };
    let spent_output = transparent::Output {
        value: zebra_chain::amount::Amount::try_from(1_000_000)?,
        lock_script: p2pkh_lock,
    };
    let tx = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        inputs: vec![input],
        outputs: vec![],
        lock_time: LockTime::unlocked(),
        expiry_height: Height(0),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };
    assert_eq!(p2sh_sigop_count(&tx, &[spent_output]), 0);

    Ok(())
}

/// End-to-end regression test for [GHSA-jv4h-j224-23cc].
///
/// Reproduces the consensus-split condition: a block whose true `zcashd` transparent sigop total
/// (legacy + P2SH, including coinbase legacy) exceeds `MAX_BLOCK_SIGOPS = 20000`. Before the fix,
/// Zebra computed a reduced total that fit under the limit and accepted such a block.
///
/// This test exercises the same accumulation the block verifier performs in
/// `zebra_consensus::block::verify_block` (`block.rs` `sigops += response.sigops()`), where each
/// transaction's contribution is `tx.sigops() + cached.p2sh_sigops()` (set in
/// `zebra_consensus::transaction.rs`'s `Block`-path response).
///
/// Asserts:
/// - the legacy-only total (Zebra's pre-fix accounting) is below the limit;
/// - the legacy + P2SH total (Zebra's post-fix, zcashd-equivalent accounting) exceeds the limit,
///   demonstrating the consensus split is now visible to the block verifier.
///
/// [GHSA-jv4h-j224-23cc]: https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-jv4h-j224-23cc
#[test]
fn block_sigop_total_includes_coinbase_and_p2sh() -> Result<()> {
    let _init_guard = zebra_test::init();

    /// Matches `zebra_consensus::MAX_BLOCK_SIGOPS`. Hard-coded to avoid a reverse dependency on
    /// `zebra-consensus`.
    const MAX_BLOCK_SIGOPS: u32 = 20_000;
    const OP_CHECKSIG: u8 = 0xac;
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUAL: u8 = 0x87;

    // Surface A: 80 OP_CHECKSIG bytes hidden in the coinbase scriptSig
    // (the coinbase script length limit is 100 bytes, including the height prefix).
    let network = Network::Mainnet;
    let nu5_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .expect("NU5 has a Mainnet activation height");
    let dummy_output_script = transparent::Script::new(&[0x51]); // OP_TRUE
    let output_amount = zebra_chain::amount::Amount::try_from(1_000_000)?;
    let coinbase_tx = Transaction::new_v5_coinbase(
        &network,
        nu5_height,
        vec![(output_amount, dummy_output_script)],
        vec![OP_CHECKSIG; 80],
    );

    // Surface B: each non-coinbase transaction has one P2SH input whose
    // 15-byte redeem script is 15 x OP_CHECKSIG, contributing 15 P2SH
    // sigops (the maximum standard P2SH redeem-script sigop count).
    let redeem_script = vec![OP_CHECKSIG; 15];
    let mut unlock_bytes = vec![redeem_script.len() as u8];
    unlock_bytes.extend_from_slice(&redeem_script);
    let unlock_script = transparent::Script::new(&unlock_bytes);

    let mut lock_bytes = Vec::with_capacity(23);
    lock_bytes.push(OP_HASH160);
    lock_bytes.push(0x14);
    lock_bytes.extend_from_slice(&[0u8; 20]);
    lock_bytes.push(OP_EQUAL);
    let lock_script = transparent::Script::new(&lock_bytes);

    let p2sh_input = transparent::Input::PrevOut {
        outpoint: transparent::OutPoint {
            hash: zebra_chain::transaction::Hash([0u8; 32]),
            index: 0,
        },
        unlock_script,
        sequence: u32::MAX,
    };
    let p2sh_spent_output = transparent::Output {
        value: zebra_chain::amount::Amount::try_from(1_000_000)?,
        lock_script,
    };
    let p2sh_tx_template = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        inputs: vec![p2sh_input],
        outputs: vec![],
        lock_time: LockTime::unlocked(),
        expiry_height: Height(0),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    // 1334 P2SH spends * 15 sigops = 20010 P2SH sigops.
    // Plus 80 coinbase legacy sigops = 20090 total, > MAX_BLOCK_SIGOPS.
    // The legacy-only total Zebra used to compute is 0 (pre-fix coinbase)
    // or 80 (post-fix coinbase), both well under the limit.
    const N_P2SH_TXS: u32 = 1334;
    let spent_outputs = std::slice::from_ref(&p2sh_spent_output);

    let coinbase_legacy = coinbase_tx.sigops().expect("sigop count is finite");
    let p2sh_legacy = p2sh_tx_template.sigops().expect("sigop count is finite");
    let p2sh_per_tx = p2sh_sigop_count(&p2sh_tx_template, spent_outputs);
    assert_eq!(coinbase_legacy, 80, "coinbase legacy sigops (Surface A)");
    assert_eq!(p2sh_per_tx, 15, "per-tx P2SH sigops (Surface B)");

    // Post-fix accounting matches what the block verifier accumulates in
    // `zebra_consensus::block::verify_block` via `response.sigops()`, where the response is built
    // in `zebra_consensus::transaction.rs`'s `Block`-path arm as `tx.sigops() +
    // cached.p2sh_sigops()`.
    //
    // Non-coinbase txs contribute legacy sigops from their inputs and outputs; here `p2sh_legacy`
    // is 0 (the redeem script bytes inside the scriptSig are NOT executed at the legacy level for a
    // literal push-only scriptSig).
    let post_fix_total = coinbase_legacy
        .saturating_add(N_P2SH_TXS.saturating_mul(p2sh_legacy.saturating_add(p2sh_per_tx)));

    assert!(
        post_fix_total > MAX_BLOCK_SIGOPS,
        "post-fix accounting must exceed MAX_BLOCK_SIGOPS to demonstrate \
         the consensus split is now visible: got {post_fix_total}"
    );

    Ok(())
}

/// Calling `is_valid` when `all_previous_outputs.len()` differs from `transaction.inputs().len()`
/// must return `Error::TxIndex`, even when the requested `input_index` is in range for
/// `all_previous_outputs`. This guards against verifying a script against a misaligned
/// previous output.
#[test]
fn is_valid_rejects_mismatched_previous_outputs_length() {
    let _init_guard = zebra_test::init();

    let transaction = SCRIPT_TX
        .zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()
        .expect("test fixture deserializes");

    // SCRIPT_TX has exactly one input. Pass two previous outputs so `.get(0)` succeeds
    // but the lengths disagree.
    let output = Output {
        value: (212 * u64::pow(10, 8)).try_into().expect("valid amount"),
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY),
    };
    let mismatched_outputs = Arc::new(vec![output.clone(), output]);

    let verifier =
        super::CachedFfiTransaction::new(transaction, mismatched_outputs, NetworkUpgrade::Blossom)
            .expect("constructor accepts mismatched-length outputs");

    let err = verifier
        .is_valid(0)
        .expect_err("mismatched length must be rejected by is_valid");
    assert_eq!(err, super::Error::TxIndex);
}

/// Calling `is_valid` with an `input_index` past the end of `all_previous_outputs` must
/// return `Error::TxIndex` instead of panicking.
#[test]
fn is_valid_rejects_out_of_range_input_index() {
    let _init_guard = zebra_test::init();

    let transaction = SCRIPT_TX
        .zcash_deserialize_into::<Arc<zebra_chain::transaction::Transaction>>()
        .expect("test fixture deserializes");
    let output = Output {
        value: (212 * u64::pow(10, 8)).try_into().expect("valid amount"),
        lock_script: transparent::Script::new(&SCRIPT_PUBKEY),
    };
    let previous_outputs = Arc::new(vec![output]);

    let verifier =
        super::CachedFfiTransaction::new(transaction, previous_outputs, NetworkUpgrade::Blossom)
            .expect("matched-length construction succeeds");

    // SCRIPT_TX has one input at index 0; index 1 is out of range.
    let err = verifier
        .is_valid(1)
        .expect_err("out-of-range input_index must error, not panic");
    assert_eq!(err, super::Error::TxIndex);
}

/// Regression test for the libzcash_script stale-sighash-buffer bypass.
///
/// Construct a V5 transaction whose scriptPubKey is:
///
/// ```text
/// <pubkey> OP_CHECKSIGVERIFY <pubkey> OP_CHECKSIG
/// ```
///
/// and whose scriptSig pushes two signatures over the same canonical
/// `SIGHASH_ALL` (0x01) digest, the first tagged with an *invalid* V5
/// hash-type byte (0x50) and the second tagged with the canonical 0x01:
///
/// ```text
/// <der_sig || 0x50>   (pushed first → bottom of stack)
/// <der_sig || 0x01>   (pushed second → top of stack)
/// ```
///
/// Script evaluation then:
///
/// 1. Consumes `<der_sig || 0x01>` via `OP_CHECKSIGVERIFY`. Zebra's callback
///    returns the canonical SIGHASH_ALL digest, the C++ verifier fills its
///    stack-local `sighashArray` with that digest, and the signature passes.
/// 2. Consumes `<der_sig || 0x50>` via `OP_CHECKSIG`. Zebra's callback sees
///    an invalid V5 hash-type byte. Prior to this fix the callback returned
///    `None`, libzcash_script silently wrote nothing to the C++ buffer, and
///    the C++ `CheckSig` verified the signature against the stale
///    SIGHASH_ALL digest from step 1 — accepting a spend that `zcashd`
///    rejects and splitting Zebra nodes from `zcashd` nodes.
///
/// With the defense-in-depth fix in `zebra-script::calculate_sighash`, the
/// callback now returns a per-call CSPRNG-derived sighash when the hash
/// type would have been rejected, so the second signature fails to verify
/// and `is_valid` returns an error — matching `zcashd`.
///
/// The bypass requires release-grade C++ optimizations in `libzcash_script`
/// (so the stack buffer is not zero-initialized and the prior digest
/// lingers between callbacks). The workspace `Cargo.toml` forces
/// `[profile.dev.package.libzcash_script]` to `opt-level = 3` in all
/// profiles so that `cargo test` exercises the vulnerable code path and
/// this regression test catches any re-introduction of the bug in both
/// dev and release builds.
#[test]
fn stale_sighash_buffer_v5_two_checksig_rejected() {
    use secp256k1::{Message, Secp256k1, SecretKey};

    let _init_guard = zebra_test::init();

    let secp = Secp256k1::new();
    let secret_key = SecretKey::from_slice(&[0xcd; 32]).expect("valid secret key");
    let public_key = secp256k1::PublicKey::from_secret_key(&secp, &secret_key);
    let pubkey_bytes = public_key.serialize();

    // scriptPubKey: <0x21> <pubkey 33 bytes> OP_CHECKSIGVERIFY
    //               <0x21> <pubkey 33 bytes> OP_CHECKSIG
    // OP_CHECKSIGVERIFY = 0xad, OP_CHECKSIG = 0xac
    let mut lock_script_bytes = Vec::with_capacity(1 + 33 + 1 + 1 + 33 + 1);
    lock_script_bytes.push(0x21);
    lock_script_bytes.extend_from_slice(&pubkey_bytes);
    lock_script_bytes.push(0xad);
    lock_script_bytes.push(0x21);
    lock_script_bytes.extend_from_slice(&pubkey_bytes);
    lock_script_bytes.push(0xac);
    let lock_script = transparent::Script::new(&lock_script_bytes);

    let previous_output = transparent::Output {
        value: 1_0000_0000u64.try_into().expect("valid amount"),
        lock_script: lock_script.clone(),
    };

    // Placeholder V5 tx used to compute the sighash; the V5 (ZIP 244) sighash
    // does not depend on the unlock script contents, so we can sign, then
    // rebuild the transaction with the real unlock script.
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
            lock_script: transparent::Script::new(&[0x00]),
        }],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    let all_previous_outputs = Arc::new(vec![previous_output.clone()]);
    let sighasher = SigHasher::new(&placeholder_tx, NetworkUpgrade::Nu5, all_previous_outputs)
        .expect("sighasher creation should succeed");

    // Canonical SIGHASH_ALL digest — this is the digest the attacker needs
    // the stale C++ buffer to still hold when the second CHECKSIG runs.
    let sighash = sighasher.sighash(HashType::ALL, Some((0, lock_script_bytes.clone())));
    let msg = Message::from_digest(*sighash.as_ref());
    let signature = secp.sign_ecdsa(&msg, &secret_key);
    let der_sig = signature.serialize_der();

    // scriptSig pushes:
    //   1. <der_sig || 0x50>  (bottom)
    //   2. <der_sig || 0x01>  (top, consumed by OP_CHECKSIGVERIFY first)
    let mut unlock_script_bytes = Vec::new();
    let sig_with_hashtype_len = (der_sig.len() + 1) as u8;

    unlock_script_bytes.push(sig_with_hashtype_len);
    unlock_script_bytes.extend_from_slice(&der_sig);
    unlock_script_bytes.push(0x50);

    unlock_script_bytes.push(sig_with_hashtype_len);
    unlock_script_bytes.extend_from_slice(&der_sig);
    unlock_script_bytes.push(0x01);

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

    assert!(
        verifier.is_valid(0).is_err(),
        "V5 tx exploiting the stale libzcash_script sighash buffer via \
         OP_CHECKSIGVERIFY + OP_CHECKSIG with an invalid second hash-type \
         byte (0x50) must be rejected, matching zcashd"
    );
}
