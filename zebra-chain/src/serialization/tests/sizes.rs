//! Arithmetic sizes checked against independently encoded bytes.

use std::sync::Arc;

use zcash_primitives::transaction::{Authorized, TransactionData, TxVersion};
use zcash_protocol::{
    consensus::{BlockHeight, BranchId},
    value::ZatBalance,
};

use crate::{
    block::{Block, CountedHeader, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::{
        CompactSize64, CompactSizeMessage, ZcashDeserializeInto, ZcashSerialize,
        MAX_PROTOCOL_MESSAGE_LEN,
    },
    transaction::{
        arbitrary::{fake_bundle_for_branch, fake_v6_transaction, test_transactions},
        LockTime, Transaction, UnminedTx,
    },
    transparent::{self, Input, Output, Script},
    work::equihash::{Solution, REGTEST_SOLUTION_SIZE},
};

fn assert_size<T: ZcashSerialize>(value: &T) -> usize {
    let encoded = value.zcash_serialize_to_vec().unwrap();
    assert_eq!(value.zcash_serialized_size(), encoded.len());
    encoded.len()
}

#[test]
fn compact_size_boundaries() {
    for (value, expected) in [
        (0, 1),
        (252, 1),
        (253, 3),
        (65535, 3),
        (65536, 5),
        (u64::from(u32::MAX), 5),
        (u64::from(u32::MAX) + 1, 9),
        (u64::MAX, 9),
    ] {
        assert_eq!(assert_size(&CompactSize64::from(value)), expected);
        if let Ok(value) = usize::try_from(value) {
            if value <= MAX_PROTOCOL_MESSAGE_LEN {
                assert_eq!(
                    assert_size(&CompactSizeMessage::try_from(value).unwrap()),
                    expected
                );
            } else {
                assert!(CompactSizeMessage::try_from(value).is_err());
            }
        }
    }
    for len in [0, 252, 253, 65535, 65536, MAX_PROTOCOL_MESSAGE_LEN] {
        assert_size(&vec![0u8; len]);
    }
}

#[test]
fn transparent_script_and_coinbase_size_boundaries() {
    for len in [0, 252, 253, 65535, 65536] {
        let script = Script::new(&vec![0x51; len]);
        assert_size(&script);
        assert_size(&Input::PrevOut {
            outpoint: transparent::OutPoint {
                hash: [1; 32].into(),
                index: 0,
            },
            unlock_script: script.clone(),
            sequence: u32::MAX,
        });
        assert_size(&Output {
            value: crate::amount::Amount::zero(),
            lock_script: script,
        });
    }
    for height in [
        1,
        16,
        17,
        127,
        128,
        32767,
        32768,
        0x7f_ffff,
        0x80_0000,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
    ] {
        for len in [1, 94, 252, 253] {
            assert_size(&Input::Coinbase {
                height: Height(height),
                data: vec![0; len],
                sequence: u32::MAX,
            });
        }
    }
    assert_size(&Input::Coinbase {
        height: Height::MIN,
        data: transparent::serialize::GENESIS_COINBASE_SCRIPT_SIG.to_vec(),
        sequence: u32::MAX,
    });
}

#[test]
fn transaction_size_versions_and_count_boundaries() {
    for (version, empty_size) in [
        (TxVersion::Sprout(1), 10),
        (TxVersion::Sprout(2), 11),
        (TxVersion::Sprout(3), 11),
        (TxVersion::Sprout(0x7fff_ffff), 11),
        (TxVersion::V3, 19),
        (TxVersion::V4, 29),
        (TxVersion::V5, 25),
    ] {
        for count in [0, 252, 253] {
            let bundle = zcash_transparent::bundle::Bundle {
                vin: vec![
                    zcash_transparent::bundle::TxIn::from_parts(
                        zcash_transparent::bundle::OutPoint::NULL,
                        zcash_transparent::address::Script(zcash_script::script::Code(vec![
                            0x51, 0
                        ])),
                        u32::MAX,
                    );
                    count
                ],
                vout: vec![
                    zcash_transparent::bundle::TxOut::new(
                        zcash_protocol::value::Zatoshis::ZERO,
                        zcash_transparent::address::Script(zcash_script::script::Code(vec![])),
                    );
                    count
                ],
                authorization: zcash_transparent::bundle::Authorized,
            };
            let data = TransactionData::<Authorized>::from_parts(
                version,
                BranchId::Nu5,
                0,
                BlockHeight::from_u32(0),
                Some(bundle),
                None,
                None,
                None,
            );
            let tx = Arc::new(Transaction(data.freeze().unwrap()));
            let size = assert_size(&tx);
            if count == 0 {
                assert_eq!(size, empty_size);
            }
            assert_eq!(UnminedTx::from(tx.clone()).size, size);
            assert_size(&vec![tx]);
        }
    }
}

#[test]
fn fee_actions_follow_encoded_transparent_sizes() {
    for (input_script_len, output_script_len) in [(109, 25), (110, 25), (109, 26), (252, 253)] {
        let inputs = vec![
            Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [1; 32].into(),
                    index: 0
                },
                unlock_script: Script::new(&vec![0; input_script_len]),
                sequence: u32::MAX,
            };
            2
        ];
        let outputs = vec![
            Output {
                value: crate::amount::Amount::zero(),
                lock_script: Script::new(&vec![0; output_script_len]),
            };
            2
        ];
        let input_bytes: usize = inputs
            .iter()
            .map(|input| input.zcash_serialize_to_vec().unwrap().len())
            .sum();
        let output_bytes: usize = outputs
            .iter()
            .map(|output| output.zcash_serialize_to_vec().unwrap().len())
            .sum();
        let expected = input_bytes
            .div_ceil(150)
            .max(output_bytes.div_ceil(34))
            .max(2);
        let tx = Transaction::test_v5(
            NetworkUpgrade::Nu5,
            inputs,
            outputs,
            LockTime::unlocked(),
            Height(0),
        );
        assert_eq!(
            crate::transaction::zip317::conventional_actions(&tx),
            u32::try_from(expected).unwrap(),
        );
        assert_eq!(
            UnminedTx::from(Arc::new(tx)).size,
            input_bytes + output_bytes + 25
        );
    }
}

#[test]
fn transaction_and_block_vector_sizes() {
    let mut sprout_phgr = false;
    let mut sprout_groth = false;
    let mut sapling_spends = false;
    let mut sapling_outputs = false;
    for network in [Network::Mainnet, Network::new_default_testnet()] {
        for (_, tx) in test_transactions(&network) {
            assert_size(&tx);
            if let Some(bundle) = tx.sprout_bundle() {
                for js in &bundle.joinsplits {
                    sprout_groth |= js.groth_proof_bytes().is_some();
                    sprout_phgr |= js.groth_proof_bytes().is_none();
                }
            }
            if let Some(bundle) = tx.sapling_bundle() {
                sapling_spends |= !bundle.shielded_spends().is_empty();
                sapling_outputs |= !bundle.shielded_outputs().is_empty();
                // Keep the real Sapling descriptions but exercise their v5 and v6 layouts.
                let data = TransactionData::<Authorized>::from_parts(
                    TxVersion::V5,
                    BranchId::Nu5,
                    0,
                    BlockHeight::from_u32(0),
                    None,
                    None,
                    Some(bundle.clone()),
                    None,
                );
                assert_size(&Transaction(data.freeze().unwrap()));
                let data = TransactionData::<Authorized>::from_parts_v6(
                    BranchId::Nu6_3,
                    0,
                    BlockHeight::from_u32(0),
                    None,
                    Some(bundle.clone()),
                    None,
                    None,
                );
                assert_size(&Transaction(data.freeze().unwrap()));
            }
        }
        for (_, bytes) in network.block_iter() {
            let block: Block = bytes.zcash_deserialize_into().unwrap();
            assert_size(&block);
            assert_eq!(assert_size(&block.header), 1487);
        }
    }
    assert!(sprout_phgr && sprout_groth && sapling_spends && sapling_outputs);
}

#[test]
fn shielded_bundle_count_boundaries() {
    let tx = test_transactions(&Network::Mainnet)
        .map(|(_, tx)| tx)
        .find(|tx| {
            tx.sapling_bundle().is_some_and(|bundle| {
                !bundle.shielded_spends().is_empty() && !bundle.shielded_outputs().is_empty()
            })
        })
        .expect("vectors contain a Sapling bundle with spends and outputs");
    let source = tx.sapling_bundle().unwrap();
    for (spends, outputs) in [(0, 253), (253, 0), (252, 252), (253, 253)] {
        let bundle = sapling_crypto::Bundle::from_parts(
            vec![source.shielded_spends()[0].clone(); spends],
            vec![source.shielded_outputs()[0].clone(); outputs],
            *source.value_balance(),
            *source.authorization(),
        );
        for version in [TxVersion::V4, TxVersion::V5] {
            let data = TransactionData::<Authorized>::from_parts(
                version,
                BranchId::Nu5,
                0,
                BlockHeight::from_u32(0),
                None,
                None,
                bundle.clone(),
                None,
            );
            assert_size(&Transaction(data.freeze().unwrap()));
        }
        let data = TransactionData::<Authorized>::from_parts_v6(
            BranchId::Nu6_3,
            0,
            BlockHeight::from_u32(0),
            None,
            bundle,
            None,
            None,
        );
        assert_size(&Transaction(data.freeze().unwrap()));
    }
    // The JoinSplit encoder chooses the proof variant on each actual description.
    for groth in [false, true] {
        let tx = test_transactions(&Network::Mainnet)
            .map(|(_, tx)| tx)
            .find(|tx| {
                tx.sprout_bundle().is_some_and(|bundle| {
                    bundle
                        .joinsplits
                        .iter()
                        .any(|js| js.groth_proof_bytes().is_some() == groth)
                })
            })
            .expect("vectors contain both Sprout proof variants");
        let source = tx.sprout_bundle().unwrap();
        let js = source
            .joinsplits
            .iter()
            .find(|js| js.groth_proof_bytes().is_some() == groth)
            .unwrap();
        for count in [252, 253] {
            let mut bundle = source.clone();
            bundle.joinsplits = vec![js.clone(); count];
            let data = TransactionData::<Authorized>::from_parts(
                if groth { TxVersion::V4 } else { TxVersion::V3 },
                BranchId::Canopy,
                0,
                BlockHeight::from_u32(0),
                None,
                Some(bundle),
                None,
                None,
            );
            assert_size(&Transaction(data.freeze().unwrap()));
        }
    }
}

fn proof_of_length(
    bundle: ::orchard::Bundle<::orchard::bundle::Authorized, ZatBalance>,
    len: usize,
) -> ::orchard::Bundle<::orchard::bundle::Authorized, ZatBalance> {
    bundle.map_authorization(
        &mut (),
        |_, _, sig| sig,
        |_, auth| {
            ::orchard::bundle::Authorized::from_parts(
                ::orchard::Proof::new(vec![0; len]),
                auth.binding_signature().clone(),
            )
        },
    )
}

#[test]
fn orchard_ironwood_actual_proof_sizes_and_empty_slots() {
    for count in [1, 252, 253] {
        let orchard =
            fake_bundle_for_branch(BranchId::Nu6_3, ::orchard::ValuePool::Orchard, count, 7)
                .unwrap();
        let ironwood =
            fake_bundle_for_branch(BranchId::Nu6_3, ::orchard::ValuePool::Ironwood, count, 8)
                .unwrap();
        for proof_len in [0, 252, 253, 65535, 65536] {
            let orchard = proof_of_length(orchard.clone(), proof_len);
            let ironwood = proof_of_length(ironwood.clone(), proof_len);
            for (orchard, ironwood) in [
                (None, None),
                (Some(orchard.clone()), None),
                (None, Some(ironwood.clone())),
                (Some(orchard), Some(ironwood)),
            ] {
                let tx = fake_v6_transaction(NetworkUpgrade::Nu6_3, orchard, ironwood);
                assert_size(&tx);
            }
        }
    }
    for proof_len in [0, 252, 253, 65535, 65536] {
        let bundle =
            fake_bundle_for_branch(BranchId::Nu5, ::orchard::ValuePool::Orchard, 1, 9).unwrap();
        let tx = Transaction::test_v5_with_orchard(
            NetworkUpgrade::Nu5,
            vec![],
            vec![],
            LockTime::unlocked(),
            Height(0),
            Some(proof_of_length(bundle, proof_len)),
        );
        assert_size(&tx);
    }
}

#[test]
fn transaction_size_preserves_upstream_length_limit() {
    let bundle =
        fake_bundle_for_branch(BranchId::Nu5, ::orchard::ValuePool::Orchard, 1, 9).unwrap();
    let limit = usize::try_from(zcash_encoding::MAX_COMPACT_SIZE).unwrap();
    for len in [limit, limit + 1] {
        let tx = Transaction::test_v5_with_orchard(
            NetworkUpgrade::Nu5,
            vec![],
            vec![],
            LockTime::unlocked(),
            Height(0),
            Some(proof_of_length(bundle.clone(), len)),
        );
        if len == limit {
            assert_size(&tx);
        } else {
            assert_eq!(
                tx.zcash_serialize(std::io::sink()).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput
            );
            assert!(std::panic::catch_unwind(|| tx.zcash_serialized_size()).is_err());
        }
    }
}

#[test]
fn header_solution_and_block_count_boundaries() {
    let (_, bytes) = Network::Mainnet.block_iter().next().unwrap();
    let block: Block = bytes.zcash_deserialize_into().unwrap();
    for (solution, expected_size) in [
        (block.header.solution, 1487),
        (Solution::Regtest([0; REGTEST_SOLUTION_SIZE]), 177),
    ] {
        let mut header = *block.header;
        header.solution = solution;
        assert_eq!(assert_size(&header), expected_size);
        assert_size(&CountedHeader {
            header: Arc::new(header),
        });
        for count in [0, 252, 253] {
            assert_size(&Block {
                header: Arc::new(header),
                transactions: vec![block.transactions[0].clone(); count],
            });
        }
    }
}
