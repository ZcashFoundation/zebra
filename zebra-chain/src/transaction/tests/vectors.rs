use super::super::*;

use crate::{
    block::{Block, Height, MAX_BLOCK_BYTES},
    parameters::{Network, NetworkUpgrade},
    serialization::{SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use std::convert::TryInto;

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

    let tx = Transaction::V5 {
        network_upgrade: NetworkUpgrade::Nu5,
        lock_time: LockTime::min_lock_time(),
        expiry_height: block::Height(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        sapling_shielded_data: None,
        orchard_shielded_data: None,
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
