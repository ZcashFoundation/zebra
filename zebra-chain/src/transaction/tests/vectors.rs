//! Fixed test vectors for transactions.

use arbitrary::v5_transactions;
use chrono::DateTime;
use color_eyre::eyre::Result;
use lazy_static::lazy_static;
use rand::{seq::IteratorRandom, thread_rng};

use std::sync::Arc;

use crate::{
    block::{Block, Height, MAX_BLOCK_BYTES},
    orchard,
    parameters::Network,
    primitives::zcash_primitives::PrecomputedTxData,
    serialization::{SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    transaction::sighash::SigHasher,
    transparent::Script,
};

use zebra_test::{
    vectors::{ZIP143_1, ZIP143_2, ZIP243_1, ZIP243_2, ZIP243_3},
    zip0143, zip0243, zip0244,
};

use super::super::*;

lazy_static! {
    pub static ref EMPTY_V5_TX: Transaction = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        Vec::new(),
        Vec::new(),
        LockTime::min_lock_time_timestamp(),
        block::Height(0),
    );
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

    let dummy_transaction =
        Transaction::test_v4(vec![], vec![], LockTime::Height(Height(1)), Height(10));

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

/// The V4 `valueBalanceSapling` check must locate the field correctly when the transaction also
/// carries Sprout JoinSplits, which sit after it on the wire.
///
/// This is the case the old check could not distinguish: it compared the whole re-serialization,
/// so *any* byte difference was reported as a bad balance.
#[test]
fn v4_value_balance_check_locates_the_field_with_joinsplits() {
    let _init_guard = zebra_test::init();

    let block: Block = zebra_test::vectors::BLOCK_MAINNET_419201_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded test vector must deserialize");

    // A V4 transaction with JoinSplits and no Sapling bundle, so the check applies and the
    // Sprout section is non-empty.
    let tx = block
        .transactions
        .iter()
        .find(|tx| {
            tx.tx_version() == TxVersion::V4
                && tx.sprout_joinsplit_descriptions().next().is_some()
                && tx.sapling_bundle().is_none()
        })
        .expect("block 419,201 has a V4 JoinSplit transaction with no Sapling bundle");

    let mut bytes = tx
        .zcash_serialize_to_vec()
        .expect("a mined transaction must serialize");

    // Round-tripping the untouched bytes must succeed: the balance really is zero.
    Transaction::zcash_deserialize(&bytes[..])
        .expect("an unmodified mined transaction must deserialize");

    // Now make `valueBalanceSapling` non-zero. It sits immediately before the two zero
    // Sapling count bytes, which are followed by the Sprout section.
    let joinsplit_count = tx.sprout_joinsplit_descriptions().count();
    const V4_JOINSPLIT_SIZE: usize = (2 * 8) + (9 * 32) + 192 + (2 * 601);
    let sprout_size = 1 + joinsplit_count * V4_JOINSPLIT_SIZE + 32 + 64;
    let counts_start = bytes.len() - sprout_size - 2;

    assert_eq!(
        &bytes[counts_start..counts_start + 2],
        &[0x00, 0x00],
        "the Sapling spend and output counts must be zero",
    );

    let value_balance_start = counts_start - 8;
    bytes[value_balance_start] = 1;

    assert!(
        matches!(
            Transaction::zcash_deserialize(&bytes[..]),
            Err(SerializationError::BadTransactionBalance)
        ),
        "a non-zero valueBalanceSapling with no Sapling bundle must be rejected",
    );
}

/// Every transaction in a block must round-trip when parsed individually.
///
/// The `valueBalanceSapling` check reads back the bytes consumed while parsing. Those bytes come
/// from a reader that is shared across the block's transactions, so any read-ahead would leave
/// trailing bytes behind and shift the field offsets. Block 419,201 contains several V4
/// transactions, so this exercises that path.
#[test]
fn v4_transactions_in_one_block_all_round_trip() {
    let _init_guard = zebra_test::init();

    let block: Block = zebra_test::vectors::BLOCK_MAINNET_419201_BYTES
        .zcash_deserialize_into()
        .expect("hard-coded test vector must deserialize");

    let v4_count = block
        .transactions
        .iter()
        .filter(|tx| tx.tx_version() == TxVersion::V4)
        .count();
    assert!(
        v4_count > 1,
        "this test needs a block with several V4 transactions, found {v4_count}",
    );

    for tx in &block.transactions {
        let bytes = tx.zcash_serialize_to_vec().expect("transaction serializes");
        let round_tripped =
            Transaction::zcash_deserialize(&bytes[..]).expect("transaction round-trips");
        assert_eq!(tx.hash(), round_tripped.hash());
    }

    // The block as a whole must also round-trip, which reads the transactions sequentially
    // from one reader.
    let block_bytes = block.zcash_serialize_to_vec().expect("block serializes");
    let round_tripped: Block = block_bytes
        .zcash_deserialize_into()
        .expect("block round-trips");
    assert_eq!(block.hash(), round_tripped.hash());
}

/// The hand-written `serde::Serialize` impl must give V6 its own variant.
///
/// This impl backs the `elasticsearch` feature as well as tests, and V6 transactions have been on
/// mainnet since NU6.3, so mislabelling them as V5 would silently drop the Ironwood bundle from
/// indexed output.
#[test]
fn serde_serialize_labels_v6_transactions_correctly() {
    use crate::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

    let _init_guard = zebra_test::init();

    let ironwood = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        ::orchard::ValuePool::Ironwood,
        1,
        3,
    )
    .expect("the Ironwood pool is defined at NU6.3");

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    assert_eq!(tx.version(), 6);

    let rendered = format!("{:?}", serde_json::to_value(&tx).expect("V6 tx serializes"));

    assert!(
        rendered.contains("V6"),
        "a V6 transaction must serialize under its own variant name, got: {rendered}",
    );
    assert!(
        rendered.contains("ironwood_shielded_data"),
        "a V6 transaction must serialize its Ironwood bundle, got: {rendered}",
    );

    // A V5 transaction must still be labelled V5 and have no Ironwood field.
    let v5 = Transaction::test_v5(
        NetworkUpgrade::Nu5,
        Vec::new(),
        Vec::new(),
        LockTime::unlocked(),
        block::Height(0),
    );
    let rendered_v5 = format!("{:?}", serde_json::to_value(&v5).expect("V5 tx serializes"));

    assert!(rendered_v5.contains("V5"));
    assert!(!rendered_v5.contains("ironwood_shielded_data"));
}

/// The sighasher must expose a v6 transaction's Ironwood bundle.
///
/// `PrecomputedTxData` rebuilds the transaction to correct the consensus branch ID, and
/// `TransactionData::from_parts` drops the Ironwood bundle. The consensus verifier only queues an
/// Ironwood Halo2 proof check when `SigHasher::ironwood_bundle` returns `Some`, so losing it here
/// means Ironwood proofs are never verified.
#[test]
fn sighasher_preserves_the_ironwood_bundle() {
    use crate::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

    let _init_guard = zebra_test::init();

    let ironwood = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        ::orchard::ValuePool::Ironwood,
        2,
        21,
    )
    .expect("the Ironwood pool is defined at NU6.3");

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    assert!(tx.has_ironwood_shielded_data());

    let sighasher = tx
        .sighasher(NetworkUpgrade::Nu6_3, std::sync::Arc::new(Vec::new()))
        .expect("a v6 transaction at NU6.3 has a valid branch ID");

    let bundle = sighasher
        .ironwood_bundle()
        .expect("the sighasher must expose the Ironwood bundle, or its proof is never verified");
    assert_eq!(bundle.actions().len(), 2);

    // The Orchard slot must stay empty rather than being filled from the Ironwood one.
    assert!(sighasher.orchard_bundle().is_none());
}

/// A transaction whose only spends and outputs are Ironwood must not look empty.
///
/// `has_inputs_and_outputs` rejects a transaction with no inputs or no outputs, and it asks
/// these two methods. If they ignore the Ironwood pool, every Ironwood-only transaction is
/// rejected as `NoInputs`/`NoOutputs` — which is a consensus split, since the rest of the
/// network accepts them from NU6.3 onward.
#[test]
fn ironwood_only_transaction_has_shielded_inputs_and_outputs() {
    use crate::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

    let _init_guard = zebra_test::init();

    let ironwood = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu6_3,
        ::orchard::ValuePool::Ironwood,
        2,
        7,
    )
    .expect("the Ironwood pool is defined at NU6.3");

    let flags = *ironwood.flags();
    assert!(
        flags.spends_enabled() && flags.outputs_enabled(),
        "this test is only meaningful when the bundle enables both spends and outputs",
    );

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));

    // There is no transparent, Sprout, Sapling or Orchard data at all.
    assert!(tx.inputs().is_empty());
    assert!(tx.outputs().is_empty());
    assert!(tx.orchard_bundle().is_none());

    assert!(
        tx.has_shielded_inputs(),
        "Ironwood spends are shielded inputs"
    );
    assert!(
        tx.has_shielded_outputs(),
        "Ironwood outputs are shielded outputs"
    );
    assert!(tx.has_transparent_or_shielded_inputs());
    assert!(tx.has_transparent_or_shielded_outputs());
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
    let oversized_tx = Transaction::test_v1(
        inputs,
        vec![output],
        LockTime::Time(DateTime::from_timestamp(61, 0).unwrap()),
    );

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
/// zebra_consensus::transaction::check::has_inputs_and_outputs
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
/// zebra_consensus::transaction::check::has_inputs_and_outputs
#[test]
fn empty_v4_round_trip() {
    let _init_guard = zebra_test::init();

    let tx = Transaction::test_v4(
        Vec::new(),
        Vec::new(),
        LockTime::min_lock_time_timestamp(),
        block::Height(0),
    );

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
    // Transaction now wraps zcash_primitives::transaction::Transaction directly,
    // so serialization roundtrip via ZcashSerialize/ZcashDeserialize validates this.
    let _nu = tx.network_upgrade().expect("network upgrade");
    let bytes = tx
        .zcash_serialize_to_vec()
        .expect("empty V5 transaction serializes");
    let _tx2: Transaction = bytes
        .zcash_deserialize_into()
        .expect("empty V5 transaction deserializes");
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

            // Transaction is now a zcash_primitives newtype, so no explicit conversion needed.
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

        // Transaction is now a zcash_primitives newtype, so no explicit conversion needed.
    }

    Ok(())
}

#[test]
fn zip244_txid() -> Result<()> {
    let _init_guard = zebra_test::init();

    for test in zip0244::TEST_VECTORS.iter() {
        let tx: Transaction = test.tx.zcash_deserialize_into()?;
        let txid = tx.hash();
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

/// Real Orchard proofs from mined transactions must have the canonical size, and padding
/// a proof with trailing bytes must make it non-canonical (GHSA-jfw5-j458-pfv6). This
/// also cross-checks `expected_proof_size` against real proofs produced by the chain.
#[test]
fn orchard_proof_size_is_canonical() {
    let mut checked = 0;

    for net in Network::iter() {
        for tx in v5_transactions(net.block_iter()) {
            let Some(bundle) = tx.orchard_bundle() else {
                continue;
            };

            // A real, mined Orchard proof has the canonical length for its actions.
            assert!(
                tx.orchard_proof_size_is_canonical(),
                "a real Orchard proof should be canonically sized"
            );

            // Padding the proof with trailing data must break canonicity. The bundle is owned by
            // `zcash_primitives` and cannot be mutated in place, so this checks the same property
            // against the expected size directly.
            let proof_len = bundle.authorization().proof().as_ref().len();
            assert_ne!(
                proof_len + 1,
                crate::orchard::shielded_data::expected_proof_size(bundle.actions().len()),
                "a padded Orchard proof must not be considered canonical"
            );

            checked += 1;
        }
    }

    assert!(
        checked > 0,
        "expected at least one Orchard transaction in the test vectors"
    );
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

            PrecomputedTxData::new(&tx, tx_nu, Arc::new(Vec::new()))
                .expect("network upgrade is valid for tx");
            sighash::SigHasher::new(&tx, tx_nu, Arc::new(Vec::new()))
                .expect("network upgrade is valid for tx");
            tx.sighash(tx_nu, HashType::ALL, Arc::new(Vec::new()), None)
                .expect("network upgrade is valid for tx");

            // All computations should fail under an nu other than the tx one.

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
                // Compute bvk and verify binding sig if there's sapling data.
                if let Some(bundle) = tx.inner().sapling_bundle() {
                    let version = tx.version();

                    // V5+ sighashes include transparent output amounts, so skip txs with
                    // transparent inputs when we don't have the previous outputs handy.
                    //
                    // References:
                    //
                    // <https://zips.z.cash/zip-0244#s-2c-amounts-sig-digest>
                    // <https://zips.z.cash/zip-0244#s-2d-scriptpubkeys-sig-digest>
                    if version >= 5 && tx.has_transparent_inputs() {
                        continue;
                    }

                    let sighash = tx
                        .sighash(nu, HashType::ALL, Arc::new(Vec::new()), None)
                        .expect("network upgrade is valid for tx");

                    // Compute binding verification key from spend and output CVs.
                    let cv_spends: sapling_crypto::value::CommitmentSum =
                        bundle.shielded_spends().iter().map(|s| s.cv()).sum();
                    let cv_outputs: sapling_crypto::value::CommitmentSum =
                        bundle.shielded_outputs().iter().map(|o| o.cv()).sum();
                    let bvk = (cv_spends - cv_outputs).into_bvk(i64::from(bundle.value_balance()));

                    bvk.verify(sighash.as_ref(), &bundle.authorization().binding_sig)
                        .expect("must pass binding signature verification");

                    if version == 4 {
                        at_least_one_v4_checked = true;
                    } else {
                        at_least_one_v5_checked = true;
                    }
                }
            }
        }

        assert!(at_least_one_v4_checked);
        assert!(at_least_one_v5_checked);
    }
}

/// Check that a v6 (Ironwood / NU6.3) transaction computes a txid and round-trips through
/// serialization.
///
/// Computing the txid drives [`Transaction::to_librustzcash`] into the librustzcash Ironwood fork's
/// v6 (ZIP-244) digest path, which is the runtime path that does not work against released
/// librustzcash. The branch id resolves to the fork's `BranchId::Nu6_3`.
#[test]
fn v6_ironwood_txid_and_roundtrip() {
    let _init_guard = zebra_test::init();

    let tx = Transaction::test_v6(
        NetworkUpgrade::Nu6_3,
        Vec::new(),
        Vec::new(),
        LockTime::min_lock_time_timestamp(),
        block::Height(0),
    );

    // Drives the librustzcash Ironwood fork's v6 digest computation.
    let txid = tx.hash();

    // The v6 wire format round-trips through Zebra's own (de)serializer.
    let bytes = tx
        .zcash_serialize_to_vec()
        .expect("v6 transaction serializes");
    let tx2: Transaction = bytes
        .zcash_deserialize_into()
        .expect("v6 transaction deserializes");

    assert_eq!(tx, tx2);
    assert_eq!(tx2.hash(), txid, "txid is stable across serialization");
}

/// A v6 transaction carrying populated Orchard-v6 and Ironwood bundles round-trips through the
/// v6 (de)serializer, and each bundle lands back in its own slot.
#[test]
fn v6_transaction_with_bundles_round_trips() {
    use crate::transaction::arbitrary::shielded::{fake_orchard_bundle, fake_v6_transaction};
    use ::orchard::bundle::{BundleVersion, Flags};
    use zcash_protocol::value::ZatBalance;

    let _init_guard = zebra_test::init();
    let zero = ZatBalance::from_i64(0).expect("zero is a valid balance");

    let orchard = fake_orchard_bundle(
        Flags::CROSS_ADDRESS_DISABLED,
        zero,
        1,
        1,
        BundleVersion::orchard_v3(),
    );
    let ironwood = fake_orchard_bundle(
        Flags::CROSS_ADDRESS_DISABLED,
        zero,
        2,
        1000,
        BundleVersion::ironwood_v3(),
    );

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, Some(orchard), Some(ironwood));

    let bytes = tx
        .zcash_serialize_to_vec()
        .expect("v6 transaction serializes");
    let tx2: Transaction = bytes
        .zcash_deserialize_into()
        .expect("v6 transaction deserializes");

    assert_eq!(tx.hash(), tx2.hash());
    // The two Orchard-shaped slots must not be swapped by the round trip.
    assert_eq!(tx2.orchard_actions().count(), 1);
    assert_eq!(tx2.ironwood_actions().count(), 2);
}

/// The `enableCrossAddress` flag (bit 2) is permitted only for the Ironwood pool. A v6 Orchard
/// bundle carrying it MUST be rejected, while the same flag on the Ironwood bundle round-trips.
///
/// This is checked at both layers that enforce it:
///
/// * the bundle constructor, which refuses to build an Orchard-pool bundle whose flags are not
///   representable under `BundleVersion::orchard_v3`; and
/// * the wire codec, which is the guard that matters for untrusted input — without it a crafted
///   bundle would deserialize and then abort the node in the txid path.
#[test]
fn v6_orchard_bundle_rejects_cross_address_flag_on_the_wire() {
    use crate::transaction::arbitrary::shielded::{
        fake_orchard_bundle, fake_v6_transaction, v6_orchard_flags_offset,
    };
    use ::orchard::bundle::{Authorized, Bundle, BundleVersion, Flags};
    use zcash_protocol::value::ZatBalance;

    let _init_guard = zebra_test::init();
    let zero = ZatBalance::from_i64(0).expect("zero is a valid balance");

    // The Orchard pool cannot even construct a bundle with `enableCrossAddress` set: the flags
    // are not representable under `orchard_v3`.
    let orchard = fake_orchard_bundle(
        Flags::CROSS_ADDRESS_DISABLED,
        zero,
        1,
        1,
        BundleVersion::orchard_v3(),
    );
    assert!(
        Bundle::<Authorized, ZatBalance>::try_from_parts(
            orchard.actions().clone(),
            Flags::ENABLED,
            zero,
            *orchard.anchor(),
            orchard.authorization().clone(),
            BundleVersion::orchard_v3(),
        )
        .is_err(),
        "an Orchard-pool bundle with enableCrossAddress must not be constructible",
    );

    // The wire guard: flip bit 2 of `flagsOrchard` on a valid v6 transaction, and the result
    // must be rejected rather than parsed.
    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, Some(orchard), None);
    let mut bytes = tx
        .zcash_serialize_to_vec()
        .expect("v6 transaction serializes");

    let flags_offset = v6_orchard_flags_offset(1);
    assert_eq!(
        bytes[flags_offset],
        Flags::CROSS_ADDRESS_DISABLED
            .to_byte(BundleVersion::orchard_v3())
            .expect("the flags are representable"),
        "the computed offset must point at the Orchard flag byte",
    );

    bytes[flags_offset] |= 0b0000_0100;
    let result: Result<Transaction, _> = bytes.zcash_deserialize_into();
    assert!(
        result.is_err(),
        "a v6 Orchard bundle with enableCrossAddress must be rejected on the wire",
    );

    // The same flag on the Ironwood bundle is valid and round-trips.
    let ironwood = fake_orchard_bundle(Flags::ENABLED, zero, 1, 1000, BundleVersion::ironwood_v3());
    assert!(
        ironwood.flags().cross_address_enabled(),
        "the Ironwood pool permits enableCrossAddress",
    );

    let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, None, Some(ironwood));
    let bytes = tx
        .zcash_serialize_to_vec()
        .expect("v6 transaction serializes");
    let tx2: Transaction = bytes
        .zcash_deserialize_into()
        .expect("a v6 Ironwood bundle with enableCrossAddress round-trips");

    assert_eq!(tx.hash(), tx2.hash());
    assert!(
        tx2.ironwood_flags()
            .expect("the round-tripped transaction has an Ironwood bundle")
            .cross_address_enabled(),
        "enableCrossAddress must survive the round trip on the Ironwood bundle",
    );
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

/// Regression test for the Orchard `rk` identity-point DoS vulnerability.
///
/// A v5 transaction whose Orchard action has `rk = [0u8; 32]` (the Pallas
/// identity point) **deserializes successfully** — Zebra performs no
/// identity-point check in [`crate::orchard::Action::zcash_deserialize`].
///
/// When the same transaction is subsequently fed to the Orchard Halo2 batch
/// verifier via [`orchard::bundle::BatchValidator::add_bundle`], the call
/// chain reaches `orchard::circuit::to_halo2_instance()`, which calls
/// `.coordinates().unwrap()` on the identity point.  `coordinates()` returns
/// `None` for the identity, so the `unwrap` **panics**, crashing the node.
///
/// ## Root cause
///
/// `zebra-chain/src/orchard/action.rs:83` reads `rk` as raw bytes with no
/// identity-point check: `reader.read_32_bytes()?.into()`.  The upstream
/// `orchard` crate defers validation to signature verification, but
/// `to_halo2_instance()` unwraps the coordinate extraction unconditionally.
///
/// An analogous identity check already exists for `ephemeral_key`
/// (`zebra-chain/src/orchard/keys.rs:225-238`), demonstrating the correct
/// pattern.
#[test]
fn orchard_rk_identity_point() {
    use crate::transaction::arbitrary::shielded::{
        fake_bundle_for_branch, V5_FIRST_ACTION_RK_OFFSET,
    };

    let _init_guard = zebra_test::init();

    // A valid single-action v5 Orchard bundle, which we then corrupt on the wire.
    let branch_id = zcash_protocol::consensus::BranchId::Nu5;
    let bundle = fake_bundle_for_branch(branch_id, ::orchard::ValuePool::Orchard, 1, 7)
        .expect("the Orchard pool is defined at NU5");

    let expected_rk: [u8; 32] = bundle.actions().head.rk().into();

    let tx = Transaction::test_v5_with_orchard(
        NetworkUpgrade::Nu5,
        Vec::new(),
        Vec::new(),
        LockTime::unlocked(),
        block::Height(0),
        Some(bundle),
    );

    let mut tx_bytes = tx
        .zcash_serialize_to_vec()
        .expect("the crafted transaction must serialize");

    assert_eq!(
        &tx_bytes[V5_FIRST_ACTION_RK_OFFSET..V5_FIRST_ACTION_RK_OFFSET + 32],
        &expected_rk[..],
        "the computed offset must point at the first action's rk",
    );

    // Set rk to the identity point encoding. Deserialization must reject it, rather than
    // accepting it and later panicking when the coordinate is extracted.
    tx_bytes[V5_FIRST_ACTION_RK_OFFSET..V5_FIRST_ACTION_RK_OFFSET + 32].fill(0);

    Transaction::zcash_deserialize(&tx_bytes[..]).expect_err("rk = identity should fail");
}

/// Reproduction for GHSA-rgwx-8r98-p34c:
/// Coinbase Sapling spend vectors allocate before zero-spend consensus rule.
///
/// A V5 coinbase transaction with Sapling spends can be serialized and
/// deserialized — the parser allocates Sapling spend vectors (bounded by
/// `TrustedPreallocate::max_allocation()`) before any coinbase-specific
/// check. The consensus rule rejecting coinbase Sapling spends only runs
/// later in `zebra-consensus`, not during deserialization.
#[test]
fn coinbase_v5_with_sapling_spends_deserializes_successfully() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    // Find a real V4 transaction with Sapling spends from the test block vectors.
    let tx_with_spends =
        arbitrary::test_transactions(&network).find(|(_, tx)| tx.sapling_spends().count() > 0);

    let Some((height, original_tx)) = tx_with_spends else {
        panic!("test block vectors must contain at least one transaction with Sapling spends");
    };

    let original_spend_count = original_tx.sapling_spends().count();
    assert!(
        original_spend_count > 0,
        "source transaction must have Sapling spends"
    );

    // Convert the V4 transaction to a fake V5 — this preserves valid Sapling data.
    let fake_v5 = arbitrary::transaction_to_fake_v5(&original_tx, &network, height);

    // Confirm the fake V5 still has Sapling spends.
    assert!(
        fake_v5.sapling_spends().count() > 0,
        "converted V5 must retain Sapling shielded data with spends",
    );

    // Replace the transparent inputs with a single coinbase input. The rebuild preserves the
    // Sapling and Orchard bundles.
    let outputs = fake_v5.outputs();
    let outputs = if outputs.is_empty() {
        vec![transparent::Output {
            value: crate::amount::Amount::zero(),
            lock_script: Script::new(&[0u8; 20]),
        }]
    } else {
        outputs
    };

    let coinbase_tx = fake_v5
        .with_transparent_inputs(vec![transparent::Input::Coinbase {
            height,
            data: vec![0x00; 4],
            sequence: 0xFFFF_FFFF,
        }])
        .with_transparent_outputs(outputs);

    // The constructed transaction must look like a coinbase with Sapling spends.
    assert!(coinbase_tx.is_coinbase(), "transaction must be coinbase");
    assert!(
        coinbase_tx.sapling_spends().count() > 0,
        "coinbase transaction has Sapling spends"
    );

    // Serialize it.
    let serialized = coinbase_tx
        .zcash_serialize_to_vec()
        .expect("coinbase V5 with Sapling spends must serialize");

    // Deserialize it — the parser must now reject coinbase transactions with
    // Sapling spends before allocating spend vectors (GHSA-rgwx-8r98-p34c fix).
    let err = serialized
        .zcash_deserialize_into::<Transaction>()
        .expect_err("coinbase with Sapling spends must be rejected during deserialization");

    assert!(
        err.to_string()
            .contains("coinbase transaction must not have Sapling spends"),
        "unexpected error: {err}"
    );
}

/// Build a serialized transaction with one coinbase input whose scriptSig is a height push
/// followed by `data_len` bytes of miner data, then deserialize it through the production
/// parse path (`Transaction::zcash_deserialize`).
fn deserialize_coinbase_tx_with_data(
    height: u32,
    data_len: usize,
) -> Result<Transaction, SerializationError> {
    let tx = Transaction::test_v1(
        vec![transparent::Input::Coinbase {
            height: Height(height),
            data: vec![0x5a; data_len],
            sequence: 0xFFFF_FFFF,
        }],
        vec![transparent::Output {
            value: crate::amount::Amount::try_from(1).expect("valid amount"),
            lock_script: Script::new(&[]),
        }],
        LockTime::min_lock_time_timestamp(),
    );
    let serialized = tx
        .zcash_serialize_to_vec()
        .expect("coinbase transaction must serialize");
    serialized.zcash_deserialize_into::<Transaction>()
}

/// The coinbase scriptSig length must be in {2 .. 100} bytes on the production parse path.
/// The check lived in `Input::zcash_deserialize`, which the `zcash_primitives` parsing
/// refactor left without production callers.
#[test]
fn coinbase_script_len_bounds_enforced_at_parse() {
    let _init_guard = zebra_test::init();

    // Height 1 encodes as a single `OP_1` byte; height 500_000 as a 4-byte push.
    const ONE_BYTE_PUSH_HEIGHT: u32 = 1;
    const FOUR_BYTE_PUSH_HEIGHT: u32 = 500_000;

    // Undersized: a bare `OP_1` script is 1 byte, below the 2-byte minimum.
    assert!(
        matches!(
            deserialize_coinbase_tx_with_data(ONE_BYTE_PUSH_HEIGHT, 0),
            Err(SerializationError::Parse("Coinbase script is too short"))
        ),
        "1-byte coinbase script must be rejected"
    );

    // Oversized: a 4-byte height push + 97 bytes of data is 101 bytes, above the maximum.
    assert!(
        matches!(
            deserialize_coinbase_tx_with_data(FOUR_BYTE_PUSH_HEIGHT, 97),
            Err(SerializationError::Parse("Coinbase script is too long"))
        ),
        "101-byte coinbase script must be rejected"
    );

    // The boundary lengths 2 and 100 are accepted.
    deserialize_coinbase_tx_with_data(ONE_BYTE_PUSH_HEIGHT, 1)
        .expect("2-byte coinbase script must be accepted");
    deserialize_coinbase_tx_with_data(FOUR_BYTE_PUSH_HEIGHT, 96)
        .expect("100-byte coinbase script must be accepted");

    // The genesis coinbase script (77 bytes, no height prefix) still parses.
    Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
        .expect("genesis block must deserialize");
}

/// An out-of-range `nExpiryHeight` wire value must be readable through `raw_expiry_height`
/// and preserved by round-trip serialization, even though `expiry_height()` cannot
/// represent values above `Height::MAX` and maps them to `None`.
#[test]
fn raw_expiry_height_preserves_out_of_range_values() {
    let _init_guard = zebra_test::init();

    for raw in [0, 499_999_999, 500_000_000, 2_147_483_648, u32::MAX] {
        let tx = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            Vec::new(),
            Vec::new(),
            LockTime::min_lock_time_timestamp(),
            block::Height(raw),
        );

        assert_eq!(tx.raw_expiry_height(), Some(raw));
        if raw == 0 || raw > Height::MAX.0 {
            // `0` means "no expiry"; over-range values cannot be represented as a `Height`.
            assert_eq!(tx.expiry_height(), None);
        } else {
            assert_eq!(tx.expiry_height(), Some(Height(raw)));
        }

        // The wire value survives a serialization round trip unchanged.
        let serialized = tx
            .zcash_serialize_to_vec()
            .expect("transaction must serialize");
        let parsed = serialized
            .zcash_deserialize_into::<Transaction>()
            .expect("parsing does not enforce the expiry maximum, the verifier does");
        assert_eq!(parsed.raw_expiry_height(), Some(raw));
    }
}

/// A non-coinbase transaction with a null-prevout input alongside a regular input parses
/// through the production entry point, is not a coinbase, and is not a valid non-coinbase.
///
/// # Consensus
///
/// > A transparent input in a non-coinbase transaction MUST NOT have a null prevout.
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnconsensus>
#[test]
fn non_coinbase_with_null_prevout_input_is_not_valid_non_coinbase() {
    let mut raw = Vec::new();
    raw.extend_from_slice(&1_u32.to_le_bytes()); // version 1
    raw.push(2); // input count
                 // Input 0: null prevout with a valid height-1 coinbase script.
    raw.extend_from_slice(&[0; 32]);
    raw.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    raw.push(2);
    raw.extend_from_slice(&[0x51, 0x00]);
    raw.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    // Input 1: a regular spend.
    raw.extend_from_slice(&[1; 32]);
    raw.extend_from_slice(&0_u32.to_le_bytes());
    raw.push(0);
    raw.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    raw.push(1); // output count
    raw.extend_from_slice(&1_u64.to_le_bytes());
    raw.push(0);
    raw.extend_from_slice(&0_u32.to_le_bytes()); // lock time

    let tx: Transaction = raw
        .zcash_deserialize_into()
        .expect("a null-prevout input with a valid height script parses");

    assert!(
        matches!(tx.inputs()[0], crate::transparent::Input::Coinbase { .. }),
        "the null-prevout input is parsed as a coinbase input"
    );
    assert!(!tx.is_coinbase(), "two inputs is never a coinbase");
    assert!(
        !tx.is_valid_non_coinbase(),
        "a non-coinbase transaction with a null-prevout input is invalid"
    );

    // The regular shape of a non-coinbase transaction stays valid.
    let mut regular = raw.clone();
    regular[4] = 1; // input count
    regular.drain(5..5 + 32 + 4 + 1 + 2 + 4); // drop input 0
    let tx: Transaction = regular
        .zcash_deserialize_into()
        .expect("a single regular input parses");
    assert!(!tx.is_coinbase());
    assert!(tx.is_valid_non_coinbase());
}
