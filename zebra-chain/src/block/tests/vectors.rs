use std::{
    collections::{HashMap, HashSet},
    io::{Cursor, Write},
    sync::Arc,
};

use chrono::{DateTime, Duration, LocalResult, TimeZone, Utc};

use crate::{
    amount::{Amount, DeferredPoolBalanceChange, NonNegative, MAX_MONEY},
    block::{
        serialize::MAX_BLOCK_BYTES, Block, BlockTimeError, Commitment::*, Hash, Header, Height,
    },
    parameters::{Network, NetworkUpgrade::*},
    sapling,
    serialization::{
        sha256d, SerializationError, ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize,
    },
    transaction::{LockTime, Transaction},
    transparent,
    value_balance::ValueBalance,
};

use super::generate; // TODO: this should be rewritten as strategies

/// Independent Bitcoin-style reduction: SHA256d, duplicating the last node on odd levels.
fn serial_transaction_merkle_root(leaves: &[[u8; 32]]) -> Option<[u8; 32]> {
    let mut level = leaves.to_vec();
    while level.len() > 1 {
        if !level.len().is_multiple_of(2) {
            level.push(*level.last().unwrap());
        }
        let mut parents = Vec::new();
        for pair in level.chunks_exact(2) {
            let mut writer = sha256d::Writer::default();
            writer.write_all(&pair[0]).unwrap();
            writer.write_all(&pair[1]).unwrap();
            parents.push(writer.finish());
        }
        level = parents;
    }
    level.first().copied()
}

/// Independent ZIP-244 reduction: zero-pad once, then hash with the auth-data personalization.
fn serial_auth_data_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    let mut level = leaves.to_vec();
    level.resize(level.len().next_power_of_two(), [0; 32]);
    while level.len() > 1 {
        let mut parents = Vec::new();
        for pair in level.chunks_exact(2) {
            let digest = blake2b_simd::Params::new()
                .hash_length(32)
                .personal(b"ZcashAuthDatHash")
                .to_state()
                .update(&pair[0])
                .update(&pair[1])
                .finalize();
            parents.push(digest.as_bytes().try_into().unwrap());
        }
        level = parents;
    }
    level[0]
}

/// Compare values, transaction IDs, exact errors, and reader position (also on failures).
fn assert_parallel_decode_matches_serial(bytes: &[u8]) {
    let mut serial_bytes = bytes;
    let mut parallel_bytes = bytes;
    let serial = Block::zcash_deserialize(&mut serial_bytes);
    let parallel = Block::zcash_deserialize_parallel(&mut parallel_bytes);
    assert_eq!(serial_bytes, parallel_bytes, "consumed bytes differ");
    match (serial, parallel) {
        (Ok(serial), Ok(parallel)) => {
            assert_eq!(serial, parallel);
            assert_eq!(
                serial.zcash_serialize_to_vec().unwrap(),
                parallel.zcash_serialize_to_vec().unwrap()
            );
            for (serial, parallel) in serial.transactions.iter().zip(&parallel.transactions) {
                assert_eq!(serial.hash(), parallel.hash());
                assert_eq!(serial.auth_digest(), parallel.auth_digest());
            }
        }
        (Err(serial), Err(parallel)) => {
            assert_eq!(format!("{serial:?}"), format!("{parallel:?}"));
        }
        (serial, parallel) => panic!("decode results differ: {serial:?} / {parallel:?}"),
    }
}

#[test]
fn parallel_decode_vectors() {
    use crate::block::serialize::{parallel_block, scan_transaction};

    let _init_guard = zebra_test::init();
    for bytes in zebra_test::vectors::BLOCKS.iter() {
        assert_parallel_decode_matches_serial(bytes);
        let block = Block::zcash_deserialize(*bytes).unwrap();
        if block.transactions.len() >= 64 {
            assert!(parallel_block(bytes).is_some(), "large vector uses Rayon");
        }
        for transaction in &block.transactions {
            let mut encoded = transaction.zcash_serialize_to_vec().unwrap();
            let end = encoded.len();
            encoded.extend_from_slice(&[0xaa, 0xbb]);
            let mut remaining = encoded.as_slice();
            assert_eq!(scan_transaction(&mut remaining), Some(()));
            assert_eq!(remaining, &encoded[end..], "scanner transaction boundary");
        }

        let mut trailing = bytes.to_vec();
        trailing.extend_from_slice(&[0xaa, 0xbb]);
        assert_parallel_decode_matches_serial(&trailing);
        for end in [0, 1, 3, 4, 100, bytes.len() / 2, bytes.len() - 1] {
            assert_parallel_decode_matches_serial(&bytes[..end.min(bytes.len())]);
        }
    }
}

#[test]
fn parallel_decode_zip244_and_v6_pools() {
    use crate::{
        block::serialize::parallel_block, transaction::arbitrary::shielded::fake_bundle_for_branch,
    };
    use ::orchard::ValuePool;
    use zcash_protocol::consensus::BranchId;

    let _init_guard = zebra_test::init();
    let mut transactions: Vec<_> = zebra_test::zip0244::TEST_VECTORS
        .iter()
        .map(|vector| Transaction::zcash_deserialize(vector.tx.as_slice()).unwrap())
        .collect();
    let sapling = transactions
        .iter()
        .find_map(|tx| tx.sapling_bundle().cloned())
        .unwrap();
    for pools in 0..8 {
        let transaction = zcash_primitives::transaction::TransactionData::from_parts_v6(
            BranchId::Nu6_3,
            0,
            0.into(),
            None,
            (pools & 1 != 0).then(|| sapling.clone()),
            (pools & 2 != 0).then(|| {
                fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Orchard, 2, 1).unwrap()
            }),
            (pools & 4 != 0).then(|| {
                fake_bundle_for_branch(BranchId::Nu6_3, ValuePool::Ironwood, 2, 2).unwrap()
            }),
        )
        .freeze()
        .unwrap();
        let mut encoded = Vec::new();
        transaction.write(&mut encoded).unwrap();
        transactions.push(Transaction::zcash_deserialize(encoded.as_slice()).unwrap());
    }
    for transaction in transactions {
        let transaction = Arc::new(transaction);
        let block = Block {
            header: Arc::new(generate::block_header().0),
            transactions: vec![transaction; 64],
        };
        let bytes = block.zcash_serialize_to_vec().unwrap();
        assert!(
            parallel_block(&bytes).is_some(),
            "shielded block uses Rayon"
        );
        assert_parallel_decode_matches_serial(&bytes);
        for end in [bytes.len() / 2, bytes.len() - 1] {
            assert_parallel_decode_matches_serial(&bytes[..end]);
        }
    }
}

#[test]
fn parallel_decode_threshold_and_malformed() {
    use crate::block::serialize::parallel_block;

    let _init_guard = zebra_test::init();
    for count in [0, 1, 63, 64, 65, 9_000, 9_500] {
        let bytes = generate::many_transaction_v4_block(count);
        assert_parallel_decode_matches_serial(&bytes);
        assert_eq!(
            parallel_block(&bytes).is_some(),
            count >= 64,
            "parallel entry threshold for {count} transactions"
        );
    }

    let bytes = generate::many_transaction_v4_block(65);
    let mut regtest = Block::zcash_deserialize(bytes.as_slice()).unwrap();
    Arc::make_mut(&mut regtest.header).solution = crate::work::equihash::Solution::from_bytes(
        &[0; crate::work::equihash::REGTEST_SOLUTION_SIZE],
    )
    .unwrap();
    let regtest = regtest.zcash_serialize_to_vec().unwrap();
    assert!(
        parallel_block(&regtest).is_some(),
        "Regtest header boundary"
    );
    assert_parallel_decode_matches_serial(&regtest);
    let (_, header) = generate::block_header();
    let header_end = header.len();
    let transaction_start = header_end + 1;
    // Transaction count: non-canonical, over TrustedPreallocate, over message limit.
    for count in [
        vec![0xfd, 1, 0],
        vec![0xfe, 0xff, 0xff, 0, 0],
        vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    ] {
        let mut malformed = header.clone();
        malformed.extend_from_slice(&count);
        malformed.extend_from_slice(&bytes[transaction_start..]);
        assert_parallel_decode_matches_serial(&malformed);
    }

    for end in (0..bytes.len()).step_by(53).chain([bytes.len() - 1]) {
        assert_parallel_decode_matches_serial(&bytes[..end]);
    }
    // Exercise invalid fields at both ends, not just scan failures. This includes
    // version/group IDs, coinbase outpoints, counts, values and the Sapling balance.
    for transaction in [0, 64] {
        for offset in 0..211 {
            let mut malformed = bytes.clone();
            malformed[transaction_start + transaction * 211 + offset] ^= 0xff;
            assert_parallel_decode_matches_serial(&malformed);
        }
    }
    // Oversized input, output and script counts, preserving the remaining suffix.
    for offset in [8, 45, 157, 166] {
        let mut malformed = bytes[..transaction_start + offset].to_vec();
        malformed.extend_from_slice(&[0xff; 9]);
        malformed.extend_from_slice(&bytes[transaction_start + offset + 1..]);
        assert_parallel_decode_matches_serial(&malformed);
    }
}

#[test]
fn parallel_decode_bounds_speculation_and_keeps_first_error() {
    use crate::block::serialize::{parallel_block, scan_batch, PARALLEL_DECODE_BATCH_BYTES};

    let _init_guard = zebra_test::init();
    let limit = PARALLEL_DECODE_BATCH_BYTES;
    let bytes = generate::many_transaction_v4_block(9_000);
    let start = generate::block_header().1.len() + 3;
    let mut ranges = Vec::new();
    for budget in [1, 211, 64 * 211, limit] {
        assert_eq!(
            scan_batch(&bytes[start..], 9_000, budget, &mut ranges),
            Some(())
        );
        assert_eq!(ranges.len(), (budget / 211).max(1));
        assert!(ranges.len() == 1 || ranges.last().unwrap().end <= budget);
    }
    // Once a range is retained, scanning cannot read past the remaining budget.
    // A truncated second transaction must not discard the first range.
    assert_eq!(
        scan_batch(&bytes[start..start + 422 - 1], 2, 422, &mut ranges),
        Some(())
    );
    assert_eq!(ranges, vec![0..211]);
    // The next transaction is complete on the wire, but not within this budget.
    assert_eq!(
        scan_batch(&bytes[start..], 9_000, 300, &mut ranges),
        Some(())
    );
    assert_eq!(ranges, vec![0..211]);
    // Bad valueBalanceSapling before, at, and after a batch boundary, and at the end.
    for index in [0, 63, 64, 127, 128, limit / 211, 8_999] {
        let mut malformed = bytes.clone();
        malformed[start + index * 211 + 200] = 1;
        let (result, _) = parallel_block(&malformed).unwrap();
        assert!(matches!(
            result,
            Err(SerializationError::BadTransactionBalance)
        ));
        assert_parallel_decode_matches_serial(&malformed);
    }
    // A later scan failure must not replace an earlier canonical field error.
    let mut malformed = bytes;
    malformed[start + 64 * 211 + 200] = 1;
    malformed.truncate(start + 66 * 211 - 1);
    assert_parallel_decode_matches_serial(&malformed);
}

/// Both reducers must preserve serial semantics across padding, chunk, and scheduling boundaries.
#[test]
fn parallel_merkle_roots_match_serial_reference() {
    use crate::{
        block::merkle::{AuthDataRoot, Root, PARALLEL_MERKLE_THRESHOLD},
        transaction::{AuthDigest, Hash as TransactionHash},
    };

    let _init_guard = zebra_test::init();
    let cutoff = PARALLEL_MERKLE_THRESHOLD;
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    let leaves: Vec<[u8; 32]> = (0..20_000)
        .map(|_| {
            let mut leaf = [0; 32];
            for bytes in leaf.chunks_exact_mut(8) {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                bytes.copy_from_slice(&state.to_le_bytes());
            }
            leaf
        })
        .collect();

    for leaf_count in [
        0,
        1,
        2,
        3,
        127,
        128,
        129,
        255,
        256,
        257,
        511,
        512,
        513,
        cutoff - 1,
        cutoff,
        cutoff + 1,
        2_047,
        2_048,
        2_049,
        4_095,
        4_096,
        4_097,
        8_191,
        8_192,
        8_193,
        9_194,
        16_383,
        16_384,
        16_385,
        19_999,
        20_000,
    ] {
        let leaves = &leaves[..leaf_count];
        let actual_auth = leaves
            .iter()
            .copied()
            .map(AuthDigest)
            .collect::<AuthDataRoot>();
        assert_eq!(
            actual_auth,
            AuthDataRoot::from(serial_auth_data_root(leaves)),
            "auth-data root for {leaf_count} leaves"
        );

        let transaction_root = || {
            leaves
                .iter()
                .copied()
                .map(TransactionHash)
                .collect::<Root>()
        };
        match serial_transaction_merkle_root(leaves) {
            Some(expected) => assert_eq!(
                transaction_root(),
                Root::from(expected),
                "transaction root for {leaf_count} leaves"
            ),
            None => assert!(
                std::panic::catch_unwind(transaction_root).is_err(),
                "an empty transaction tree must still panic"
            ),
        }
    }
}

/// Parallel reduction must preserve independent serial roots and block headers.
#[test]
fn parallel_merkle_roots_match_serial_vectors() {
    use crate::{
        block::merkle::{AuthDataRoot, Root},
        transaction::AuthDigest,
    };

    let _init_guard = zebra_test::init();

    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = Block::zcash_deserialize(*block_bytes).unwrap();
        let auth_digests: Vec<_> = block
            .transactions
            .iter()
            .map(|tx| tx.auth_digest().unwrap_or(AuthDigest([0xFF; 32])).0)
            .collect();
        let transaction_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash().0).collect();

        assert_eq!(
            block.auth_data_root(),
            AuthDataRoot::from(serial_auth_data_root(&auth_digests)),
            "auth data root at {:?}",
            block.coinbase_height()
        );
        assert_eq!(
            block.header.merkle_root,
            Root::from(serial_transaction_merkle_root(&transaction_hashes).unwrap()),
            "header transaction root at {:?}",
            block.coinbase_height()
        );
        assert_eq!(
            block.transactions.iter().collect::<Root>(),
            block.header.merkle_root,
            "parallel transaction root at {:?}",
            block.coinbase_height()
        );
    }
}

/// Cover empty, legacy, mixed-version, and padded trees.
#[test]
fn parallel_merkle_roots_match_serial_edge_cases() {
    use crate::{block::merkle::AuthDataRoot, transaction::AuthDigest};

    let _init_guard = zebra_test::init();
    let legacy =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES).unwrap();
    let mut mixed =
        Block::zcash_deserialize(&**zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES).unwrap();

    assert!(legacy
        .transactions
        .iter()
        .all(|tx| tx.auth_digest().is_none()));
    assert!(mixed
        .transactions
        .iter()
        .any(|tx| tx.auth_digest().is_some()));
    mixed.transactions.insert(1, legacy.transactions[0].clone());

    for source in [legacy, mixed] {
        for tx_count in [0, 1, 2, 3, 7, 15, 16, 17, 32, 257] {
            let block = Block {
                header: source.header.clone(),
                transactions: source
                    .transactions
                    .iter()
                    .cycle()
                    .take(tx_count)
                    .cloned()
                    .collect(),
            };
            let digests: Vec<_> = block
                .transactions
                .iter()
                .map(|tx| tx.auth_digest().unwrap_or(AuthDigest([0xFF; 32])).0)
                .collect();
            let serial = AuthDataRoot::from(serial_auth_data_root(&digests));
            let actual = block.auth_data_root();

            assert_eq!(actual, serial, "auth data root for {tx_count} transactions");
            if tx_count == 0 {
                assert_eq!(actual, AuthDataRoot::from([0; 32]));
            }
        }
    }
}

#[test]
fn blockheaderhash_debug() {
    let _init_guard = zebra_test::init();

    let preimage = b"foo bar baz";
    let mut sha_writer = sha256d::Writer::default();
    let _ = sha_writer.write_all(preimage);

    let hash = Hash(sha_writer.finish());

    assert_eq!(
        format!("{hash:?}"),
        "block::Hash(\"3166411bd5343e0b284a108f39a929fbbb62619784f8c6dafe520703b5b446bf\")"
    );
}

#[test]
fn blockheaderhash_from_blockheader() {
    let _init_guard = zebra_test::init();

    let (blockheader, _blockheader_bytes) = generate::block_header();

    let hash = Hash::from(&blockheader);

    assert_eq!(
        format!("{hash:?}"),
        "block::Hash(\"d1d6974bbe1d4d127c889119b2fc05724c67588dc72708839727586b8c2bc939\")"
    );

    let mut bytes = Cursor::new(Vec::new());

    blockheader
        .zcash_serialize(&mut bytes)
        .expect("these bytes to serialize from a blockheader without issue");

    bytes.set_position(0);
    let other_header = bytes
        .zcash_deserialize_into()
        .expect("these bytes to deserialize into a blockheader without issue");

    assert_eq!(blockheader, other_header);
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10585.
///
/// `Block::chain_value_pool_change()` previously aggregated transaction value
/// balances with `flat_map(|tx| tx.value_balance(utxos))`. Because
/// `Result<T, E>` implements `IntoIterator`, `Err(_)` yielded zero items and
/// silently dropped the failing transaction from the block sum. The block
/// helper now uses `try_fold` so transaction-level value-balance errors
/// propagate.
#[test]
fn chain_value_pool_change_propagates_transaction_value_balance_errors() {
    let _init_guard = zebra_test::init();

    let max_money: Amount<NonNegative> = MAX_MONEY.try_into().expect("MAX_MONEY is a valid amount");
    // Two `MAX_MONEY` transparent outputs make the transaction-level output
    // sum exceed `MAX_MONEY`, so `value_balance` returns `Err`.
    let coinbase = Transaction::test_v1(
        vec![transparent::Input::Coinbase {
            height: Height(1),
            // 1 byte of data keeps the scriptSig at the 2-byte consensus minimum.
            data: vec![0],
            sequence: 0xFFFF_FFFF,
        }],
        vec![
            transparent::Output::new(max_money, transparent::Script::new(&[])),
            transparent::Output::new(max_money, transparent::Script::new(&[])),
        ],
        LockTime::unlocked(),
    );
    let utxos = HashMap::<_, transparent::Utxo>::new();

    assert!(
        coinbase.value_balance(&utxos).is_err(),
        "transaction-level value balance should reject an output sum above MAX_MONEY"
    );

    let header: Header = zebra_test::vectors::DUMMY_HEADER
        .zcash_deserialize_into()
        .expect("dummy header should deserialize");
    let block = Block {
        header: Arc::new(header),
        transactions: vec![Arc::new(coinbase)],
    };

    assert!(
        block
            .chain_value_pool_change(
                &utxos,
                DeferredPoolBalanceChange::zero(),
                &Network::Mainnet,
                ValueBalance::zero()
            )
            .is_err(),
        "block-level aggregation should propagate transaction value-balance errors"
    );
}

/// Unspendable genesis outputs are not issued supply, even on a custom monetary schedule.
#[test]
fn genesis_transparent_outputs_are_not_issued_supply() {
    use crate::parameters::testnet::{ConfiguredActivationHeights, Parameters};

    let _init_guard = zebra_test::init();
    for bytes in [
        &*zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES,
        &*zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES,
    ] {
        let genesis: Block = bytes.zcash_deserialize_into().unwrap();
        assert!(genesis.transactions[0]
            .outputs()
            .iter()
            .all(|output| output.value.is_zero()));
    }

    let network = Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        })
        .unwrap()
        .clear_funding_streams()
        .with_lockbox_disbursements(vec![])
        .to_network()
        .unwrap();
    let genesis: Block = zebra_test::vectors::BLOCK_TESTNET_GENESIS_BYTES
        .zcash_deserialize_into()
        .unwrap();
    for value in [0, 1, MAX_MONEY] {
        let mut genesis = genesis.clone();
        let transaction = Arc::make_mut(&mut genesis.transactions[0]);
        let mut outputs = transaction.outputs();
        outputs[0].value = value.try_into().unwrap();
        transaction.set_outputs(outputs);
        for network in [&network, &Network::Mainnet] {
            let change = genesis
                .chain_value_pool_change(
                    &HashMap::<_, transparent::Utxo>::new(),
                    DeferredPoolBalanceChange::zero(),
                    network,
                    ValueBalance::zero(),
                )
                .unwrap();
            let pools = ValueBalance::zero()
                .add_chain_value_pool_change(change)
                .unwrap();
            assert!(pools.transparent_amount().is_zero());
            assert!(pools.nsm_amount().is_zero());
            assert!(pools.total().unwrap().is_zero());
        }
    }
}

/// From NU7, 60% of a block's transaction fees accrue to the NSM reserve instead of being
/// claimed by the miner, so they appear in the block's chain value pool change.
#[test]
fn chain_value_pool_change_accrues_the_nsm_reserve() {
    use crate::{
        parameters::testnet::ConfiguredActivationHeights, transaction::Hash as TransactionHash,
        transparent::OutPoint,
    };

    let _init_guard = zebra_test::init();

    let height = Height(1_000);
    let spent_value: Amount<NonNegative> = 100_000.try_into().expect("valid amount");
    let change: Amount<NonNegative> = 90_000.try_into().expect("valid amount");
    // The one non-coinbase transaction below pays a fee of 10,000 zatoshi.

    let coinbase = Transaction::test_v1(
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: 0xFFFF_FFFF,
        }],
        vec![transparent::Output::new(
            change,
            transparent::Script::new(&[]),
        )],
        LockTime::unlocked(),
    );

    let outpoint = OutPoint::from_usize(TransactionHash([0; 32]), 0);
    let spend = Transaction::test_v1(
        vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[]),
            sequence: 0,
        }],
        vec![transparent::Output::new(
            change,
            transparent::Script::new(&[]),
        )],
        LockTime::unlocked(),
    );

    let utxos = HashMap::from([(
        outpoint,
        transparent::Utxo::new(
            transparent::Output::new(spent_value, transparent::Script::new(&[])),
            Height(999),
            false,
        ),
    )]);

    let header: Header = zebra_test::vectors::DUMMY_HEADER
        .zcash_deserialize_into()
        .expect("dummy header should deserialize");
    let block = Block {
        header: Arc::new(header),
        transactions: vec![Arc::new(coinbase), Arc::new(spend)],
    };

    for (nu7, expected_reserve) in [(None, 0), (Some(height.0), 6_000), (Some(999), 6_000)] {
        let network = Network::new_regtest(
            ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(2),
                nu6: Some(3),
                nu6_1: Some(4),
                nu6_2: Some(5),
                nu6_3: Some(6),
                nu7,
                ..Default::default()
            }
            .into(),
        );
        let (change, fees) = block
            .chain_value_pool_change_and_fees(
                &utxos,
                DeferredPoolBalanceChange::zero(),
                &network,
                ValueBalance::zero(),
            )
            .expect("chain value pool change should be calculable");
        assert_eq!(change.nsm_amount().zatoshis(), expected_reserve);
        assert_eq!(fees.map(|fees| fees.zatoshis()), nu7.map(|_| 10_000));
    }
}

/// Seed, reissue, fund and roll back using the same contextual accounting used by state commits.
#[test]
fn nsm_seed_reissuance_and_funding_follow_the_parent() -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        amount::NegativeAllowed,
        parameters::{
            subsidy::{
                block_subsidy_with_parent_pools, funding_stream_address, funding_stream_values,
                scheduled_block_subsidy, subsidy_is_valid, CoinbaseTransactionError,
                FundingStreamReceiver, SubsidyError,
            },
            testnet::{
                ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
                ConfiguredFundingStreams, Parameters,
            },
        },
    };
    use std::ops::Neg;

    let height = Height(1_000);
    let reserve = Amount::<NonNegative>::try_from(10_000_000_000u64)?;
    let network = Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(height.0),
            ..Default::default()
        })?
        .with_initial_nsm_value_balance(reserve)
        .with_nsm_reissuance_height(height)
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(height..Height(1_010)),
            recipients: Some(vec![
                ConfiguredFundingStreamRecipient::new_for(FundingStreamReceiver::MajorGrants),
                ConfiguredFundingStreamRecipient {
                    receiver: FundingStreamReceiver::Deferred,
                    numerator: 12,
                    addresses: None,
                },
            ]),
        }])
        .to_network()?;
    let before_seed = ValueBalance::zero();
    let header: Header = zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
    let make_block = |height, outputs| Block {
        header: Arc::new(header),
        transactions: vec![Arc::new(Transaction::test_v1(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            outputs,
            LockTime::unlocked(),
        ))],
    };
    let before_activation = make_block(
        Height(999),
        vec![transparent::Output::new(
            scheduled_block_subsidy(Height(999), &network)?,
            transparent::Script::new(&[]),
        )],
    );
    let pre_activation_change = before_activation.chain_value_pool_change(
        &HashMap::<_, transparent::Utxo>::new(),
        DeferredPoolBalanceChange::zero(),
        &network,
        before_seed,
    )?;
    let parent = before_seed.add_chain_value_pool_change(pre_activation_change)?;
    assert!(parent.nsm_amount().is_zero());

    let total = block_subsidy_with_parent_pools(height, &network, parent)?;
    assert_eq!(
        (total - scheduled_block_subsidy(height, &network)?)?.zatoshis(),
        1_375
    );
    let funding = funding_stream_values(height, &network, total)?;
    let grant = funding[&FundingStreamReceiver::MajorGrants];
    let deferred = funding[&FundingStreamReceiver::Deferred];
    assert_eq!(grant.zatoshis(), total.zatoshis() * 8 / 100);
    assert_eq!(deferred.zatoshis(), total.zatoshis() * 12 / 100);
    let grant_script = funding_stream_address(height, &network, FundingStreamReceiver::MajorGrants)
        .expect("configured funding address")
        .script();
    let miner = (total - grant - deferred)?;
    let block = make_block(
        height,
        vec![
            transparent::Output::new(miner, transparent::Script::new(&[])),
            transparent::Output::new(grant, grant_script.clone()),
        ],
    );
    let deferred_change = DeferredPoolBalanceChange::new(deferred.constrain::<NegativeAllowed>()?);
    let (change, fees) = block.chain_value_pool_change_and_fees(
        &HashMap::<_, transparent::Utxo>::new(),
        deferred_change,
        &network,
        parent,
    )?;
    assert_eq!(fees, Some(Amount::zero()));
    assert_eq!(change.nsm_amount().zatoshis(), 9_999_998_625);
    let after = parent.add_chain_value_pool_change(change)?;
    assert_eq!(after.nsm_amount().zatoshis(), 9_999_998_625);
    assert_eq!(after.total()?, (parent.total()? + total)?);
    let next = make_block(height.next()?, Vec::new());
    let next_change = next.chain_value_pool_change(
        &HashMap::<_, transparent::Utxo>::new(),
        DeferredPoolBalanceChange::zero(),
        &network,
        after,
    )?;
    assert_eq!(next_change.nsm_amount().zatoshis(), -1_375);
    assert_eq!(ValueBalance::from_bytes(&after.to_bytes())?, after);

    // Keeping the total correct but paying funding from scheduled issuance alone is invalid.
    let scheduled_grant =
        funding_stream_values(height, &network, scheduled_block_subsidy(height, &network)?)?
            [&FundingStreamReceiver::MajorGrants];
    let underfunded = make_block(
        height,
        vec![
            transparent::Output::new(
                (total - scheduled_grant - deferred)?,
                transparent::Script::new(&[]),
            ),
            transparent::Output::new(scheduled_grant, grant_script),
        ],
    );
    assert_eq!(
        underfunded.chain_value_pool_change(
            &HashMap::<_, transparent::Utxo>::new(),
            deferred_change,
            &network,
            parent,
        )?,
        change,
        "ledger accounting does not validate funding payouts",
    );
    assert!(matches!(
        subsidy_is_valid(&underfunded, &network, total),
        Err(CoinbaseTransactionError::Subsidy(
            SubsidyError::FundingStreamNotFound
        )),
    ));
    assert_eq!(subsidy_is_valid(&block, &network, total)?, deferred_change);
    let mut other_parent = after;
    other_parent.set_nsm_amount((reserve * 2)?);
    let other_total = block_subsidy_with_parent_pools(height.next()?, &network, other_parent)?;
    assert!(matches!(
        subsidy_is_valid(&block, &network, other_total),
        Err(CoinbaseTransactionError::Subsidy(
            SubsidyError::FundingStreamNotFound
        )),
    ));

    // Both a reissuance reorg and an activation-crossing reorg restore the exact prior pools.
    assert_eq!(after.add_chain_value_pool_change(change.neg())?, parent);
    assert_eq!(
        parent.add_chain_value_pool_change(pre_activation_change.neg())?,
        before_seed
    );
    Ok(())
}

#[test]
fn blockheader_serialization() {
    let _init_guard = zebra_test::init();

    // Includes the 32-byte nonce and 3-byte equihash length field.
    const BLOCK_HEADER_LENGTH: usize = crate::work::equihash::Solution::INPUT_LENGTH
        + 32
        + 3
        + crate::work::equihash::SOLUTION_SIZE;

    for block in zebra_test::vectors::BLOCKS.iter() {
        // successful deserialization

        let header_bytes = &block[..BLOCK_HEADER_LENGTH];

        let mut header = header_bytes
            .zcash_deserialize_into::<Header>()
            .expect("blockheader test vector should deserialize");

        // successful serialization

        let _serialized_header = header
            .zcash_serialize_to_vec()
            .expect("blockheader test vector should serialize");

        // deserialiation errors

        let header_bytes = [&[255; 4], &header_bytes[4..]].concat();

        let deserialization_err = header_bytes
            .zcash_deserialize_into::<Header>()
            .expect_err("blockheader test vector should fail to deserialize");

        let SerializationError::Parse(err_msg) = deserialization_err else {
            panic!("SerializationError variant should be Parse")
        };

        assert_eq!(err_msg, "high bit was set in version field");

        let header_bytes = [&[0; 4], &header_bytes[4..]].concat();

        let deserialization_err = header_bytes
            .zcash_deserialize_into::<Header>()
            .expect_err("blockheader test vector should fail to deserialize");

        let SerializationError::Parse(err_msg) = deserialization_err else {
            panic!("SerializationError variant should be Parse")
        };

        assert_eq!(err_msg, "version must be at least 4");

        // serialiation errors

        header.version = u32::MAX;

        let serialization_err = header
            .zcash_serialize_to_vec()
            .expect_err("blockheader test vector with modified version should fail to serialize");

        assert_eq!(
            serialization_err.kind(),
            std::io::ErrorKind::Other,
            "error kind should be Other"
        );

        let err_msg = serialization_err
            .into_inner()
            .expect("there should be an inner error");

        assert_eq!(err_msg.to_string(), "high bit was set in version field");
    }
}

/// Checks that [`Header::serialized_size`] matches the actual size of serialized headers
/// on every network.
#[test]
fn blockheader_serialized_size() {
    let _init_guard = zebra_test::init();

    // `BLOCKS` contains Mainnet and Testnet blocks, whose headers have the same size.
    for block in zebra_test::vectors::BLOCKS.iter() {
        let mut header = block[..Header::serialized_size(&Network::Mainnet)]
            .zcash_deserialize_into::<Header>()
            .expect("blockheader test vector should deserialize");

        let serialized_header = header
            .zcash_serialize_to_vec()
            .expect("blockheader test vector should serialize");

        assert_eq!(
            serialized_header.len(),
            Header::serialized_size(&Network::Mainnet),
            "serialized header size should match Header::serialized_size on Mainnet"
        );

        // Regtest headers only differ in the size of the Equihash solution.
        header.solution = crate::work::equihash::Solution::from_bytes(
            &[0; crate::work::equihash::REGTEST_SOLUTION_SIZE],
        )
        .expect("Regtest solution size should be valid");

        let serialized_header = header
            .zcash_serialize_to_vec()
            .expect("Regtest blockheader should serialize");

        assert_eq!(
            serialized_header.len(),
            Header::serialized_size(&Network::new_regtest(Default::default())),
            "serialized header size should match Header::serialized_size on Regtest"
        );
    }
}

#[test]
fn round_trip_blocks() {
    let _init_guard = zebra_test::init();

    // this one has a bad version field, but it is still valid
    zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("bad version block test vector should deserialize");

    // now do a round-trip test on all the block test vectors
    for block_bytes in zebra_test::vectors::BLOCKS.iter() {
        let block = block_bytes
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let round_trip_bytes = block
            .zcash_serialize_to_vec()
            .expect("vec serialization is infallible");

        assert_eq!(&round_trip_bytes[..], *block_bytes);
    }
}

#[test]
fn coinbase_parsing_rejects_above_0x80() {
    let _init_guard = zebra_test::init();

    zebra_test::vectors::BAD_BLOCK_MAINNET_202_BYTES
        .zcash_deserialize_into::<Block>()
        .expect_err("parsing fails");
}

#[test]
fn block_test_vectors_unique() {
    let _init_guard = zebra_test::init();

    let block_count = zebra_test::vectors::BLOCKS.len();
    let block_hashes: HashSet<_> = zebra_test::vectors::BLOCKS
        .iter()
        .map(|b| {
            b.zcash_deserialize_into::<Block>()
                .expect("block is structurally valid")
                .hash()
        })
        .collect();

    // putting the same block in two files is an easy mistake to make
    assert_eq!(
        block_count,
        block_hashes.len(),
        "block test vectors must be unique"
    );
}

/// Checks that:
///
/// - the block test vector indexes match the heights in the block data;
/// - each post-Sapling block has a corresponding final Sapling root;
/// - each post-Orchard block has a corresponding final Orchard root.
#[test]
fn block_test_vectors() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let sapling_anchors = net.sapling_anchors();
        let orchard_anchors = net.orchard_anchors();

        for (&height, block) in net.block_iter() {
            let block = block
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid");
            assert_eq!(
                block.coinbase_height().expect("block height is valid").0,
                height,
                "deserialized height must match BTreeMap key height"
            );

            if height
                >= Sapling
                    .activation_height(&net)
                    .expect("activation height")
                    .0
            {
                assert!(
                sapling_anchors.contains_key(&height),
                "post-sapling block test vectors must have matching sapling root test vectors: \
                 missing {net} {height}"
            );
            }

            if height >= Nu5.activation_height(&net).expect("activation height").0 {
                assert!(
                    orchard_anchors.contains_key(&height),
                    "post-nu5 block test vectors must have matching orchard root test vectors: \
                 missing {net} {height}"
                );
            }
        }
    }
}

/// Checks that the block commitment field parses without errors.
/// For sapling and blossom blocks, also check the final sapling root value.
///
/// TODO: add chain history test vectors?
#[test]
fn block_commitment() {
    let _init_guard = zebra_test::init();

    for net in Network::iter() {
        let sapling_anchors = net.sapling_anchors();

        for (height, block) in net.block_iter() {
            if let FinalSaplingRoot(anchor) = block
                .zcash_deserialize_into::<Block>()
                .expect("block is structurally valid")
                .commitment(&net)
                .expect("unexpected structurally invalid block commitment at {net} {height}")
            {
                let expected_anchor = *sapling_anchors
                    .get(height)
                    .expect("unexpected missing final sapling root test vector");
                assert_eq!(
                    anchor,
                    sapling::tree::Root::try_from(*expected_anchor).unwrap(),
                    "unexpected invalid final sapling root commitment at {net} {height}"
                );
            }
        }
    }
}

#[test]
fn block_limits_multi_tx() {
    let _init_guard = zebra_test::init();

    // Test multiple small transactions to fill a block max size

    // Create a block just below the limit
    let mut block = generate::large_multi_transaction_block();

    // Serialize the block
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() <= MAX_BLOCK_BYTES as usize);

    // Deserialize by now is ok as we are lower than the limit
    let block2 = Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");
    assert_eq!(block, block2);

    // Add 1 more transaction to the block, limit will be reached
    block = generate::oversized_multi_transaction_block();

    // Serialize will still be fine
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() > MAX_BLOCK_BYTES as usize);

    // Deserialize will now fail
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

#[test]
fn block_limits_single_tx() {
    let _init_guard = zebra_test::init();

    // Test block limit with a big single transaction

    // Create a block just below the limit
    let mut block = generate::large_single_transaction_block_many_inputs();

    // Serialize the block
    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() <= MAX_BLOCK_BYTES as usize);

    // Deserialize by now is ok as we are lower than the limit
    Block::zcash_deserialize(&data[..])
        .expect("block should deserialize as we are just below limit");

    // Add 1 more input to the transaction, limit will be reached
    block = generate::oversized_single_transaction_block_many_inputs();

    let mut data = Vec::new();
    block
        .zcash_serialize(&mut data)
        .expect("block should serialize as we are not limiting generation yet");

    assert!(data.len() > MAX_BLOCK_BYTES as usize);

    // Will fail as block overall size is above limit
    Block::zcash_deserialize(&data[..]).expect_err("block should not deserialize");
}

/// Test wrapper for `BlockHeader.time_is_valid_at`.
///
/// Generates a block header, sets its `time` to `block_header_time`, then
/// calls `time_is_valid_at`.
fn node_time_check(
    block_header_time: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), BlockTimeError> {
    let (mut header, _header_bytes) = generate::block_header();
    header.time = block_header_time;
    // pass a zero height and hash - they are only used in the returned error
    header.time_is_valid_at(now, &Height(0), &Hash([0; 32]))
}

#[test]
fn time_check_now() {
    let _init_guard = zebra_test::init();

    // These checks are deterministic, because all the times are offset
    // from the current time.
    let now = Utc::now();
    let three_hours_in_the_past = now - Duration::hours(3);
    let two_hours_in_the_future = now + Duration::hours(2);
    let two_hours_and_one_second_in_the_future = now + Duration::hours(2) + Duration::seconds(1);

    node_time_check(now, now).expect("the current time should be valid as a block header time");
    node_time_check(three_hours_in_the_past, now)
        .expect("a past time should be valid as a block header time");
    node_time_check(two_hours_in_the_future, now)
        .expect("2 hours in the future should be valid as a block header time");
    node_time_check(two_hours_and_one_second_in_the_future, now)
        .expect_err("2 hours and 1 second in the future should be invalid as a block header time");

    // Now invert the tests
    // 3 hours in the future should fail
    node_time_check(now, three_hours_in_the_past)
        .expect_err("3 hours in the future should be invalid as a block header time");
    // The past should succeed
    node_time_check(now, two_hours_in_the_future)
        .expect("2 hours in the past should be valid as a block header time");
    node_time_check(now, two_hours_and_one_second_in_the_future)
        .expect("2 hours and 1 second in the past should be valid as a block header time");
}

/// Valid unix epoch timestamps for blocks, in seconds
static BLOCK_HEADER_VALID_TIMESTAMPS: &[i64] = &[
    // These times are currently invalid DateTimes, but they could
    // become valid in future chrono versions
    i64::MIN,
    i64::MIN + 1,
    // These times are valid DateTimes
    (i32::MIN as i64) - 1,
    (i32::MIN as i64),
    (i32::MIN as i64) + 1,
    -1,
    0,
    1,
    LockTime::MIN_TIMESTAMP - 1,
    LockTime::MIN_TIMESTAMP,
    LockTime::MIN_TIMESTAMP + 1,
];

/// Invalid unix epoch timestamps for blocks, in seconds
static BLOCK_HEADER_INVALID_TIMESTAMPS: &[i64] = &[
    (i32::MAX as i64) - 1,
    (i32::MAX as i64),
    (i32::MAX as i64) + 1,
    LockTime::MAX_TIMESTAMP - 1,
    LockTime::MAX_TIMESTAMP,
    LockTime::MAX_TIMESTAMP + 1,
    // These times are currently invalid DateTimes, but they could
    // become valid in future chrono versions
    i64::MAX - 1,
    i64::MAX,
];

#[test]
fn time_check_fixed() {
    let _init_guard = zebra_test::init();

    // These checks are non-deterministic, but the times are all in the
    // distant past or far future. So it's unlikely that the test
    // machine will have a clock that makes these tests fail.
    let now = Utc::now();

    for valid_timestamp in BLOCK_HEADER_VALID_TIMESTAMPS {
        let block_header_time = match Utc.timestamp_opt(*valid_timestamp, 0) {
            LocalResult::Single(time) => time,
            LocalResult::None => {
                // Skip the test if the timestamp is invalid
                continue;
            }
            LocalResult::Ambiguous(_, _) => {
                // Utc doesn't have ambiguous times
                unreachable!();
            }
        };
        node_time_check(block_header_time, now)
            .expect("the time should be valid as a block header time");
        // Invert the check, leading to an invalid time
        node_time_check(now, block_header_time)
            .expect_err("the inverse comparison should be invalid");
    }

    for invalid_timestamp in BLOCK_HEADER_INVALID_TIMESTAMPS {
        let block_header_time = match Utc.timestamp_opt(*invalid_timestamp, 0) {
            LocalResult::Single(time) => time,
            LocalResult::None => {
                // Skip the test if the timestamp is invalid
                continue;
            }
            LocalResult::Ambiguous(_, _) => {
                // Utc doesn't have ambiguous times
                unreachable!();
            }
        };
        node_time_check(block_header_time, now)
            .expect_err("the time should be invalid as a block header time");
        // Invert the check, leading to a valid time
        node_time_check(now, block_header_time).expect("the inverse comparison should be valid");
    }
}

/// `Block::transaction_fees` sums the fees of the non-coinbase transactions, so the fees ZIP 235 removes
/// from circulation are rounded once per block rather than once per transaction.
#[test]
fn transaction_fees_sum_the_non_coinbase_transactions() {
    let _init_guard = zebra_test::init();

    let height = Height(347_499);
    let amount = |zatoshis: i64| Amount::<NonNegative>::try_from(zatoshis).expect("valid amount");
    let output = |zatoshis| transparent::Output {
        value: amount(zatoshis),
        lock_script: transparent::Script::new(&[]),
    };

    let coinbase = Transaction::test_v4(
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![output(1_000_000)],
        LockTime::unlocked(),
        height,
    );

    let mut utxos = HashMap::new();
    let mut transactions = vec![Arc::new(coinbase)];
    for (index, fee) in [1, 1, 1_000].into_iter().enumerate() {
        let outpoint = transparent::OutPoint {
            hash: crate::transaction::Hash([index as u8 + 1; 32]),
            index: 0,
        };
        utxos.insert(
            outpoint,
            transparent::Utxo::new(output(10_000), Height(1), false),
        );
        transactions.push(Arc::new(Transaction::test_v4(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            vec![output(10_000 - fee)],
            LockTime::unlocked(),
            height,
        )));
    }

    let mut block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into::<Block>()
        .expect("block should deserialize");

    let network = Network::new_regtest(
        crate::parameters::testnet::ConfiguredActivationHeights {
            nu7: Some(height.0),
            ..Default::default()
        }
        .into(),
    );
    for (count, fees, contribution) in [(1, 0, 0), (3, 2, 1), (4, 1_002, 601)] {
        block.transactions = transactions[..count].to_vec();
        assert_eq!(block.transaction_fees(&utxos), Ok(amount(fees)));
        let (change, gross_fees) = block
            .chain_value_pool_change_and_fees(
                &utxos,
                DeferredPoolBalanceChange::zero(),
                &network,
                ValueBalance::zero(),
            )
            .expect("valid transaction balances");
        assert_eq!(gross_fees, Some(amount(fees)));
        assert_eq!(change.nsm_amount().zatoshis(), contribution);
        assert_eq!(change.transparent_amount().zatoshis(), 1_000_000 - fees);
    }

    // NU7 rejects an individual negative fee even when other fees would offset it.
    Arc::make_mut(&mut block.transactions[3]).set_outputs(vec![output(10_001)]);
    assert!(matches!(
        block.chain_value_pool_change_and_fees(
            &utxos,
            DeferredPoolBalanceChange::zero(),
            &network,
            ValueBalance::zero(),
        ),
        Err(crate::value_balance::ValueBalanceError::Total(_)),
    ));
    let (change, fees) = block
        .chain_value_pool_change_and_fees(
            &utxos,
            DeferredPoolBalanceChange::zero(),
            &Network::Mainnet,
            ValueBalance::zero(),
        )
        .expect("pre-NU7 accounting leaves fee validation to the verifier");
    assert_eq!(fees, None);
    assert_eq!(change.transparent_amount().zatoshis(), 999_999);
    assert_eq!(change.nsm_amount().zatoshis(), 0);
}
