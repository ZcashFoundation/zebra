//! Fixed test vectors for transactions.

use arbitrary::v5_transactions;
use chrono::DateTime;
use color_eyre::eyre::Result;
use lazy_static::lazy_static;
use rand::{seq::IteratorRandom, thread_rng};

use crate::{
    block::{Block, Height, MAX_BLOCK_BYTES},
    parameters::Network,
    primitives::zcash_primitives::PrecomputedTxData,
    serialization::{SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::{sighash::SigHasher, txid::TxIdBuilder},
    transparent::Script,
};

use zebra_test::{
    vectors::{ZIP143_1, ZIP143_2, ZIP243_1, ZIP243_2, ZIP243_3},
    zip0143, zip0243, zip0244,
};

use super::super::*;

lazy_static! {
    pub static ref EMPTY_V5_TX: Transaction = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: block::Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };
}

/// Build a mock output list for pre-V5 transactions, with (index+1)
/// copies of `output`, which is used to computed the sighash.
///
/// Pre-V5, the entire output list is not used; only the output for the
/// given index is read. Therefore, we just need a list where `array[index]`
/// is the given `output`.
fn mock_pre_v5_output_list(output: transparent::Output, index: usize) -> Vec<transparent::Output> {
    std::iter::repeat_n(output, index + 1).collect()
}

#[test]
fn transactionhash_struct_from_str_roundtrip() {
    let _init_guard = zebra_test::init();

    let hash: Hash = "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf"
        .parse()
        .unwrap();

    assert_eq!(
        format!("{hash:?}"),
        r#"transaction::Hash("3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf")"#
    );
    assert_eq!(
        hash.to_string(),
        "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf"
    );
}

#[test]
fn auth_digest_struct_from_str_roundtrip() {
    let _init_guard = zebra_test::init();

    let digest: AuthDigest = "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf"
        .parse()
        .unwrap();

    assert_eq!(
        format!("{digest:?}"),
        r#"AuthDigest("3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf")"#
    );
    assert_eq!(
        digest.to_string(),
        "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf"
    );
}

#[test]
fn wtx_id_struct_from_str_roundtrip() {
    let _init_guard = zebra_test::init();

    let wtx_id: WtxId = "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf0000000000000000000000000000000000000000000000000000000000000001"
        .parse()
        .unwrap();

    assert_eq!(
        format!("{wtx_id:?}"),
        r#"WtxId { id: transaction::Hash("3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf"), auth_digest: AuthDigest("0000000000000000000000000000000000000000000000000000000000000001") }"#
    );
    assert_eq!(
        wtx_id.to_string(),
        "3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf0000000000000000000000000000000000000000000000000000000000000001"
    );
}

#[test]
fn librustzcash_tx_deserialize_and_round_trip() {
    let _init_guard = zebra_test::init();

    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

#[test]
fn librustzcash_tx_hash() {
    let _init_guard = zebra_test::init();

    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    // TxID taken from comment in zebra_test::vectors
    let hash = tx.hash();
    let expected = "64f0bd7fe30ce23753358fe3a2dc835b8fba9c0274c4e2c54a6f73114cb55639"
        .parse::<Hash>()
        .expect("hash should parse correctly");

    assert_eq!(hash, expected);
}

#[test]
fn doesnt_deserialize_transaction_with_invalid_value_balance() {
    let _init_guard = zebra_test::init();

    let dummy_transaction = Transaction::V4 {
        inputs: vec![],
        outputs: vec![],
        lock_time: LockTime::Height(Height(1)),
        expiry_height: Height(10),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let mut input_bytes = Vec::new();
    dummy_transaction
        .zcash_serialize(&mut input_bytes)
        .expect("dummy transaction should serialize");
    // Set value balance to non-zero
    // There are 4 * 4 byte fields and 2 * 1 byte compact sizes = 18 bytes before the 8 byte amount
    // (Zcash is little-endian unless otherwise specified:
    // https://zips.z.cash/protocol/nu5.pdf#endian)
    input_bytes[18] = 1;

    let result = Transaction::zcash_deserialize(&input_bytes[..]);

    assert!(matches!(
        result,
        Err(SerializationError::BadTransactionBalance)
    ));
}

#[test]
fn zip143_deserialize_and_round_trip() {
    let _init_guard = zebra_test::init();

    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_1[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP143_2[..])
        .expect("transaction test vector from ZIP143 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP143_2[..], &data2[..]);
}

#[test]
fn zip243_deserialize_and_round_trip() {
    let _init_guard = zebra_test::init();

    let tx1 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_1[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data1 = Vec::new();
    tx1.zcash_serialize(&mut data1)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_1[..], &data1[..]);

    let tx2 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_2[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data2 = Vec::new();
    tx2.zcash_serialize(&mut data2)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_2[..], &data2[..]);

    let tx3 = Transaction::zcash_deserialize(&zebra_test::vectors::ZIP243_3[..])
        .expect("transaction test vector from ZIP243 should deserialize");

    let mut data3 = Vec::new();
    tx3.zcash_serialize(&mut data3)
        .expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::ZIP243_3[..], &data3[..]);
}

#[test]
fn deserialize_large_transaction() {
    let _init_guard = zebra_test::init();

    // Create a dummy input and output.
    let input =
        transparent::Input::zcash_deserialize(&zebra_test::vectors::DUMMY_INPUT1[..]).unwrap();
    let output =
        transparent::Output::zcash_deserialize(&zebra_test::vectors::DUMMY_OUTPUT1[..]).unwrap();

    // Serialize the input so that we can determine its serialized size.
    let mut input_data = Vec::new();
    input
        .zcash_serialize(&mut input_data)
        .expect("input should serialize");

    // Calculate the number of inputs that fit into the transaction size limit.
    let tx_inputs_num = MAX_BLOCK_BYTES as usize / input_data.len();

    // Set the precalculated amount of inputs and a single output.
    let inputs = std::iter::repeat_n(input, tx_inputs_num).collect::<Vec<_>>();

    // Create an oversized transaction. Adding the output and lock time causes
    // the transaction to overflow the threshold.
    let oversized_tx = Transaction::V1 {
        inputs,
        outputs: vec![output],
        lock_time: LockTime::Time(DateTime::from_timestamp(61, 0).unwrap()),
    };

    // Serialize the transaction.
    let mut tx_data = Vec::new();
    oversized_tx
        .zcash_serialize(&mut tx_data)
        .expect("transaction should serialize");

    // Check that the transaction is oversized.
    assert!(tx_data.len() > MAX_BLOCK_BYTES as usize);

    // The deserialization should fail because the transaction is too big.
    Transaction::zcash_deserialize(&tx_data[..])
        .expect_err("transaction should not deserialize due to its size");
}

// Transaction V5 test vectors

/// An empty transaction v5, with no Orchard, Sapling, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v5_round_trip() {
    let _init_guard = zebra_test::init();

    let tx: &Transaction = &EMPTY_V5_TX;

    let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
    let tx2: &Transaction = &data
        .zcash_deserialize_into()
        .expect("tx should deserialize");

    assert_eq!(tx, tx2);

    let data2 = tx2
        .zcash_serialize_to_vec()
        .expect("vec serialization is infallible");

    assert_eq!(data, data2, "data must be equal if structs are equal");
}

/// An empty transaction v4, with no Sapling, Sprout, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v4_round_trip() {
    let _init_guard = zebra_test::init();

    let tx = Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: block::Height(0),
        joinsplit_data: None,
        sapling_shielded_data: None,
    };

    let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
    let tx2 = data
        .zcash_deserialize_into()
        .expect("tx should deserialize");

    assert_eq!(tx, tx2);

    let data2 = tx2
        .zcash_serialize_to_vec()
        .expect("vec serialization is infallible");

    assert_eq!(data, data2, "data must be equal if structs are equal");
}

/// Check if an empty V5 transaction can be deserialized by librustzcash too.
#[test]
fn empty_v5_librustzcash_round_trip() {
    let _init_guard = zebra_test::init();

    let tx: &Transaction = &EMPTY_V5_TX;
    let nu = tx.network_upgrade().expect("network upgrade");

    tx.to_librustzcash(nu).expect(
        "librustzcash deserialization might work for empty zebra serialized transactions. \
        Hint: if empty transactions fail, but other transactions work, delete this test",
    );
}

#[test]
fn invalid_orchard_nullifier() {
    let _init_guard = zebra_test::init();

    use std::convert::TryFrom;

    // generated by proptest using something as:
    // ```rust
    // ...
    // array::uniform32(any::<u8>()).prop_map(|x| Self::try_from(x).unwrap()).boxed()
    // ...
    // ```
    let invalid_nullifier_bytes = [
        62, 157, 27, 63, 100, 228, 1, 82, 140, 16, 238, 78, 68, 19, 221, 184, 189, 207, 230, 95,
        194, 216, 165, 24, 110, 221, 139, 195, 106, 98, 192, 71,
    ];

    assert_eq!(
        orchard::Nullifier::try_from(invalid_nullifier_bytes)
            .err()
            .unwrap()
            .to_string(),
        SerializationError::Parse("Invalid pallas::Base value for orchard Nullifier").to_string()
    );
}

/// Do a round-trip test via librustzcash on fake v5 transactions created from v4 transactions
/// in the block test vectors.
/// Makes sure that zebra-serialized transactions can be deserialized by librustzcash.
#[test]
fn fake_v5_librustzcash_round_trip() {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        fake_v5_librustzcash_round_trip_for_network(network);
    }
}

fn fake_v5_librustzcash_round_trip_for_network(network: Network) {
    let block_iter = network.block_iter();

    let overwinter_activation_height = NetworkUpgrade::Overwinter
        .activation_height(&network)
        .expect("a valid height")
        .0;

    let nu5_activation_height = NetworkUpgrade::Nu5
        .activation_height(&network)
        .unwrap_or(Height::MAX_EXPIRY_HEIGHT)
        .0;

    // skip blocks that are before overwinter as they will not have a valid consensus branch id
    // skip blocks equal or greater Nu5 activation as they are already v5 transactions
    let blocks_after_overwinter_and_before_nu5 = block_iter
        .skip_while(|(height, _)| **height < overwinter_activation_height)
        .take_while(|(height, _)| **height < nu5_activation_height);

    for (height, original_bytes) in blocks_after_overwinter_and_before_nu5 {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let mut fake_block = original_block.clone();
        fake_block.transactions = fake_block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(|t| arbitrary::transaction_to_fake_v5(t, &network, Height(*height)))
            .map(Into::into)
            .collect();

        // test each transaction
        for (original_tx, fake_tx) in original_block
            .transactions
            .iter()
            .zip(fake_block.transactions.iter())
        {
            assert_ne!(
                &original_tx, &fake_tx,
                "v1-v4 transactions must change when converted to fake v5"
            );

            let fake_bytes = fake_tx
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            assert_ne!(
                &original_bytes[..],
                fake_bytes,
                "v1-v4 transaction data must change when converted to fake v5"
            );

            let nu = fake_tx.network_upgrade().expect("network upgrade");

            fake_tx
                .to_librustzcash(nu)
                .expect("librustzcash deserialization must work for zebra serialized transactions");
        }
    }
}

#[test]
fn zip244_round_trip() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let tx = test.tx.zcash_deserialize_into::<Transaction>()?;
        let reencoded = tx.zcash_serialize_to_vec()?;

        assert_eq!(test.tx, reencoded);

        let nu = tx.network_upgrade().expect("network upgrade");

        tx.to_librustzcash(nu)
            .expect("librustzcash deserialization must work for zebra serialized transactions");
    }

    Ok(())
}

#[test]
fn zip244_txid() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let txid = TxIdBuilder::new(&test.tx.zcash_deserialize_into::<Transaction>()?)
            .txid()
            .expect("txid");

        assert_eq!(txid.0, test.txid);
    }

    Ok(())
}

#[test]
fn zip244_auth_digest() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let auth_digest = transaction.auth_digest();
        assert_eq!(
            auth_digest
                .expect("must have auth_digest since it must be a V5 transaction")
                .0,
            test.auth_digest
        );
    }

    Ok(())
}

#[test]
fn test_vec143_1() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction = ZIP143_1.zcash_deserialize_into::<Transaction>()?;

    let hasher = SigHasher::new(
        &transaction,
        NetworkUpgrade::Overwinter,
        Arc::new(Vec::new()),
    )
    .expect("network upgrade is valid for tx");

    let hash = hasher.sighash(HashType::ALL, None);
    let expected = "a1f1a4e5cd9bd522322d661edd2af1bf2a7019cfab94ece18f4ba935b0a19073";
    let result = hex::encode(hash);
    let span = tracing::span!(
        tracing::Level::ERROR,
        "compare_final",
        expected.len = expected.len(),
        buf.len = result.len()
    );
    let _guard = span.enter();
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec143_2() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction = ZIP143_2.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("2f6e04963b4c0100")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&hex::decode("53")?);
    let input_ind = 1;
    let output = transparent::Output {
        value,
        lock_script: lock_script.clone(),
    };
    let all_previous_outputs = mock_pre_v5_output_list(output, input_ind);

    let hasher = SigHasher::new(
        &transaction,
        NetworkUpgrade::Overwinter,
        Arc::new(all_previous_outputs),
    )
    .expect("network upgrade is valid for tx");

    let hash = hasher.sighash(
        HashType::SINGLE,
        Some((input_ind, lock_script.as_raw_bytes().to_vec())),
    );
    let expected = "23652e76cb13b85a0e3363bb5fca061fa791c40c533eccee899364e6e60bb4f7";
    let result: &[u8] = hash.as_ref();
    let result = hex::encode(result);
    let span = tracing::span!(
        tracing::Level::ERROR,
        "compare_final",
        expected.len = expected.len(),
        buf.len = result.len()
    );
    let _guard = span.enter();
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec243_1() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction = ZIP243_1.zcash_deserialize_into::<Transaction>()?;

    let hasher = SigHasher::new(&transaction, NetworkUpgrade::Sapling, Arc::new(Vec::new()))
        .expect("network upgrade is valid for tx");

    let hash = hasher.sighash(HashType::ALL, None);
    let expected = "63d18534de5f2d1c9e169b73f9c783718adbef5c8a7d55b5e7a37affa1dd3ff3";
    let result = hex::encode(hash);
    let span = tracing::span!(
        tracing::Level::ERROR,
        "compare_final",
        expected.len = expected.len(),
        buf.len = result.len()
    );
    let _guard = span.enter();
    assert_eq!(expected, result);

    let precomputed_tx_data =
        PrecomputedTxData::new(&transaction, NetworkUpgrade::Sapling, Arc::new(Vec::new()))
            .expect("network upgrade is valid for tx");
    let alt_sighash =
        crate::primitives::zcash_primitives::sighash(&precomputed_tx_data, HashType::ALL, None);
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec243_2() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction = ZIP243_2.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("adedf02996510200")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&[]);
    let input_ind = 1;
    let output = transparent::Output {
        value,
        lock_script: lock_script.clone(),
    };
    let all_previous_outputs = mock_pre_v5_output_list(output, input_ind);

    let hasher = SigHasher::new(
        &transaction,
        NetworkUpgrade::Sapling,
        Arc::new(all_previous_outputs),
    )
    .expect("network upgrade is valid for tx");

    let hash = hasher.sighash(
        HashType::NONE,
        Some((input_ind, lock_script.as_raw_bytes().to_vec())),
    );
    let expected = "bbe6d84f57c56b29b914c694baaccb891297e961de3eb46c68e3c89c47b1a1db";
    let result = hex::encode(hash);
    let span = tracing::span!(
        tracing::Level::ERROR,
        "compare_final",
        expected.len = expected.len(),
        buf.len = result.len()
    );
    let _guard = span.enter();
    assert_eq!(expected, result);

    let lock_script = Script::new(&[]);
    let prevout = transparent::Output {
        value,
        lock_script: lock_script.clone(),
    };
    let index = input_ind;
    let all_previous_outputs = mock_pre_v5_output_list(prevout, input_ind);

    let precomputed_tx_data = PrecomputedTxData::new(
        &transaction,
        NetworkUpgrade::Sapling,
        Arc::new(all_previous_outputs),
    )
    .expect("network upgrade is valid for tx");
    let alt_sighash = crate::primitives::zcash_primitives::sighash(
        &precomputed_tx_data,
        HashType::NONE,
        Some((index, lock_script.as_raw_bytes().to_vec())),
    );
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec243_3() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transaction = ZIP243_3.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("80f0fa0200000000")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&hex::decode(
        "76a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
    )?);
    let input_ind = 0;
    let all_previous_outputs = vec![transparent::Output {
        value,
        lock_script: lock_script.clone(),
    }];

    let hasher = SigHasher::new(
        &transaction,
        NetworkUpgrade::Sapling,
        Arc::new(all_previous_outputs),
    )
    .expect("network upgrade is valid for tx");

    let hash = hasher.sighash(
        HashType::ALL,
        Some((input_ind, lock_script.as_raw_bytes().to_vec())),
    );
    let expected = "f3148f80dfab5e573d5edfe7a850f5fd39234f80b5429d3a57edcc11e34c585b";
    let result = hex::encode(hash);
    let span = tracing::span!(
        tracing::Level::ERROR,
        "compare_final",
        expected.len = expected.len(),
        buf.len = result.len()
    );
    let _guard = span.enter();
    assert_eq!(expected, result);

    let lock_script = Script::new(&hex::decode(
        "76a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
    )?);
    let prevout = transparent::Output {
        value,
        lock_script: lock_script.clone(),
    };
    let index = input_ind;

    let all_previous_outputs = vec![prevout];
    let precomputed_tx_data = PrecomputedTxData::new(
        &transaction,
        NetworkUpgrade::Sapling,
        Arc::new(all_previous_outputs),
    )
    .expect("network upgrade is valid for tx");
    let alt_sighash = crate::primitives::zcash_primitives::sighash(
        &precomputed_tx_data,
        HashType::ALL,
        Some((index, lock_script.as_raw_bytes().to_vec())),
    );
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn zip143_sighash() -> Result<()> {
    let _init_guard = zebra_test::init();

    for (i, test) in zip0143::TEST_VECTORS.iter().enumerate() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let (input_index, output) = match test.transparent_input {
            Some(transparent_input) => (
                Some(transparent_input as usize),
                Some(transparent::Output {
                    value: test.amount.try_into()?,
                    lock_script: transparent::Script::new(test.script_code.as_ref()),
                }),
            ),
            None => (None, None),
        };
        let all_previous_outputs: Vec<_> = match output.clone() {
            Some(output) => mock_pre_v5_output_list(output, input_index.unwrap()),
            None => vec![],
        };
        let result = hex::encode(
            transaction
                .sighash(
                    NetworkUpgrade::try_from(test.consensus_branch_id).expect("network upgrade"),
                    HashType::from_bits(test.hash_type).expect("must be a valid HashType"),
                    Arc::new(all_previous_outputs),
                    input_index.map(|input_index| {
                        (
                            input_index,
                            output.unwrap().lock_script.as_raw_bytes().to_vec(),
                        )
                    }),
                )
                .expect("network upgrade is valid for tx"),
        );
        let expected = hex::encode(test.sighash);
        assert_eq!(expected, result, "test #{i}: sighash does not match");
    }

    Ok(())
}

#[test]
fn zip243_sighash() -> Result<()> {
    let _init_guard = zebra_test::init();

    for (i, test) in zip0243::TEST_VECTORS.iter().enumerate() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let (input_index, output) = match test.transparent_input {
            Some(transparent_input) => (
                Some(transparent_input as usize),
                Some(transparent::Output {
                    value: test.amount.try_into()?,
                    lock_script: transparent::Script::new(test.script_code.as_ref()),
                }),
            ),
            None => (None, None),
        };
        let all_previous_outputs: Vec<_> = match output.clone() {
            Some(output) => mock_pre_v5_output_list(output, input_index.unwrap()),
            None => vec![],
        };
        let result = hex::encode(
            transaction
                .sighash(
                    NetworkUpgrade::try_from(test.consensus_branch_id).expect("network upgrade"),
                    HashType::from_bits(test.hash_type).expect("must be a valid HashType"),
                    Arc::new(all_previous_outputs),
                    input_index.map(|input_index| {
                        (
                            input_index,
                            output.unwrap().lock_script.as_raw_bytes().to_vec(),
                        )
                    }),
                )
                .expect("network upgrade is valid for tx"),
        );
        let expected = hex::encode(test.sighash);
        assert_eq!(expected, result, "test #{i}: sighash does not match");
    }

    Ok(())
}

#[test]
fn zip244_sighash() -> Result<()> {
    let _init_guard = zebra_test::init();

    for (i, test) in zip0244::TEST_VECTORS.iter().enumerate() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;

        let all_previous_outputs: Arc<Vec<_>> = Arc::new(
            test.amounts
                .iter()
                .zip(test.script_pubkeys.iter())
                .map(|(amount, script_pubkey)| transparent::Output {
                    value: (*amount).try_into().unwrap(),
                    lock_script: transparent::Script::new(script_pubkey.as_ref()),
                })
                .collect(),
        );

        let result = hex::encode(
            transaction
                .sighash(
                    NetworkUpgrade::Nu5,
                    HashType::ALL,
                    all_previous_outputs.clone(),
                    None,
                )
                .expect("network upgrade is valid for tx"),
        );
        let expected = hex::encode(test.sighash_shielded);
        assert_eq!(expected, result, "test #{i}: sighash does not match");

        if let Some(sighash_all) = test.sighash_all {
            let result = hex::encode(
                transaction
                    .sighash(
                        NetworkUpgrade::Nu5,
                        HashType::ALL,
                        all_previous_outputs,
                        test.transparent_input
                            .map(|idx| (idx as _, test.script_pubkeys[idx as usize].clone())),
                    )
                    .expect("network upgrade is valid for tx"),
            );
            let expected = hex::encode(sighash_all);
            assert_eq!(expected, result, "test #{i}: sighash does not match");
        }
    }

    Ok(())
}

#[test]
fn consensus_branch_id() {
    for net in Network::iter() {
        for tx in v5_transactions(net.block_iter()).filter(|tx| {
            !tx.has_transparent_inputs() && tx.has_shielded_data() && tx.network_upgrade().is_some()
        }) {
            let tx_nu = tx
                .network_upgrade()
                .expect("this test shouldn't use txs without a network upgrade");

            let any_other_nu = NetworkUpgrade::iter()
                .filter(|&nu| nu != tx_nu)
                .choose(&mut thread_rng())
                .expect("there must be a network upgrade other than the tx one");

            // All computations should succeed under the tx nu.

            tx.to_librustzcash(tx_nu)
                .expect("tx is convertible under tx nu");
            PrecomputedTxData::new(&tx, tx_nu, Arc::new(Vec::new()))
                .expect("network upgrade is valid for tx");
            sighash::SigHasher::new(&tx, tx_nu, Arc::new(Vec::new()))
                .expect("network upgrade is valid for tx");
            tx.sighash(tx_nu, HashType::ALL, Arc::new(Vec::new()), None)
                .expect("network upgrade is valid for tx");

            // All computations should fail under an nu other than the tx one.

            tx.to_librustzcash(any_other_nu)
                .expect_err("tx is not convertible under nu other than the tx one");

            let err = PrecomputedTxData::new(&tx, any_other_nu, Arc::new(Vec::new())).unwrap_err();
            assert!(
                matches!(err, crate::Error::InvalidConsensusBranchId),
                "precomputing tx sighash data errors under nu other than the tx one"
            );

            let err = sighash::SigHasher::new(&tx, any_other_nu, Arc::new(Vec::new())).unwrap_err();
            assert!(
                matches!(err, crate::Error::InvalidConsensusBranchId),
                "creating the sighasher errors under nu other than the tx one"
            );

            let err = tx
                .sighash(any_other_nu, HashType::ALL, Arc::new(Vec::new()), None)
                .unwrap_err();
            assert!(
                matches!(err, crate::Error::InvalidConsensusBranchId),
                "the sighash computation errors under nu other than the tx one"
            );
        }
    }
}

#[test]
fn binding_signatures() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let sapling_activation_height = NetworkUpgrade::Sapling
            .activation_height(&net)
            .expect("a valid height")
            .0;

        let mut at_least_one_v4_checked = false;
        let mut at_least_one_v5_checked = false;

        for (height, block) in net
            .block_iter()
            .skip_while(|(height, _)| **height < sapling_activation_height)
        {
            let nu = NetworkUpgrade::current(&net, Height(*height));

            for tx in block
                .zcash_deserialize_into::<Block>()
                .expect("a valid block")
                .transactions
            {
                match &*tx {
                    Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => (),
                    Transaction::V4 {
                        sapling_shielded_data,
                        ..
                    } => {
                        if let Some(sapling_shielded_data) = sapling_shielded_data {
                            let sighash = tx
                                .sighash(nu, HashType::ALL, Arc::new(Vec::new()), None)
                                .expect("network upgrade is valid for tx");

                            let bvk = redjubjub::VerificationKey::try_from(
                                sapling_shielded_data.binding_verification_key(),
                            )
                            .expect("a valid redjubjub::VerificationKey");

                            bvk.verify(sighash.as_ref(), &sapling_shielded_data.binding_sig)
                                .expect("must pass verification");

                            at_least_one_v4_checked = true;
                        }
                    }
                    Transaction::V5 {
                        sapling_shielded_data,
                        ..
                    } => {
                        if let Some(sapling_shielded_data) = sapling_shielded_data {
                            // V5 txs have the outputs spent by their transparent inputs hashed into
                            // their SIGHASH, so we need to exclude txs with transparent inputs.
                            //
                            // References:
                            //
                            // <https://zips.z.cash/zip-0244#s-2c-amounts-sig-digest>
                            // <https://zips.z.cash/zip-0244#s-2d-scriptpubkeys-sig-digest>
                            if tx.has_transparent_inputs() {
                                continue;
                            }

                            let sighash = tx
                                .sighash(nu, HashType::ALL, Arc::new(Vec::new()), None)
                                .expect("network upgrade is valid for tx");

                            let bvk = redjubjub::VerificationKey::try_from(
                                sapling_shielded_data.binding_verification_key(),
                            )
                            .expect("a valid redjubjub::VerificationKey");

                            bvk.verify(sighash.as_ref(), &sapling_shielded_data.binding_sig)
                                .expect("verification passes");

                            at_least_one_v5_checked = true;
                        }
                    }
                    #[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
                    Transaction::V6 {
                        sapling_shielded_data,
                        ..
                    } => {
                        if let Some(sapling_shielded_data) = sapling_shielded_data {
                            // V6 txs have the outputs spent by their transparent inputs hashed into
                            // their SIGHASH, so we need to exclude txs with transparent inputs.
                            //
                            // References:
                            //
                            // <https://zips.z.cash/zip-0244#s-2c-amounts-sig-digest>
                            // <https://zips.z.cash/zip-0244#s-2d-scriptpubkeys-sig-digest>
                            if tx.has_transparent_inputs() {
                                continue;
                            }

                            let sighash = tx
                                .sighash(nu, HashType::ALL, Arc::new(Vec::new()), None)
                                .expect("network upgrade is valid for tx");

                            let bvk = redjubjub::VerificationKey::try_from(
                                sapling_shielded_data.binding_verification_key(),
                            )
                            .expect("a valid redjubjub::VerificationKey");

                            bvk.verify(sighash.as_ref(), &sapling_shielded_data.binding_sig)
                                .expect("verification passes");

                            at_least_one_v5_checked = true;
                        }
                    }
                }
            }
        }

        assert!(at_least_one_v4_checked);
        assert!(at_least_one_v5_checked);
    }
}

#[test]
fn test_coinbase_script() -> Result<()> {
    let _init_guard = zebra_test::init();

    let tx = hex::decode("0400008085202f89010000000000000000000000000000000000000000000000000000000000000000ffffffff0503b0e72100ffffffff04e8bbe60e000000001976a914ba92ff06081d5ff6542af8d3b2d209d29ba6337c88ac40787d010000000017a914931fec54c1fea86e574462cc32013f5400b891298738c94d010000000017a914c7a4285ed7aed78d8c0e28d7f1839ccb4046ab0c87286bee000000000017a914d45cb1adffb5215a42720532a076f02c7c778c908700000000b0e721000000000000000000000000").unwrap();

    let transaction = tx.zcash_deserialize_into::<Transaction>()?;

    let recoded_tx = transaction.zcash_serialize_to_vec().unwrap();
    assert_eq!(tx, recoded_tx);

    let data = transaction.inputs()[0].coinbase_script().unwrap();
    let expected = hex::decode("03b0e72100").unwrap();
    assert_eq!(data, expected);

    Ok(())
}

// Transaction V6 test vectors

#[cfg(all(zcash_unstable = "nu7", feature = "tx_v6"))]
mod v6_tests {
    use super::*;
    use group::ff::FromUniformBytes;
    use group::ff::PrimeField;
    use group::prime::PrimeCurveAffine;
    use halo2::pasta::pallas;
    use reddsa::{orchard::SpendAuth, SigningKey, VerificationKey};

    /// Helper: derive a valid ActionVerificationKey from a 64-byte seed.
    fn rk_from_seed(seed: [u8; 64]) -> zcash_tachyon::keys::public::ActionVerificationKey {
        let sk_scalar = pallas::Scalar::from_uniform_bytes(&seed);
        let sk_bytes = sk_scalar.to_repr();
        let sk = SigningKey::<SpendAuth>::try_from(sk_bytes).unwrap();
        let pk = VerificationKey::<SpendAuth>::from(&sk);
        zcash_tachyon::keys::public::ActionVerificationKey::try_from(<[u8; 32]>::from(pk)).unwrap()
    }

    /// Helper: derive Fp from a 64-byte seed.
    fn fp_from_seed(seed: [u8; 64]) -> pasta_curves::Fp {
        pasta_curves::Fp::from_uniform_bytes(&seed)
    }

    lazy_static! {
        /// An empty V6 transaction with no bundles at all.
        pub static ref EMPTY_V6_TX: Transaction = Transaction::V6 {
            network_upgrade: NetworkUpgrade::Nu7,
            lock_time: LockTime::min_lock_time_timestamp(),
            expiry_height: block::Height(0),
            zip233_amount: Amount::try_from(0).unwrap(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            sapling_shielded_data: None,
            orchard_shielded_data: None,
            tachyon_shielded_data: None,
        };

        /// V6 transaction with a tachyon bundle, stamp = None.
        pub static ref V6_TX_TACHYON_NO_STAMP: Transaction = {
            let rk = rk_from_seed([0x42u8; 64]);
            let action = zcash_tachyon::Action {
                cv: zcash_tachyon::value::Commitment::from(pasta_curves::EpAffine::generator()),
                rk,
                sig: zcash_tachyon::action::Signature::from([0x01u8; 64]),
            };
            Transaction::V6 {
                network_upgrade: NetworkUpgrade::Nu7,
                lock_time: LockTime::min_lock_time_timestamp(),
                expiry_height: block::Height(0),
                zip233_amount: Amount::try_from(0).unwrap(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                sapling_shielded_data: None,
                orchard_shielded_data: None,
                tachyon_shielded_data: Some(zcash_tachyon::Bundle {
                    actions: vec![action],
                    value_balance: 0i64,
                    binding_sig: zcash_tachyon::bundle::Signature::from([0x02u8; 64]),
                    stamp: None,
                }),
            }
        };

        /// V6 transaction with a tachyon bundle that includes a stamp with tachygrams.
        pub static ref V6_TX_TACHYON_WITH_STAMP: Transaction = {
            let rk = rk_from_seed([0x42u8; 64]);
            let action = zcash_tachyon::Action {
                cv: zcash_tachyon::value::Commitment::from(pasta_curves::EpAffine::generator()),
                rk,
                sig: zcash_tachyon::action::Signature::from([0x01u8; 64]),
            };
            let tachygram = zcash_tachyon::Tachygram::from(fp_from_seed([0xAAu8; 64]));
            let anchor = zcash_tachyon::Anchor::from(fp_from_seed([0xBBu8; 64]));
            Transaction::V6 {
                network_upgrade: NetworkUpgrade::Nu7,
                lock_time: LockTime::min_lock_time_timestamp(),
                expiry_height: block::Height(0),
                zip233_amount: Amount::try_from(0).unwrap(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                sapling_shielded_data: None,
                orchard_shielded_data: None,
                tachyon_shielded_data: Some(zcash_tachyon::Bundle {
                    actions: vec![action],
                    value_balance: 100i64,
                    binding_sig: zcash_tachyon::bundle::Signature::from([0x02u8; 64]),
                    stamp: Some(zcash_tachyon::Stamp {
                        tachygrams: vec![tachygram],
                        anchor,
                        proof: zcash_tachyon::Proof,
                    }),
                }),
            }
        };

        /// V6 transaction with multiple actions and multiple tachygrams.
        pub static ref V6_TX_TACHYON_MULTI_ACTION: Transaction = {
            let rk1 = rk_from_seed([0x42u8; 64]);
            let rk2 = rk_from_seed([0x43u8; 64]);
            let action1 = zcash_tachyon::Action {
                cv: zcash_tachyon::value::Commitment::from(pasta_curves::EpAffine::generator()),
                rk: rk1,
                sig: zcash_tachyon::action::Signature::from([0x01u8; 64]),
            };
            let action2 = zcash_tachyon::Action {
                cv: zcash_tachyon::value::Commitment::from(pasta_curves::EpAffine::generator()),
                rk: rk2,
                sig: zcash_tachyon::action::Signature::from([0x03u8; 64]),
            };
            let tg1 = zcash_tachyon::Tachygram::from(fp_from_seed([0xAAu8; 64]));
            let tg2 = zcash_tachyon::Tachygram::from(fp_from_seed([0xCCu8; 64]));
            let tg3 = zcash_tachyon::Tachygram::from(fp_from_seed([0xDDu8; 64]));
            let anchor = zcash_tachyon::Anchor::from(fp_from_seed([0xBBu8; 64]));
            Transaction::V6 {
                network_upgrade: NetworkUpgrade::Nu7,
                lock_time: LockTime::min_lock_time_timestamp(),
                expiry_height: block::Height(0),
                zip233_amount: Amount::try_from(500).unwrap(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                sapling_shielded_data: None,
                orchard_shielded_data: None,
                tachyon_shielded_data: Some(zcash_tachyon::Bundle {
                    actions: vec![action1, action2],
                    value_balance: 300i64,
                    binding_sig: zcash_tachyon::bundle::Signature::from([0x02u8; 64]),
                    stamp: Some(zcash_tachyon::Stamp {
                        tachygrams: vec![tg1, tg2, tg3],
                        anchor,
                        proof: zcash_tachyon::Proof,
                    }),
                }),
            }
        };
    }

    /// An empty V6 transaction round-trip test.
    #[test]
    fn empty_v6_round_trip() {
        let _init_guard = zebra_test::init();

        let tx: &Transaction = &EMPTY_V6_TX;

        let data = tx.zcash_serialize_to_vec().expect("tx should serialize");
        let tx2: &Transaction = &data
            .zcash_deserialize_into()
            .expect("tx should deserialize");

        assert_eq!(tx, tx2);

        let data2 = tx2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(data, data2, "data must be equal if structs are equal");
    }

    /// Generate and print V6 tachyon test vectors as hex.
    #[test]
    fn generate_v6_tachyon_test_vectors() {
        let _init_guard = zebra_test::init();

        let test_cases: &[(&str, &Transaction)] = &[
            ("EMPTY_V6_TX", &EMPTY_V6_TX),
            ("V6_TX_TACHYON_NO_STAMP", &V6_TX_TACHYON_NO_STAMP),
            ("V6_TX_TACHYON_WITH_STAMP", &V6_TX_TACHYON_WITH_STAMP),
            ("V6_TX_TACHYON_MULTI_ACTION", &V6_TX_TACHYON_MULTI_ACTION),
        ];

        for (name, tx) in test_cases {
            let bytes = tx
                .zcash_serialize_to_vec()
                .expect("tx should serialize");

            let tx2: Transaction = bytes
                .zcash_deserialize_into()
                .expect("tx should deserialize");

            assert_eq!(*tx, &tx2, "{name} round-trip failed");

            println!("{name}: {}", hex::encode(&bytes));
        }
    }

    /// Assert that V6 tachyon test vectors serialize to exact expected bytes.
    #[test]
    fn v6_tachyon_test_vectors_exact_encoding() {
        let _init_guard = zebra_test::init();

        let test_cases: &[(&str, &Transaction, &str)] = &[
            (
                "EMPTY_V6_TX",
                &EMPTY_V6_TX,
                "06000080ffffffffffffffff0065cd1d000000000000000000000000000000000000",
            ),
            (
                "V6_TX_TACHYON_NO_STAMP",
                &V6_TX_TACHYON_NO_STAMP,
                "06000080ffffffffffffffff0065cd1d0000000000000000000000000000000000010100000000ed302d991bf94c09fc98462200000000000000000000000000000040ba6454c4a1d42730b53cbf30d05d3f95aa541c98eba0205a75bb7983443b37310101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010100000000000000000202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020200",
            ),
            (
                "V6_TX_TACHYON_WITH_STAMP",
                &V6_TX_TACHYON_WITH_STAMP,
                "06000080ffffffffffffffff0065cd1d0000000000000000000000000000000000010100000000ed302d991bf94c09fc98462200000000000000000000000000000040ba6454c4a1d42730b53cbf30d05d3f95aa541c98eba0205a75bb7983443b37310101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010164000000000000000202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020201015f555555c6580ae6f8e822b5273ca4f06593dbd767c60fa50d7a6852ca2b9e1b4f44444458b35c8c947f94ae11ebee5823220b073f5a913542860cc19196c724",
            ),
            (
                "V6_TX_TACHYON_MULTI_ACTION",
                &V6_TX_TACHYON_MULTI_ACTION,
                "06000080ffffffffffffffff0065cd1d00000000f4010000000000000000000000010200000000ed302d991bf94c09fc98462200000000000000000000000000000040ba6454c4a1d42730b53cbf30d05d3f95aa541c98eba0205a75bb7983443b37310101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010101010100000000ed302d991bf94c09fc98462200000000000000000000000000000040336a1f7ed0903193f39fa5306f3fd88d8a0a8907a1defde547f117e7075d9e01030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303030303032c010000000000000202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020202020201035f555555c6580ae6f8e822b5273ca4f06593dbd767c60fa50d7a6852ca2b9e1b3f333333ea0daf32301606a8fb9939c1e0b03a3616ee12c67692b02f5901f12d2f2222227c6801d9cbac77a1e54884299e3f6a65ed819456ab9e549e206c1a374f44444458b35c8c947f94ae11ebee5823220b073f5a913542860cc19196c724",
            ),
        ];

        for (name, tx, expected_hex) in test_cases {
            let bytes = tx
                .zcash_serialize_to_vec()
                .expect("tx should serialize");

            assert_eq!(hex::encode(&bytes), *expected_hex, "{name} encoding mismatch");
        }
    }
}
