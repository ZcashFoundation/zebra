//! Tests for whether cited anchors are checked properly.

use std::sync::Arc;

use zebra_chain::{
    amount::Amount,
    block::Block,
    primitives::{ed25519, x25519, Groth16Proof},
    sapling,
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
    sprout::{self, JoinSplit},
    transaction::{JoinSplitData, Transaction, UnminedTx},
};

use crate::{
    arbitrary::Prepare,
    service::{
        check::anchors::tx_anchors_refer_to_final_treestates,
        write::validate_and_commit_non_finalized,
    },
    tests::setup::{new_state_with_mainnet_genesis, transaction_v4_from_coinbase},
    DiskWriteBatch, SemanticallyVerifiedBlock, ValidateContextError,
};

// Sprout

/// Check that, when primed with the first blocks that contain Sprout anchors, a
/// Sprout Spend's referenced anchor is validated.
#[test]
fn check_sprout_anchors() {
    let _init_guard = zebra_test::init();

    let (finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

    // Delete the empty anchor from the database
    let mut batch = DiskWriteBatch::new();
    batch.delete_sprout_anchor(
        &finalized_state,
        &sprout::tree::NoteCommitmentTree::default().root(),
    );
    finalized_state
        .write_batch(batch)
        .expect("unexpected I/O error");

    // Create a block at height 1.
    let block_1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Create a block just before the first Sprout anchors.
    let block_395 = zebra_test::vectors::BLOCK_MAINNET_395_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Add initial transactions to [`block_1`].
    let block_1 = prepare_sprout_block(block_1, block_395);

    // Create a block at height == 2 that references the Sprout note commitment tree state
    // from [`block_1`].
    let block_2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Exercise Sprout anchor checking with the first shielded transactions with
    // anchors.
    let block_396 = zebra_test::vectors::BLOCK_MAINNET_396_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // Add the transactions with the first anchors to [`block_2`].
    let block_2 = prepare_sprout_block(block_2, block_396);

    let unmined_txs: Vec<_> = block_2
        .block
        .transactions
        .iter()
        .map(UnminedTx::from)
        .collect();

    let check_unmined_tx_anchors_result = unmined_txs.iter().try_for_each(|unmined_tx| {
        tx_anchors_refer_to_final_treestates(
            &finalized_state.db,
            non_finalized_state.best_chain(),
            unmined_tx,
        )
    });

    assert!(
        matches!(
            check_unmined_tx_anchors_result,
            Err(ValidateContextError::UnknownSproutAnchor { .. }),
        ),
        "unexpected result: {check_unmined_tx_anchors_result:?}",
    );

    // Validate and commit [`block_1`]. This will add an anchor referencing the
    // empty note commitment tree to the state.
    assert!(validate_and_commit_non_finalized(
        &finalized_state.db,
        &mut non_finalized_state,
        block_1
    )
    .is_ok());

    let check_unmined_tx_anchors_result = unmined_txs.iter().try_for_each(|unmined_tx| {
        tx_anchors_refer_to_final_treestates(
            &finalized_state.db,
            non_finalized_state.best_chain(),
            unmined_tx,
        )
    });

    assert!(check_unmined_tx_anchors_result.is_ok());

    // Validate and commit [`block_2`]. This will also check the anchors.
    assert_eq!(
        validate_and_commit_non_finalized(&finalized_state.db, &mut non_finalized_state, block_2),
        Ok(())
    );
}

fn prepare_sprout_block(
    mut block_to_prepare: Block,
    reference_block: Block,
) -> SemanticallyVerifiedBlock {
    // Convert the coinbase transaction to a version that the non-finalized state will accept.
    block_to_prepare.transactions[0] =
        transaction_v4_from_coinbase(&block_to_prepare.transactions[0]).into();

    reference_block
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sprout_joinsplit_data())
        .for_each(|tx| {
            // Extract joinsplit descriptions from the V2 transaction using the new API.
            // These are known V2 transactions with BCTV14 proofs.
            let joinsplit_descs: Vec<_> = tx.sprout_joinsplit_descriptions().collect();
            let pub_key = tx
                .sprout_joinsplit_pub_key()
                .expect("V2 transactions with joinsplit data must have a pub key");

            // Build new JoinSplit<Groth16Proof> values from the JsDescriptions,
            // changing the proof to Groth16 and zeroing the value balance for semantic validation.
            let new_joinsplits: Vec<JoinSplit<Groth16Proof>> = joinsplit_descs
                .iter()
                .map(|js| {
                    let anchor = sprout::tree::Root::from(js.anchor());
                    let raw_nullifiers = js.nullifiers();
                    let nullifiers = [
                        sprout::note::Nullifier::from(raw_nullifiers[0]),
                        sprout::note::Nullifier::from(raw_nullifiers[1]),
                    ];
                    let raw_commitments = js.commitments();
                    let commitments = [
                        sprout::commitment::NoteCommitment::from(raw_commitments[0]),
                        sprout::commitment::NoteCommitment::from(raw_commitments[1]),
                    ];
                    let raw_seed = js.random_seed();
                    let random_seed = sprout::RandomSeed::from(*raw_seed);
                    let raw_macs = js.macs();
                    let vmacs = [
                        sprout::note::Mac::from(raw_macs[0]),
                        sprout::note::Mac::from(raw_macs[1]),
                    ];
                    JoinSplit {
                        vpub_old: Amount::zero(),
                        vpub_new: Amount::zero(),
                        anchor,
                        nullifiers,
                        commitments,
                        ephemeral_key: x25519::PublicKey::from([0u8; 32]),
                        random_seed,
                        vmacs,
                        zkproof: Groth16Proof::from([0; 192]),
                        enc_ciphertexts: [
                            sprout::note::EncryptedNote([0u8; 601]),
                            sprout::note::EncryptedNote([0u8; 601]),
                        ],
                    }
                })
                .collect();

            let joinsplit_data = new_joinsplits
                .split_first()
                .map(|(first, rest)| JoinSplitData {
                    first: first.clone(),
                    rest: rest.to_vec(),
                    pub_key,
                    sig: ed25519::Signature::from_bytes(&[0u8; 64]),
                });

            // Build a V4 transaction with the adjusted joinsplit data.
            let new_tx = build_v4_tx_with_joinsplit_data(joinsplit_data);

            // Add the new adjusted transaction to [`block_to_prepare`].
            block_to_prepare.transactions.push(Arc::new(new_tx));
        });

    Arc::new(block_to_prepare).prepare()
}

/// Build a V4 (Sapling) transaction containing the given sprout joinsplit data.
/// Transparent inputs/outputs and sapling shielded data are empty.
/// The transaction bytes are constructed manually and deserialized into a `Transaction`.
fn build_v4_tx_with_joinsplit_data(
    joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
) -> Transaction {
    let mut bytes: Vec<u8> = Vec::new();

    // V4 overwintered header: version=4, overwintered flag set (= 0x80000004 LE)
    bytes.extend_from_slice(&0x8000_0004u32.to_le_bytes());
    // versionGroupId = SAPLING_VERSION_GROUP_ID = 0x892F2085 LE
    bytes.extend_from_slice(&0x892F_2085u32.to_le_bytes());
    // nTransparentInputs = 0 (compact_size)
    bytes.push(0x00);
    // nTransparentOutputs = 0 (compact_size)
    bytes.push(0x00);
    // nLockTime = min_lock_time_timestamp (500_000_000 = 0x1DCD6500 LE)
    bytes.extend_from_slice(&500_000_000u32.to_le_bytes());
    // nExpiryHeight = 0
    bytes.extend_from_slice(&0u32.to_le_bytes());
    // valueBalanceSapling = 0 (i64 LE)
    bytes.extend_from_slice(&0i64.to_le_bytes());
    // nSpendsSapling = 0 (compact_size)
    bytes.push(0x00);
    // nOutputsSapling = 0 (compact_size)
    bytes.push(0x00);

    // Joinsplit data
    if let Some(ref jsd) = joinsplit_data {
        jsd.zcash_serialize(&mut bytes)
            .expect("joinsplit_data serialization should succeed");
    } else {
        // nJoinSplit = 0 (compact_size)
        bytes.push(0x00);
    }

    Transaction::zcash_deserialize(bytes.as_slice())
        .expect("manually constructed V4 transaction should deserialize")
}

/// Build a V4 transaction with the same sapling shielded data as `tx`,
/// but with `valueBalanceSapling` set to zero (to pass chain value pool checks).
///
/// Serializes `tx`, patches the valueBalanceSapling field to 0, then deserializes.
fn build_v4_tx_with_sapling_data_zero_balance(tx: &Transaction) -> Transaction {
    // Serialize the transaction to bytes.
    let mut tx_bytes = tx
        .zcash_serialize_to_vec()
        .expect("transaction serialization should succeed");

    // Parse past the V4 header (4 bytes nVersion + 4 bytes nVersionGroupId).
    let mut pos = 8usize;

    // Parse and skip transparent inputs.
    let n_inputs = parse_compact_size_bytes(&tx_bytes, &mut pos);
    for _ in 0..n_inputs {
        // Each TxIn: outpoint (36 bytes) + scriptSig (compact_size + bytes) + sequence (4 bytes)
        pos += 36; // outpoint
        let script_len = parse_compact_size_bytes(&tx_bytes, &mut pos);
        pos += script_len as usize + 4; // script + sequence
    }

    // Parse and skip transparent outputs.
    let n_outputs = parse_compact_size_bytes(&tx_bytes, &mut pos);
    for _ in 0..n_outputs {
        // Each TxOut: value (8 bytes) + scriptPubKey (compact_size + bytes)
        pos += 8; // value
        let script_len = parse_compact_size_bytes(&tx_bytes, &mut pos);
        pos += script_len as usize; // scriptPubKey
    }

    // Skip nLockTime (4 bytes) and nExpiryHeight (4 bytes).
    pos += 8;

    // Now `pos` points to valueBalanceSapling (8 bytes i64 LE). Set to 0.
    assert!(
        pos + 8 <= tx_bytes.len(),
        "transaction too short to contain valueBalanceSapling"
    );
    tx_bytes[pos..pos + 8].copy_from_slice(&0i64.to_le_bytes());

    let tx = Transaction::zcash_deserialize(tx_bytes.as_slice())
        .expect("patched V4 transaction should deserialize");

    // Remove transparent inputs/outputs that reference UTXOs not in the test state.
    // This test only cares about sapling anchors, not transparent validation.
    tx.with_transparent_inputs(vec![])
        .with_transparent_outputs(vec![])
}

/// Parse a CompactSize integer from `bytes` at the given `pos`, advancing `pos`.
fn parse_compact_size_bytes(bytes: &[u8], pos: &mut usize) -> u64 {
    let first = bytes[*pos];
    *pos += 1;
    match first {
        0xfd => {
            let val = u16::from_le_bytes([bytes[*pos], bytes[*pos + 1]]);
            *pos += 2;
            val as u64
        }
        0xfe => {
            let val = u32::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
            ]);
            *pos += 4;
            val as u64
        }
        0xff => {
            let val = u64::from_le_bytes([
                bytes[*pos],
                bytes[*pos + 1],
                bytes[*pos + 2],
                bytes[*pos + 3],
                bytes[*pos + 4],
                bytes[*pos + 5],
                bytes[*pos + 6],
                bytes[*pos + 7],
            ]);
            *pos += 8;
            val
        }
        n => n as u64,
    }
}

// Sapling

/// Check that, when primed with the first Sapling blocks, a Sapling Spend's referenced anchor is
/// validated.
#[test]
fn check_sapling_anchors() {
    let _init_guard = zebra_test::init();

    let (finalized_state, mut non_finalized_state, _genesis) = new_state_with_mainnet_genesis();

    // Delete the empty anchor from the database
    let mut batch = DiskWriteBatch::new();
    batch.delete_sapling_anchor(
        &finalized_state,
        &sapling::tree::NoteCommitmentTree::default().root(),
    );
    finalized_state
        .write_batch(batch)
        .expect("unexpected I/O error");

    // Create a block at height 1 that has the first Sapling note commitments
    let mut block1 = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // convert the coinbase transaction to a version that the non-finalized state will accept
    block1.transactions[0] = transaction_v4_from_coinbase(&block1.transactions[0]).into();

    // Prime finalized state with the Sapling start + 1, which has the first
    // Sapling note commitment
    let block_419201 = zebra_test::vectors::BLOCK_MAINNET_419201_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    block_419201
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sapling_shielded_data())
        .for_each(|tx| {
            // Build a V4 transaction with zeroed value balance by serializing and patching bytes.
            // The transaction must have value_balance = 0 to pass chain value pool checks.
            let new_tx = build_v4_tx_with_sapling_data_zero_balance(tx.as_ref());
            block1.transactions.push(Arc::new(new_tx));
        });

    let block1 = Arc::new(block1).prepare();

    // Create a block at height == 2 that references the Sapling note commitment tree state
    // from earlier block
    let mut block2 = zebra_test::vectors::BLOCK_MAINNET_2_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    // convert the coinbase transaction to a version that the non-finalized state will accept
    block2.transactions[0] = transaction_v4_from_coinbase(&block2.transactions[0]).into();

    // Exercise Sapling anchor checking with Sapling start + 2, which refers to the note commitment
    // tree as of the last transaction of the previous block
    let block_419202 = zebra_test::vectors::BLOCK_MAINNET_419202_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    block_419202
        .transactions
        .into_iter()
        .filter(|tx| tx.has_sapling_shielded_data())
        .for_each(|tx| {
            // Build a V4 transaction with zeroed value balance by serializing and patching bytes.
            // The transaction must have value_balance = 0 to pass chain value pool checks.
            let new_tx = build_v4_tx_with_sapling_data_zero_balance(tx.as_ref());
            block2.transactions.push(Arc::new(new_tx));
        });

    let block2 = Arc::new(block2).prepare();

    let unmined_txs: Vec<_> = block2
        .block
        .transactions
        .iter()
        .map(UnminedTx::from)
        .collect();

    let check_unmined_tx_anchors_result = unmined_txs.iter().try_for_each(|unmined_tx| {
        tx_anchors_refer_to_final_treestates(
            &finalized_state.db,
            non_finalized_state.best_chain(),
            unmined_tx,
        )
    });

    assert!(matches!(
        check_unmined_tx_anchors_result,
        Err(ValidateContextError::UnknownSaplingAnchor { .. })
    ));

    assert!(validate_and_commit_non_finalized(
        &finalized_state.db,
        &mut non_finalized_state,
        block1
    )
    .is_ok());

    let check_unmined_tx_anchors_result = unmined_txs.iter().try_for_each(|unmined_tx| {
        tx_anchors_refer_to_final_treestates(
            &finalized_state.db,
            non_finalized_state.best_chain(),
            unmined_tx,
        )
    });

    assert!(check_unmined_tx_anchors_result.is_ok());

    assert_eq!(
        validate_and_commit_non_finalized(&finalized_state.db, &mut non_finalized_state, block2),
        Ok(())
    );
}

// TODO: create a test for orchard anchors
