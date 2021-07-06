use std::convert::TryInto;

use color_eyre::eyre::Result;
use lazy_static::lazy_static;

use zebra_test::zip0244;

use super::super::*;
use crate::{
    block::{Block, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::{SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::{sighash::SigHasher, txid::TxIdBuilder},
};

use crate::{amount::Amount, transaction::Transaction};

use transparent::Script;
use zebra_test::vectors::{ZIP143_1, ZIP143_2, ZIP243_1, ZIP243_2, ZIP243_3};

macro_rules! assert_hash_eq {
    ($expected:literal, $hasher:expr, $f:ident) => {
        let mut buf = vec![];
        $hasher
            .$f(&mut buf)
            .expect("hashing into a vec never fails");
        let expected = $expected;
        let result = hex::encode(buf);
        let span = tracing::span!(
            tracing::Level::ERROR,
            "compare_vecs",
            expected.len = expected.len(),
            result.len = result.len(),
            hash_fn = stringify!($f)
        );
        let guard = span.enter();
        assert_eq!(expected, result);
        drop(guard);
    };
}

lazy_static! {
    pub static ref EMPTY_V5_TX: Transaction = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        lock_time: LockTime::min_lock_time(),
        expiry_height: block::Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };
}

#[test]
fn librustzcash_tx_deserialize_and_round_trip() {
    zebra_test::init();

    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::GENERIC_TESTNET_TX[..])
        .expect("transaction test vector from librustzcash should deserialize");

    let mut data2 = Vec::new();
    tx.zcash_serialize(&mut data2).expect("tx should serialize");

    assert_eq!(&zebra_test::vectors::GENERIC_TESTNET_TX[..], &data2[..]);
}

#[test]
fn librustzcash_tx_hash() {
    zebra_test::init();

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
    zebra_test::init();

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
    zebra_test::init();

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
    zebra_test::init();

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

// Transaction V5 test vectors

/// An empty transaction v5, with no Orchard, Sapling, or Transparent data
///
/// empty transaction are invalid, but Zebra only checks this rule in
/// zebra_consensus::transaction::Verifier
#[test]
fn empty_v5_round_trip() {
    zebra_test::init();

    let tx: &Transaction = &*EMPTY_V5_TX;

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
    zebra_test::init();

    let tx = Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::min_lock_time(),
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
    zebra_test::init();

    let tx: &Transaction = &*EMPTY_V5_TX;
    let _alt_tx: zcash_primitives::transaction::Transaction = tx.try_into().expect(
        "librustzcash deserialization might work for empty zebra serialized transactions. \
        Hint: if empty transactions fail, but other transactions work, delete this test",
    );
}

/// Do a round-trip test on fake v5 transactions created from v4 transactions
/// in the block test vectors.
///
/// Covers Sapling only, Transparent only, and Sapling/Transparent v5
/// transactions.
#[test]
fn fake_v5_round_trip() {
    zebra_test::init();

    fake_v5_round_trip_for_network(Network::Mainnet);
    fake_v5_round_trip_for_network(Network::Testnet);
}

fn fake_v5_round_trip_for_network(network: Network) {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    let overwinter_activation_height = NetworkUpgrade::Overwinter
        .activation_height(network)
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
            .map(|t| arbitrary::transaction_to_fake_v5(t, network, Height(*height)))
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

#[test]
fn invalid_orchard_nullifier() {
    zebra_test::init();

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
    zebra_test::init();

    fake_v5_librustzcash_round_trip_for_network(Network::Mainnet);
    fake_v5_librustzcash_round_trip_for_network(Network::Testnet);
}

fn fake_v5_librustzcash_round_trip_for_network(network: Network) {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    let overwinter_activation_height = NetworkUpgrade::Overwinter
        .activation_height(network)
        .expect("a valid height")
        .0;

    // skip blocks that are before overwinter as they will not have a valid consensus branch id
    let blocks_after_overwinter =
        block_iter.skip_while(|(height, _)| **height < overwinter_activation_height);

    for (height, original_bytes) in blocks_after_overwinter {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let mut fake_block = original_block.clone();
        fake_block.transactions = fake_block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(|t| arbitrary::transaction_to_fake_v5(t, network, Height(*height)))
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

#[test]
fn zip244_round_trip() -> Result<()> {
    zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let reencoded = transaction.zcash_serialize_to_vec()?;
        assert_eq!(test.tx, reencoded);

        let _alt_tx: zcash_primitives::transaction::Transaction = (&transaction)
            .try_into()
            .expect("librustzcash deserialization must work for zebra serialized transactions");
    }

    Ok(())
}

#[test]
fn zip244_txid() -> Result<()> {
    zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let hasher = TxIdBuilder::new(&transaction);
        let txid = hasher.txid()?;
        assert_eq!(txid.0, test.txid);
    }

    Ok(())
}

#[test]
fn test_vec143_1() -> Result<()> {
    zebra_test::init();

    let transaction = ZIP143_1.zcash_deserialize_into::<Transaction>()?;

    let hasher = SigHasher::new(
        &transaction,
        HashType::ALL,
        NetworkUpgrade::Overwinter,
        None,
    );

    assert_hash_eq!("03000080", hasher, hash_header);

    assert_hash_eq!("7082c403", hasher, hash_groupid);

    assert_hash_eq!(
        "d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136",
        hasher,
        hash_prevouts
    );

    assert_hash_eq!(
        "a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272",
        hasher,
        hash_sequence
    );

    assert_hash_eq!(
        "ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a",
        hasher,
        hash_outputs
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_joinsplits
    );

    assert_hash_eq!("481cdd86", hasher, hash_lock_time);

    assert_hash_eq!("b3cc4318", hasher, hash_expiry_height);

    assert_hash_eq!("01000000", hasher, hash_hash_type);

    assert_hash_eq!(
        "030000807082c403d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a0000000000000000000000000000000000000000000000000000000000000000481cdd86b3cc431801000000",
        hasher,
        hash_sighash_zip143
    );

    let hash = hasher.sighash();
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
    zebra_test::init();

    let transaction = ZIP143_2.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("2f6e04963b4c0100")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&hex::decode("53")?);
    let input_ind = 1;

    let hasher = SigHasher::new(
        &transaction,
        HashType::SINGLE,
        NetworkUpgrade::Overwinter,
        Some((input_ind, transparent::Output { value, lock_script })),
    );

    assert_hash_eq!("03000080", hasher, hash_header);

    assert_hash_eq!("7082c403", hasher, hash_groupid);

    assert_hash_eq!(
        "92b8af1f7e12cb8de105af154470a2ae0a11e64a24a514a562ff943ca0f35d7f",
        hasher,
        hash_prevouts
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_sequence
    );

    assert_hash_eq!(
        "edc32cce530f836f7c31c53656f859f514c3ff8dcae642d3e17700fdc6e829a4",
        hasher,
        hash_outputs
    );

    assert_hash_eq!(
        "f59e41b40f3a60be90bee2be11b0956dfff06a6d8e22668c4f215bd87b20d514",
        hasher,
        hash_joinsplits
    );

    assert_hash_eq!("97b0e4e4", hasher, hash_lock_time);

    assert_hash_eq!("c705fc05", hasher, hash_expiry_height);

    assert_hash_eq!("03000000", hasher, hash_hash_type);

    assert_hash_eq!(
        "378af1e40f64e125946f62c2fa7b2fecbcb64b6968912a6381ce3dc166d56a1d62f5a8d7",
        hasher,
        hash_input_prevout
    );

    assert_hash_eq!("0153", hasher, hash_input_script_code);

    assert_hash_eq!("2f6e04963b4c0100", hasher, hash_input_amount);

    assert_hash_eq!("e8c7203d", hasher, hash_input_sequence);

    let hash = hasher.sighash();
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
    zebra_test::init();

    let transaction = ZIP243_1.zcash_deserialize_into::<Transaction>()?;

    let hasher = SigHasher::new(&transaction, HashType::ALL, NetworkUpgrade::Sapling, None);

    assert_hash_eq!("04000080", hasher, hash_header);

    assert_hash_eq!("85202f89", hasher, hash_groupid);

    assert_hash_eq!(
        "d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136",
        hasher,
        hash_prevouts
    );

    assert_hash_eq!(
        "a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272",
        hasher,
        hash_sequence
    );

    assert_hash_eq!(
        "ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a",
        hasher,
        hash_outputs
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_joinsplits
    );

    assert_hash_eq!(
        "3fd9edb96dccf5b9aeb71e3db3710e74be4f1dfb19234c1217af26181f494a36",
        hasher,
        hash_shielded_spends
    );

    assert_hash_eq!(
        "dafece799f638ba7268bf8fe43f02a5112f0bb32a84c4a8c2f508c41ff1c78b5",
        hasher,
        hash_shielded_outputs
    );

    assert_hash_eq!("481cdd86", hasher, hash_lock_time);

    assert_hash_eq!("b3cc4318", hasher, hash_expiry_height);

    assert_hash_eq!("442117623ceb0500", hasher, hash_value_balance);

    assert_hash_eq!("01000000", hasher, hash_hash_type);

    assert_hash_eq!(
        "0400008085202f89d53a633bbecf82fe9e9484d8a0e727c73bb9e68c96e72dec30144f6a84afa136a5f25f01959361ee6eb56a7401210ee268226f6ce764a4f10b7f29e54db37272ab6f7f6c5ad6b56357b5f37e16981723db6c32411753e28c175e15589172194a00000000000000000000000000000000000000000000000000000000000000003fd9edb96dccf5b9aeb71e3db3710e74be4f1dfb19234c1217af26181f494a36dafece799f638ba7268bf8fe43f02a5112f0bb32a84c4a8c2f508c41ff1c78b5481cdd86b3cc4318442117623ceb050001000000",
        hasher,
        hash_sighash_zip243
    );

    let hash = hasher.sighash();
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

    let alt_sighash = crate::primitives::zcash_primitives::sighash(
        &transaction,
        HashType::ALL,
        NetworkUpgrade::Sapling,
        None,
    );
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec243_2() -> Result<()> {
    zebra_test::init();

    let transaction = ZIP243_2.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("adedf02996510200")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&[]);
    let input_ind = 1;

    let hasher = SigHasher::new(
        &transaction,
        HashType::NONE,
        NetworkUpgrade::Sapling,
        Some((input_ind, transparent::Output { value, lock_script })),
    );

    assert_hash_eq!("04000080", hasher, hash_header);

    assert_hash_eq!("85202f89", hasher, hash_groupid);

    assert_hash_eq!(
        "cacf0f5210cce5fa65a59f314292b3111d299e7d9d582753cf61e1e408552ae4",
        hasher,
        hash_prevouts
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_sequence
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_outputs
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_joinsplits
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_shielded_spends
    );

    assert_hash_eq!(
        "b79530fcec83211d21e3c355db538c138d625784c27370e9d1039a8515a23f87",
        hasher,
        hash_shielded_outputs
    );

    assert_hash_eq!("d7034302", hasher, hash_lock_time);

    assert_hash_eq!("011b9a07", hasher, hash_expiry_height);

    assert_hash_eq!("6620edc067ff0200", hasher, hash_value_balance);

    assert_hash_eq!("02000000", hasher, hash_hash_type);

    assert_hash_eq!(
        "090f47a068e227433f9e49d3aa09e356d8d66d0c0121e91a3c4aa3f27fa1b63396e2b41d",
        hasher,
        hash_input_prevout
    );

    assert_hash_eq!("00", hasher, hash_input_script_code);

    assert_hash_eq!("adedf02996510200", hasher, hash_input_amount);

    assert_hash_eq!("4e970568", hasher, hash_input_sequence);

    assert_hash_eq!(
        "0400008085202f89cacf0f5210cce5fa65a59f314292b3111d299e7d9d582753cf61e1e408552ae40000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000b79530fcec83211d21e3c355db538c138d625784c27370e9d1039a8515a23f87d7034302011b9a076620edc067ff020002000000090f47a068e227433f9e49d3aa09e356d8d66d0c0121e91a3c4aa3f27fa1b63396e2b41d00adedf029965102004e970568",
        hasher,
        hash_sighash_zip243
    );

    let hash = hasher.sighash();
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
    let prevout = transparent::Output { value, lock_script };
    let index = input_ind as usize;
    let inputs = transaction.inputs();
    let input = Some((&prevout, &inputs[index], index));

    let alt_sighash = crate::primitives::zcash_primitives::sighash(
        &transaction,
        HashType::NONE,
        NetworkUpgrade::Sapling,
        input,
    );
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn test_vec243_3() -> Result<()> {
    zebra_test::init();

    let transaction = ZIP243_3.zcash_deserialize_into::<Transaction>()?;

    let value = hex::decode("80f0fa0200000000")?.zcash_deserialize_into::<Amount<_>>()?;
    let lock_script = Script::new(&hex::decode(
        "76a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
    )?);
    let input_ind = 0;

    let hasher = SigHasher::new(
        &transaction,
        HashType::ALL,
        NetworkUpgrade::Sapling,
        Some((input_ind, transparent::Output { value, lock_script })),
    );

    assert_hash_eq!("04000080", hasher, hash_header);

    assert_hash_eq!("85202f89", hasher, hash_groupid);

    assert_hash_eq!(
        "fae31b8dec7b0b77e2c8d6b6eb0e7e4e55abc6574c26dd44464d9408a8e33f11",
        hasher,
        hash_prevouts
    );

    assert_hash_eq!(
        "6c80d37f12d89b6f17ff198723e7db1247c4811d1a695d74d930f99e98418790",
        hasher,
        hash_sequence
    );

    assert_hash_eq!(
        "d2b04118469b7810a0d1cc59568320aad25a84f407ecac40b4f605a4e6868454",
        hasher,
        hash_outputs
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_joinsplits
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_shielded_spends
    );

    assert_hash_eq!(
        "0000000000000000000000000000000000000000000000000000000000000000",
        hasher,
        hash_shielded_outputs
    );

    assert_hash_eq!("29b00400", hasher, hash_lock_time);

    assert_hash_eq!("48b00400", hasher, hash_expiry_height);

    assert_hash_eq!("0000000000000000", hasher, hash_value_balance);

    assert_hash_eq!("01000000", hasher, hash_hash_type);

    assert_hash_eq!(
        "a8c685478265f4c14dada651969c45a65e1aeb8cd6791f2f5bb6a1d9952104d901000000",
        hasher,
        hash_input_prevout
    );

    assert_hash_eq!(
        "1976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac",
        hasher,
        hash_input_script_code
    );

    assert_hash_eq!("80f0fa0200000000", hasher, hash_input_amount);

    assert_hash_eq!("feffffff", hasher, hash_input_sequence);

    assert_hash_eq!(
        "0400008085202f89fae31b8dec7b0b77e2c8d6b6eb0e7e4e55abc6574c26dd44464d9408a8e33f116c80d37f12d89b6f17ff198723e7db1247c4811d1a695d74d930f99e98418790d2b04118469b7810a0d1cc59568320aad25a84f407ecac40b4f605a4e686845400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000029b0040048b00400000000000000000001000000a8c685478265f4c14dada651969c45a65e1aeb8cd6791f2f5bb6a1d9952104d9010000001976a914507173527b4c3318a2aecd793bf1cfed705950cf88ac80f0fa0200000000feffffff",
        hasher,
        hash_sighash_zip243
    );

    let hash = hasher.sighash();
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
    let prevout = transparent::Output { value, lock_script };
    let index = input_ind as usize;
    let inputs = transaction.inputs();
    let input = Some((&prevout, &inputs[index], index));

    let alt_sighash = crate::primitives::zcash_primitives::sighash(
        &transaction,
        HashType::ALL,
        NetworkUpgrade::Sapling,
        input,
    );
    let result = hex::encode(alt_sighash);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn zip244_sighash() -> Result<()> {
    zebra_test::init();

    for (i, test) in zip0244::TEST_VECTORS.iter().enumerate() {
        let transaction = test.tx.zcash_deserialize_into::<Transaction>()?;
        let input = match test.amount {
            Some(amount) => Some((
                test.transparent_input
                    .expect("test vector must have transparent_input when it has amount"),
                transparent::Output {
                    value: amount.try_into()?,
                    lock_script: transparent::Script::new(
                        test.script_code
                            .as_ref()
                            .expect("test vector must have script_code when it has amount"),
                    ),
                },
            )),
            None => None,
        };
        let result = hex::encode(transaction.sighash(NetworkUpgrade::Nu5, HashType::ALL, input));
        let expected = hex::encode(test.sighash_all);
        assert_eq!(expected, result, "test #{}: sighash does not match", i);
    }

    Ok(())
}

/// Use librustzcash to compute sighashes and compare with zebra sighashes.
#[test]
fn librustzcash_sighash() {
    zebra_test::init();

    librustzcash_sighash_for_network(Network::Mainnet);
    librustzcash_sighash_for_network(Network::Testnet);
}

fn librustzcash_sighash_for_network(network: Network) {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (height, original_bytes) in block_iter {
        let original_block = original_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // skip blocks that are before overwinter as they will not have a valid consensus branch id
        if *height
            < NetworkUpgrade::Overwinter
                .activation_height(network)
                .expect("a valid height")
                .0
        {
            continue;
        }

        let network_upgrade = NetworkUpgrade::current(network, Height(*height));

        // Test each transaction. Skip the coinbase transaction
        for original_tx in original_block.transactions.iter().skip(1) {
            let original_sighash = original_tx.sighash(network_upgrade, HashType::ALL, None);

            let alt_sighash = crate::primitives::zcash_primitives::sighash(
                original_tx,
                HashType::ALL,
                network_upgrade,
                None,
            );

            assert_eq!(original_sighash, alt_sighash);
        }
    }
}
