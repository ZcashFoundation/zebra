//! Fixed test vectors for transactions.

use chrono::DateTime;
use color_eyre::eyre::Result;
use lazy_static::lazy_static;

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

    #[cfg(feature = "tx-v6")]
    pub static ref EMPTY_V6_TX: Transaction = Transaction::V6 {
        network_upgrade: NetworkUpgrade::Nu7,
        lock_time: LockTime::min_lock_time_timestamp(),
        expiry_height: block::Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
        orchard_zsa_issue_data: None
    };
}

/// Build a mock output list for pre-V5 transactions, with (index+1)
/// copies of `output`, which is used to computed the sighash.
///
/// Pre-V5, the entire output list is not used; only the output for the
/// given index is read. Therefore, we just need a list where `array[index]`
/// is the given `output`.
fn mock_pre_v5_output_list(output: transparent::Output, index: usize) -> Vec<transparent::Output> {
    iter::repeat(output).take(index + 1).collect()
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
    let inputs = std::iter::repeat(input)
        .take(tx_inputs_num)
        .collect::<Vec<_>>();

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

fn tx_round_trip(tx: &Transaction) {
    let _init_guard = zebra_test::init();

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

/// An empty transaction v5, with no Orchard, Sapling, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v5_round_trip() {
    tx_round_trip(&EMPTY_V5_TX)
}

#[cfg(feature = "tx-v6")]
/// An empty transaction v6, with no Orchard/OrchardZSA, Sapling, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v6_round_trip() {
    tx_round_trip(&EMPTY_V6_TX)
}

fn tx_librustzcash_round_trip(tx: &Transaction) {
    let _init_guard = zebra_test::init();

    let _alt_tx: zcash_primitives::transaction::Transaction = tx.try_into().expect(
        "librustzcash deserialization might work for empty zebra serialized transactions. \
        Hint: if empty transactions fail, but other transactions work, delete this test",
    );
}

/// Check if an empty V5 transaction can be deserialized by librustzcash too.
#[test]
fn empty_v5_librustzcash_round_trip() {
    tx_librustzcash_round_trip(&EMPTY_V5_TX);
}

#[cfg(feature = "tx-v6")]
/// Check if an empty V6 transaction can be deserialized by librustzcash too.
#[test]
fn empty_v6_librustzcash_round_trip() {
    tx_librustzcash_round_trip(&EMPTY_V6_TX);
}

/// Do a round-trip test on fake v5 transactions created from v4 transactions
/// in the block test vectors.
///
/// Covers Sapling only, Transparent only, and Sapling/Transparent v5
/// transactions.
#[test]
fn fake_v5_round_trip() {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        fake_v5_round_trip_for_network(network);
    }
}

fn fake_v5_round_trip_for_network(network: Network) {
    let block_iter = network.block_iter();

    let overwinter_activation_height = NetworkUpgrade::Overwinter
        .activation_height(&network)
        .expect("a valid height")
        .0;

    // skip blocks that are before overwinter as they will not have a valid consensus branch id
    let blocks_after_overwinter =
        block_iter.skip_while(|(height, _)| **height < overwinter_activation_height);

    for (height, original_bytes) in blocks_after_overwinter {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // skip this block if it only contains v5 transactions,
        // the block round-trip test covers it already
        if original_block
            .transactions
            .iter()
            .all(|trans| matches!(trans.as_ref(), &Transaction::V5 { .. }))
        {
            continue;
        }

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

            let fake_tx2 = fake_bytes
                .zcash_deserialize_into::<Transaction>()
                .expect("tx is structurally valid");

            assert_eq!(fake_tx.as_ref(), &fake_tx2);

            let fake_bytes2 = fake_tx2
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            assert_eq!(
                fake_bytes, fake_bytes2,
                "data must be equal if structs are equal"
            );
        }

        // test full blocks
        assert_ne!(
            &original_block, &fake_block,
            "v1-v4 transactions must change when converted to fake v5"
        );

        let fake_bytes = fake_block
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_ne!(
            &original_bytes[..],
            fake_bytes,
            "v1-v4 transaction data must change when converted to fake v5"
        );

        // skip fake blocks which exceed the block size limit
        if fake_bytes.len() > MAX_BLOCK_BYTES.try_into().unwrap() {
            continue;
        }

        let fake_block2 = fake_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        assert_eq!(fake_block, fake_block2);

        let fake_bytes2 = fake_block2
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(
            fake_bytes, fake_bytes2,
            "data must be equal if structs are equal"
        );
    }
}

#[cfg(feature = "tx-v6")]
/// Do a serialization round-trip on OrchardZSA workflow blocks and their V6
/// transactions.
#[test]
fn v6_round_trip() {
    use zebra_test::vectors::ORCHARD_ZSA_WORKFLOW_BLOCKS;

    let _init_guard = zebra_test::init();

    for block_bytes in ORCHARD_ZSA_WORKFLOW_BLOCKS.iter() {
        let block = block_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // test full blocks
        let block_bytes2 = block
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(
            block_bytes, &block_bytes2,
            "data must be equal if structs are equal"
        );

        // test each transaction
        for tx in &block.transactions {
            let tx_bytes = tx
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            let tx2 = tx_bytes
                .zcash_deserialize_into::<Transaction>()
                .expect("tx is structurally valid");

            assert_eq!(tx.as_ref(), &tx2);

            let tx_bytes2 = tx2
                .zcash_serialize_to_vec()
                .expect("vec serialization is infallible");

            assert_eq!(
                tx_bytes, tx_bytes2,
                "data must be equal if structs are equal"
            );
        }
    }
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

            let _alt_tx: zcash_primitives::transaction::Transaction = fake_tx
                .as_ref()
                .try_into()
                .expect("librustzcash deserialization must work for zebra serialized transactions");
        }
    }
}

#[cfg(feature = "tx-v6")]
/// Confirms each V6 transaction in the OrchardZSA test blocks converts to librustzcash’s
/// transaction type without error.
#[test]
fn v6_librustzcash_tx_conversion() {
    use zebra_test::vectors::ORCHARD_ZSA_WORKFLOW_BLOCKS;

    let _init_guard = zebra_test::init();

    for block_bytes in ORCHARD_ZSA_WORKFLOW_BLOCKS.iter() {
        let block = block_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // Test each V6 transaction
        for tx in block
            .transactions
            .iter()
            .filter(|tx| matches!(tx.as_ref(), &Transaction::V6 { .. }))
        {
            let _alt_tx: zcash_primitives::transaction::Transaction = tx
                .as_ref()
                .try_into()
                .expect("librustzcash conversion must work for zebra transactions");
        }
    }
}

#[test]
fn zip244_round_trip() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let reencoded = transaction.zcash_serialize_to_vec()?;
        assert_eq!(test.tx, reencoded);

        // The borrow is actually needed to call the correct trait impl
        #[allow(clippy::needless_borrow)]
        let _alt_tx: zcash_primitives::transaction::Transaction = (&transaction)
            .try_into()
            .expect("librustzcash deserialization must work for zebra serialized transactions");
    }

    Ok(())
}

#[test]
fn zip244_txid() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let hasher = TxIdBuilder::new(&transaction);
        let txid = hasher.txid()?;
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
        NetworkUpgrade::Overwinter.branch_id().unwrap(),
        &[],
    );

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
        NetworkUpgrade::Overwinter.branch_id().unwrap(),
        &all_previous_outputs,
    );

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

    let hasher = SigHasher::new(
        &transaction,
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        &[],
    );

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

    let precomputed_tx_data = PrecomputedTxData::new(
        &transaction,
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        &[],
    );
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
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        &all_previous_outputs,
    );

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
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        &all_previous_outputs,
    );
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
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        &all_previous_outputs,
    );

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

    let all_previous_outputs = &[prevout];
    let precomputed_tx_data = PrecomputedTxData::new(
        &transaction,
        NetworkUpgrade::Sapling.branch_id().unwrap(),
        all_previous_outputs,
    );
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
        let result = hex::encode(transaction.sighash(
            ConsensusBranchId(test.consensus_branch_id),
            HashType::from_bits(test.hash_type).expect("must be a valid HashType"),
            &all_previous_outputs,
            input_index.map(|input_index| {
                (
                    input_index,
                    output.unwrap().lock_script.as_raw_bytes().to_vec(),
                )
            }),
        ));
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
        let result = hex::encode(transaction.sighash(
            ConsensusBranchId(test.consensus_branch_id),
            HashType::from_bits(test.hash_type).expect("must be a valid HashType"),
            &all_previous_outputs,
            input_index.map(|input_index| {
                (
                    input_index,
                    output.unwrap().lock_script.as_raw_bytes().to_vec(),
                )
            }),
        ));
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

        let all_previous_outputs: Vec<_> = test
            .amounts
            .iter()
            .zip(test.script_pubkeys.iter())
            .map(|(amount, script_pubkey)| transparent::Output {
                value: (*amount).try_into().unwrap(),
                lock_script: transparent::Script::new(script_pubkey.as_ref()),
            })
            .collect();

        let result = hex::encode(transaction.sighash(
            NetworkUpgrade::Nu5.branch_id().unwrap(),
            HashType::ALL,
            &all_previous_outputs,
            None,
        ));
        let expected = hex::encode(test.sighash_shielded);
        assert_eq!(expected, result, "test #{i}: sighash does not match");

        if let Some(sighash_all) = test.sighash_all {
            let result = hex::encode(
                transaction.sighash(
                    NetworkUpgrade::Nu5.branch_id().unwrap(),
                    HashType::ALL,
                    &all_previous_outputs,
                    test.transparent_input
                        .map(|idx| (idx as _, test.script_pubkeys[idx as usize].clone())),
                ),
            );
            let expected = hex::encode(sighash_all);
            assert_eq!(expected, result, "test #{i}: sighash does not match");
        }
    }

    Ok(())
}

#[test]
fn binding_signatures() {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        binding_signatures_for_network(network);
    }
}

fn binding_signatures_for_network(network: Network) {
    let block_iter = network.block_iter();

    for (height, bytes) in block_iter {
        let upgrade = NetworkUpgrade::current(&network, Height(*height));

        let block = bytes
            .zcash_deserialize_into::<Block>()
            .expect("a valid block");

        for tx in block.transactions {
            match &*tx {
                Transaction::V1 { .. } | Transaction::V2 { .. } | Transaction::V3 { .. } => (),
                Transaction::V4 {
                    sapling_shielded_data,
                    ..
                } => {
                    if let Some(sapling_shielded_data) = sapling_shielded_data {
                        let shielded_sighash =
                            tx.sighash(upgrade.branch_id().unwrap(), HashType::ALL, &[], None);

                        let bvk = redjubjub::VerificationKey::try_from(
                            sapling_shielded_data.binding_verification_key(),
                        )
                        .expect("a valid redjubjub::VerificationKey");

                        bvk.verify(
                            shielded_sighash.as_ref(),
                            &sapling_shielded_data.binding_sig,
                        )
                        .expect("must pass verification");
                    }
                }
                tx_v5_and_v6! {
                    sapling_shielded_data,
                    ..
                } => {
                    if let Some(sapling_shielded_data) = sapling_shielded_data {
                        let shielded_sighash =
                            tx.sighash(upgrade.branch_id().unwrap(), HashType::ALL, &[], None);

                        let bvk = redjubjub::VerificationKey::try_from(
                            sapling_shielded_data.binding_verification_key(),
                        )
                        .expect("a valid redjubjub::VerificationKey");

                        bvk.verify(
                            shielded_sighash.as_ref(),
                            &sapling_shielded_data.binding_sig,
                        )
                        .expect("must pass verification");
                    }
                }
            }
        }
    }
}
