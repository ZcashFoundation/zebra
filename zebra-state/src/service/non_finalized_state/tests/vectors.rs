//! Fixed test vectors for the non-finalized state.

use std::{sync::Arc, time::Duration};

use zebra_chain::{
    amount::{Amount, NegativeAllowed, NonNegative},
    block::{self, Block, Height},
    history_tree::NonEmptyHistoryTree,
    orchard,
    parallel::tree::NoteCommitmentTrees,
    parameters::{Network, NetworkUpgrade},
    primitives::zcash_history::BlockCommitmentTreeRoots,
    serialization::ZcashDeserializeInto,
    subtree::NoteCommitmentSubtree,
    transparent,
    value_balance::ValueBalance,
};
use zebra_test::prelude::*;

use crate::{
    arbitrary::Prepare,
    service::{
        finalized_state::{calculate_deferred_pool_balance_change, FinalizedState},
        non_finalized_state::{Chain, NonFinalizedState, MIN_DURATION_BETWEEN_BACKUP_UPDATES},
        InvalidateError, ReconsiderError,
    },
    tests::FakeChainHelper,
    Config,
};

#[test]
fn construct_empty() {
    let _init_guard = zebra_test::init();
    let _chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::zero(),
    );
}

#[test]
fn construct_single() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;

    let mut chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;

    assert_eq!(1, chain.blocks.len());

    Ok(())
}

#[test]
fn chain_snapshots_share_records_and_outputs_until_finalization() -> Result<()> {
    let _init_guard = zebra_test::init();
    let block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let root_chain = Chain::new(
        &Network::Mainnet,
        Height(434_872),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    )
    .push(block.clone().prepare().test_with_zero_spent_utxos())?;
    let mut chain = root_chain.clone().push(
        block
            .make_fake_child()
            .prepare()
            .test_with_zero_spent_utxos(),
    )?;
    let snapshot = chain.clone();
    for (current, retained) in chain.blocks.values().zip(snapshot.blocks.values()) {
        assert!(Arc::ptr_eq(current, retained));
    }
    for (outpoint, utxo) in chain.created_utxos.iter() {
        assert!(Arc::ptr_eq(
            utxo,
            snapshot.created_utxos.get(outpoint).unwrap()
        ));
    }

    let outpoint = *root_chain.created_utxos.iter().next().unwrap().0;
    let mut owned = chain.created_utxo(&outpoint).unwrap();
    owned.output.lock_script = transparent::Script::new(&[0; 4096]);
    assert_ne!(owned, snapshot.created_utxo(&outpoint).unwrap());
    let forked = chain.fork(block.hash()).unwrap();
    assert!(forked.eq_internal_state(&root_chain));
    assert!(chain.eq_internal_state(&snapshot));
    drop(forked);
    drop(root_chain);

    let root_record = Arc::downgrade(chain.blocks.values().next().unwrap());
    let output = Arc::downgrade(chain.created_utxos.get(&outpoint).unwrap());
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, block.hash());
    assert!(snapshot.created_utxo(&outpoint).is_some());
    assert!(chain.created_utxo(&outpoint).is_none());
    assert!(root_record.upgrade().is_some());
    drop(finalized);
    assert!(root_record.upgrade().is_some());
    drop(snapshot);
    assert!(root_record.upgrade().is_none());
    assert!(output.upgrade().is_none());
    Ok(())
}

/// Build value-conserving transfers with complete spent-output maps, including intra-block spends.
/// This exercises chain indexing, not proof-of-work, script, or subsidy verification.
fn large_transfer_chain(
    destinations: &[&[usize]],
) -> Result<(
    Chain,
    Vec<crate::ContextuallyVerifiedBlock>,
    [transparent::Address; 2],
    Amount<NonNegative>,
)> {
    use std::collections::HashMap;
    use zebra_chain::{
        amount::{DeferredPoolBalanceChange, MAX_MONEY},
        parameters::NetworkKind,
        transaction::{self, LockTime, Transaction},
    };

    let network = Network::Mainnet;
    let finalized_height = Height(600_000);
    let value: Amount<NonNegative> = (MAX_MONEY / 2 + 1).try_into()?;
    let pools = ValueBalance::from_transparent_amount(value);
    let addresses = [
        transparent::Address::from_script_hash(NetworkKind::Mainnet, [0x41; 20]),
        transparent::Address::from_script_hash(NetworkKind::Mainnet, [0x42; 20]),
    ];
    let chain = Chain::new(
        &network,
        finalized_height,
        NoteCommitmentTrees::default(),
        Default::default(),
        pools,
    );
    let mut outpoint = transparent::OutPoint {
        hash: transaction::Hash([0x11; 32]),
        index: 0,
    };
    let mut utxo = transparent::OrderedUtxo::new(
        transparent::Output::new(value, addresses[0].script()),
        finalized_height,
        1,
    );
    let mut parent = block::Hash([0x22; 32]);
    let mut blocks = Vec::new();
    for (block_index, destinations) in destinations.iter().enumerate() {
        let height = (finalized_height + i64::try_from(block_index + 1)?)
            .expect("the fixture heights are below the maximum");
        let mut transactions = vec![Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output::new(
                Amount::zero(),
                transparent::Script::new(&[]),
            )],
            LockTime::unlocked(),
            Height(0),
        ))];
        let mut spent = HashMap::new();
        for &destination in *destinations {
            let transaction = Arc::new(Transaction::test_v4(
                vec![transparent::Input::PrevOut {
                    outpoint,
                    unlock_script: transparent::Script::new(&[0; 107]),
                    sequence: u32::MAX,
                }],
                vec![transparent::Output::new(
                    value,
                    addresses[destination].script(),
                )],
                LockTime::unlocked(),
                Height(0),
            ));
            assert!(spent.insert(outpoint, utxo).is_none());
            outpoint = transparent::OutPoint {
                hash: transaction.hash(),
                index: 0,
            };
            utxo = transparent::OrderedUtxo::new(
                transaction.outputs()[0].clone(),
                height,
                transactions.len(),
            );
            transactions.push(transaction);
        }
        let mut header: block::Header =
            zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
        header.previous_block_hash = parent;
        header.merkle_root = transactions
            .iter()
            .map(|transaction| transaction.hash())
            .collect();
        let block = Arc::new(Block {
            header: Arc::new(header),
            transactions,
        })
        .set_work(10);
        assert_eq!(
            block.transaction_fees(&spent)?,
            Amount::<NonNegative>::zero()
        );
        let contextual = crate::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            block.prepare(),
            spent,
            DeferredPoolBalanceChange::zero(),
            &network,
            pools,
        )?;
        assert_eq!(contextual.chain_value_pool_change, ValueBalance::zero());
        parent = contextual.hash;
        blocks.push(contextual);
    }
    Ok((chain, blocks, addresses, value))
}

/// Run with `RUST_LOG=info` and `--ignored --nocapture`; fixture setup is excluded from timings.
#[test]
#[ignore = "microbenchmark"]
fn transparent_chain_push_microbenchmark() -> Result<()> {
    use std::{hint::black_box, time::Instant};
    use zebra_chain::serialization::ZcashSerialize;

    let _init_guard = zebra_test::init();
    let destinations: Vec<_> = (0..9_000).map(|index| index % 2).collect();
    let (chain, blocks, _, _) = large_transfer_chain(&[&destinations])?;
    let block = &blocks[0];
    let mut samples = Vec::new();
    for iteration in 0..25 {
        let chain = chain.clone();
        let block = block.clone();
        let start = Instant::now();
        let pushed = black_box(chain.push(block)?);
        let elapsed = start.elapsed().as_secs_f64() * 1_000.0;
        if iteration >= 5 {
            samples.push(elapsed);
        }
        drop(pushed);
    }
    samples.sort_by(f64::total_cmp);
    tracing::info!(
        transactions = block.block.transactions.len(),
        bytes = block.block.zcash_serialized_size(),
        min_ms = samples[0],
        median_ms = (samples[9] + samples[10]) / 2.0,
        max_ms = samples[19],
        ?samples,
        "transparent chain push microbenchmark",
    );
    Ok(())
}

#[test]
fn transparent_indexes_match_legacy_push_and_rollback() -> Result<()> {
    use crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes;

    let _init_guard = zebra_test::init();
    let (mut chain, mut blocks, _, _) = large_transfer_chain(&[&[1, 0, 1], &[1, 1, 0], &[0, 1]])?;
    // Contextual maps may contain unrelated outputs; they must not become chain entries.
    let unrelated = transparent::OutPoint {
        hash: zebra_chain::transaction::Hash([0x88; 32]),
        index: 17,
    };
    let unrelated_output = transparent::OrderedUtxo::new(
        transparent::Output::new(Amount::zero(), transparent::Script::new(&[])),
        Height(1),
        1,
    );
    for block in &mut blocks {
        Arc::make_mut(&mut block.new_outputs).insert(unrelated, unrelated_output.clone());
        Arc::make_mut(&mut block.spent_outputs).insert(unrelated, unrelated_output.clone());
    }
    let mut snapshots = Vec::new();
    for block in &blocks {
        chain = chain.push(block.clone())?;
        assert_legacy_indexes(&chain);
        snapshots.push(chain.clone());
    }
    for (block, snapshot) in blocks.iter().zip(&snapshots) {
        let forked = chain.fork(block.hash).unwrap();
        assert!(forked.eq_internal_state(snapshot));
        assert_legacy_indexes(&forked);
    }
    for block in &blocks {
        assert_eq!(chain.pop_root().0.hash, block.hash);
        assert_legacy_indexes(&chain);
        for snapshot in &snapshots {
            assert_legacy_indexes(snapshot);
        }
    }
    Ok(())
}

#[test]
fn mixed_shielded_indexes_match_legacy_push_and_rollback() -> Result<()> {
    use crate::service::non_finalized_state::chain::index::tests::assert_legacy_indexes;

    let _init_guard = zebra_test::init();
    for bytes in [
        &*zebra_test::vectors::BLOCK_MAINNET_396_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_434873_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_1687107_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_1687108_BYTES,
    ] {
        let shielded: Block = bytes.zcash_deserialize_into()?;
        let (mut chain, blocks, _, _) = large_transfer_chain(&[&[1], &[0, 1]])?;
        chain.chain_value_pools = ValueBalance::fake_populated_pool();
        let root = chain.push(blocks[0].clone())?;
        let mut mixed = blocks[1].block.as_ref().clone();
        mixed
            .transactions
            .extend(shielded.transactions.into_iter().skip(1));
        Arc::make_mut(&mut mixed.header).merkle_root = mixed
            .transactions
            .iter()
            .map(|transaction| transaction.hash())
            .collect();
        let mut mixed = Arc::new(mixed).prepare().test_with_zero_spent_utxos();
        Arc::make_mut(&mut mixed.spent_outputs).extend(
            blocks[1]
                .spent_outputs
                .iter()
                .map(|(key, value)| (*key, value.clone())),
        );
        // These fixtures exercise index updates, not cross-upgrade value-pool validation.
        mixed.chain_value_pool_change = ValueBalance::zero();
        let mut chain = root.clone().push(mixed.clone())?;
        assert_legacy_indexes(&chain);
        let reverted = chain.fork(blocks[0].hash).unwrap();
        assert!(reverted.eq_internal_state(&root));
        assert!(reverted.push(mixed)?.eq_internal_state(&chain));
        chain.pop_root();
        assert_legacy_indexes(&chain);
        chain.pop_root();
        assert_legacy_indexes(&chain);
        assert_legacy_indexes(&root);
    }
    Ok(())
}

#[test]
fn multi_output_and_interleaved_shielded_indexes_match_legacy() -> Result<()> {
    use std::collections::{HashMap, HashSet};
    use zebra_chain::{
        amount::DeferredPoolBalanceChange,
        transaction::{self, LockTime, Transaction},
    };

    use crate::{
        service::non_finalized_state::chain::index::tests::assert_legacy_indexes,
        ContextuallyVerifiedBlock, TransactionLocation,
    };

    let _init_guard = zebra_test::init();
    let (mut chain, blocks, addresses, value) = large_transfer_chain(&[&[1], &[1]])?;
    chain.chain_value_pools = ValueBalance::fake_populated_pool();
    let root = chain.push(blocks[0].clone())?;
    let value = u64::from(value);
    let signed_value = i64::try_from(value)?;
    let height = blocks[1].height;
    let mut block = blocks[1].block.as_ref().clone();
    block.transactions.truncate(1);
    let mut outpoint = transparent::OutPoint {
        hash: blocks[0].transaction_hashes[1],
        index: 0,
    };
    let mut spent_output = blocks[0].new_outputs[&outpoint].clone();
    let mut spent = HashMap::new();
    let mut expected_transactions = vec![(
        TransactionLocation::from_usize(blocks[0].height, 1),
        outpoint.hash,
    )];

    let shielded_only = [
        &*zebra_test::vectors::BLOCK_MAINNET_1687106_BYTES,
        &*zebra_test::vectors::BLOCK_MAINNET_1687121_BYTES,
    ]
    .into_iter()
    .map(|bytes| {
        bytes
            .zcash_deserialize_into::<Block>()
            .unwrap()
            .transactions
            .into_iter()
            .find(|transaction| {
                !transaction.has_transparent_inputs_or_outputs() && transaction.has_shielded_data()
            })
            .expect("the fixture contains a shielded-only transaction")
    })
    .collect::<Vec<_>>();
    // The first transaction repeats address B; later transactions pay both A and B again.
    // The second transaction spends the first transaction's middle output, not output zero.
    for (index, outputs) in [
        vec![(3, 0), (value - 7, 1), (4, 1)],
        vec![(value - 9, 1), (2, 0)],
        vec![(value - 10, 0), (1, 1)],
    ]
    .into_iter()
    .enumerate()
    {
        let outputs: Vec<_> = outputs
            .into_iter()
            .map(|(value, address)| {
                transparent::Output::new(value.try_into().unwrap(), addresses[address].script())
            })
            .collect();
        let transaction = Arc::new(Transaction::test_v4(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            outputs.clone(),
            LockTime::unlocked(),
            Height(0),
        ));
        assert!(spent.insert(outpoint, spent_output).is_none());
        let transaction_index = block.transactions.len();
        expected_transactions.push((
            TransactionLocation::from_usize(height, transaction_index),
            transaction.hash(),
        ));
        let output_index = usize::from(index == 0);
        outpoint = transparent::OutPoint {
            hash: transaction.hash(),
            index: output_index.try_into()?,
        };
        spent_output =
            transparent::OrderedUtxo::new(outputs[output_index].clone(), height, transaction_index);
        block.transactions.push(transaction);
        if let Some(shielded) = shielded_only.get(index) {
            block.transactions.push(shielded.clone());
        }
    }
    assert_eq!(block.transactions.len(), 6);
    Arc::make_mut(&mut block.header).merkle_root = block
        .transactions
        .iter()
        .map(|transaction| transaction.hash())
        .collect();
    let mixed = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        Arc::new(block).prepare(),
        spent,
        DeferredPoolBalanceChange::zero(),
        &Network::Mainnet,
        root.chain_value_pools,
    )?;

    let assert_queries =
        |chain: &Chain,
         balances_and_received: [(i64, u64); 2],
         transactions: &[(TransactionLocation, transaction::Hash)]| {
            assert_legacy_indexes(chain);
            for (address, (balance, received)) in addresses.into_iter().zip(balances_and_received) {
                let selected = HashSet::from([address]);
                assert_eq!(
                    chain.partial_transparent_balance_change(&selected),
                    (balance.try_into().unwrap(), received),
                );
                assert_eq!(
                    chain
                        .partial_transparent_tx_ids(&selected, blocks[0].height..=height)
                        .into_iter()
                        .collect::<Vec<_>>(),
                    transactions,
                );
            }
        };
    assert_queries(
        &root,
        [(-signed_value, 0), (signed_value, value)],
        &expected_transactions[..1],
    );
    let mut chain = root.clone().push(mixed.clone())?;
    assert_queries(
        &chain,
        [(-5, value - 5), (5, 3 * value - 11)],
        &expected_transactions,
    );
    let snapshot = chain.clone();
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert_queries(
        &reverted,
        [(-signed_value, 0), (signed_value, value)],
        &expected_transactions[..1],
    );
    assert!(reverted.push(mixed)?.eq_internal_state(&snapshot));
    chain.pop_root();
    assert_queries(
        &chain,
        [
            (signed_value - 5, value - 5),
            (5 - signed_value, 2 * value - 11),
        ],
        &expected_transactions[1..],
    );
    chain.pop_root();
    assert_queries(&chain, [(0, 0); 2], &[]);
    assert_queries(
        &snapshot,
        [(-5, value - 5), (5, 3 * value - 11)],
        &expected_transactions,
    );
    Ok(())
}

#[test]
fn large_self_transfers_preserve_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[1], &[1, 1]])?;
    let root = chain.push(blocks[0].clone())?;
    let chain = root.clone().push(blocks[1].clone())?;
    let selected = HashSet::from([addresses[1]]);
    assert_eq!(
        chain.partial_transparent_balance_change(&selected).0,
        value.constrain::<NegativeAllowed>()?
    );
    let (created, spent) = chain.partial_transparent_utxo_changes(&selected);
    assert_eq!(created.len(), 3);
    assert_eq!(spent.len(), 2);
    assert_eq!(
        created
            .iter()
            .filter(|(location, _)| !spent.contains(location))
            .count(),
        1
    );
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(blocks[1].clone())?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn reverting_intra_block_transfers_preserves_non_finalized_indexes() -> Result<()> {
    let _init_guard = zebra_test::init();
    let (chain, blocks, _, _) = large_transfer_chain(&[&[1], &[0, 1]])?;
    let root = chain.push(blocks[0].clone())?;
    let chain = root.clone().push(blocks[1].clone())?;
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(blocks[1].clone())?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn finalizing_self_transfers_preserves_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[0, 0], &[1]])?;
    let mut chain = chain.push(blocks[0].clone())?.push(blocks[1].clone())?;
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, blocks[0].hash);
    let selected = HashSet::from([addresses[0]]);
    assert_eq!(
        chain.partial_transparent_balance_change(&selected).0,
        (Amount::<NegativeAllowed>::zero() - value.constrain::<NegativeAllowed>()?)?
    );
    let (created, spent) = chain.partial_transparent_utxo_changes(&selected);
    assert!(created.is_empty());
    assert_eq!(spent.len(), 1);
    let (finalized, _) = chain.pop_root();
    assert_eq!(finalized.hash, blocks[1].hash);
    assert_eq!(
        chain.partial_transparent_balance_change(&HashSet::from(addresses)),
        (Amount::<NegativeAllowed>::zero(), 0)
    );
    assert_eq!(chain.partial_transparent_transfers.len(), 0);
    Ok(())
}

#[test]
fn coinbase_fee_transfer_preserves_non_finalized_indexes() -> Result<()> {
    use std::collections::HashSet;
    use zebra_chain::amount::DeferredPoolBalanceChange;

    let _init_guard = zebra_test::init();
    let (chain, blocks, addresses, value) = large_transfer_chain(&[&[1], &[1]])?;
    let root = chain.push(blocks[0].clone())?;
    let mut block = blocks[1].block.as_ref().clone();
    block.transactions[0] = Arc::new(
        block.transactions[0]
            .as_ref()
            .clone()
            .with_transparent_outputs(vec![transparent::Output::new(value, addresses[1].script())]),
    );
    block.transactions[1] = Arc::new(
        block.transactions[1]
            .as_ref()
            .clone()
            .with_transparent_outputs(vec![transparent::Output::new(
                Amount::zero(),
                transparent::Script::new(&[]),
            )]),
    );
    Arc::make_mut(&mut block.header).merkle_root = block
        .transactions
        .iter()
        .map(|transaction| transaction.hash())
        .collect();
    assert_eq!(block.transaction_fees(&blocks[1].spent_outputs)?, value);
    let block = crate::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        Arc::new(block).prepare(),
        blocks[1].spent_outputs.as_ref().clone(),
        DeferredPoolBalanceChange::zero(),
        &Network::Mainnet,
        root.chain_value_pools,
    )?;
    assert_eq!(block.chain_value_pool_change, ValueBalance::zero());
    let chain = root.clone().push(block.clone())?;
    assert_eq!(
        chain
            .partial_transparent_balance_change(&HashSet::from([addresses[1]]))
            .0,
        value.constrain::<NegativeAllowed>()?
    );
    let reverted = chain.fork(blocks[0].hash).unwrap();
    assert!(reverted.eq_internal_state(&root));
    assert!(reverted.push(block)?.eq_internal_state(&chain));
    Ok(())
}

#[test]
fn contextual_maps_preserve_intra_block_spends() -> Result<()> {
    use crate::{ContextuallyVerifiedBlock, SemanticallyVerifiedBlock};
    use std::collections::HashMap;
    use zebra_chain::{
        amount::DeferredPoolBalanceChange,
        parameters::testnet::ConfiguredActivationHeights,
        transaction::{LockTime, Transaction},
    };

    let height = Height(1_000);
    let output = |value: u64| {
        transparent::Output::new(
            value.try_into().unwrap(),
            transparent::Script::new(&[0; 1024]),
        )
    };
    let coinbase = Arc::new(Transaction::test_v1(
        vec![transparent::Input::Coinbase {
            height,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![output(10)],
        LockTime::unlocked(),
    ));
    let mut transactions = vec![coinbase];
    let mut spent = HashMap::new();
    for value in [9, 8] {
        let previous = transactions.last().unwrap();
        let outpoint = transparent::OutPoint {
            hash: previous.hash(),
            index: 0,
        };
        spent.insert(
            outpoint,
            transparent::OrderedUtxo::new(
                previous.outputs()[0].clone(),
                height,
                transactions.len() - 1,
            ),
        );
        transactions.push(Arc::new(Transaction::test_v1(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            vec![output(value)],
            LockTime::unlocked(),
        )));
    }
    let block = Arc::new(Block {
        header: Arc::new(zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?),
        transactions,
    });
    let plain = transparent::utxos_from_ordered_utxos(spent.clone());
    let nu7 = Network::new_regtest(
        ConfiguredActivationHeights {
            nu7: Some(height.0),
            ..Default::default()
        }
        .into(),
    );
    for network in [Network::Mainnet, nu7] {
        let expected = block.chain_value_pool_change_and_fees(
            &plain,
            DeferredPoolBalanceChange::zero(),
            &network,
            ValueBalance::zero(),
        )?;
        let (contextual, fees) = ContextuallyVerifiedBlock::with_block_spent_utxos_and_fees(
            block.clone().prepare(),
            spent.clone(),
            DeferredPoolBalanceChange::zero(),
            &network,
            ValueBalance::zero(),
        )?;
        assert_eq!((contextual.chain_value_pool_change, fees), expected);
        assert_eq!(contextual.spent_outputs.as_ref(), &spent);
        assert_eq!(block.transaction_fees(&spent)?.zatoshis(), 2);
        let cloned = contextual.clone();
        assert_eq!(cloned, contextual);
        assert!(Arc::ptr_eq(&contextual.new_outputs, &cloned.new_outputs));
        assert!(Arc::ptr_eq(
            &contextual.spent_outputs,
            &cloned.spent_outputs
        ));
        let semantic = SemanticallyVerifiedBlock::from(cloned);
        assert_eq!(&semantic.new_outputs, contextual.new_outputs.as_ref());
        let unique = contextual;
        let expected_outputs = unique.new_outputs.as_ref().clone();
        assert_eq!(Arc::strong_count(&unique.new_outputs), 1);
        assert_eq!(
            SemanticallyVerifiedBlock::from(unique).new_outputs,
            expected_outputs
        );
    }
    Ok(())
}

#[test]
fn construct_many() -> Result<()> {
    let _init_guard = zebra_test::init();

    let mut block: Arc<Block> =
        zebra_test::vectors::BLOCK_MAINNET_434873_BYTES.zcash_deserialize_into()?;
    let initial_height = block
        .coinbase_height()
        .expect("Block 434873 should have its height in its coinbase tx.");
    let mut blocks = vec![];

    while blocks.len() < 100 {
        let next_block = block.make_fake_child();
        blocks.push(block);
        block = next_block;
    }

    let mut chain = Chain::new(
        &Network::Mainnet,
        (initial_height - 1).expect("Initial height should be at least 1."),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );

    for block in blocks {
        chain = chain.push(block.prepare().test_with_zero_spent_utxos())?;
    }

    assert_eq!(100, chain.blocks.len());

    Ok(())
}

#[test]
fn ord_matches_work() -> Result<()> {
    let _init_guard = zebra_test::init();
    let less_block = zebra_test::vectors::BLOCK_MAINNET_434873_BYTES
        .zcash_deserialize_into::<Arc<Block>>()?
        .set_work(1);
    let more_block = less_block.clone().set_work(10);

    let mut lesser_chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    lesser_chain = lesser_chain.push(less_block.prepare().test_with_zero_spent_utxos())?;

    let mut bigger_chain = Chain::new(
        &Network::Mainnet,
        Height(0),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::zero(),
    );
    bigger_chain = bigger_chain.push(more_block.prepare().test_with_zero_spent_utxos())?;

    assert!(bigger_chain > lesser_chain);

    Ok(())
}

#[test]
fn best_chain_wins() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        best_chain_wins_for_network(network)?;
    }

    Ok(())
}

fn best_chain_wins_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let expected_hash = block2.hash();

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    state.commit_new_chain(block2.prepare(), &finalized_state)?;
    state.commit_new_chain(child.prepare(), &finalized_state)?;

    let best_chain = state.best_chain().unwrap();
    assert!(best_chain.height_by_hash.contains_key(&expected_hash));

    Ok(())
}

#[test]
fn finalize_pops_from_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        finalize_pops_from_best_chain_for_network(network)?;
    }

    Ok(())
}

fn finalize_pops_from_best_chain_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child = block1.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(child.prepare(), &finalized_state)?;

    let finalized = state.finalize().inner_block();

    assert_eq!(block1, finalized);

    let finalized = state.finalize().inner_block();
    assert_eq!(block2, finalized);

    assert!(state.best_chain().is_none());

    Ok(())
}

#[test]
fn invalidate_block_removes_block_and_descendants_from_chain() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        invalidate_block_removes_block_and_descendants_from_chain_for_network(network)?;
    }

    Ok(())
}

fn invalidate_block_removes_block_and_descendants_from_chain_for_network(
    network: Network,
) -> Result<()> {
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(block3.clone().prepare(), &finalized_state)?;

    assert_eq!(
        state
            .best_chain()
            .unwrap_or(&Arc::new(Chain::default()))
            .blocks
            .len(),
        3
    );

    let _ = state.invalidate_block(block2.hash());

    let post_invalidated_chain = state.best_chain().unwrap();

    assert_eq!(post_invalidated_chain.blocks.len(), 1);
    assert!(
        post_invalidated_chain.contains_block_hash(block1.hash()),
        "the new modified chain should contain block1"
    );

    assert!(
        !post_invalidated_chain.contains_block_hash(block2.hash()),
        "the new modified chain should not contain block2"
    );
    assert!(
        !post_invalidated_chain.contains_block_hash(block3.hash()),
        "the new modified chain should not contain block3"
    );

    let invalidated_blocks_state = &state.invalidated_blocks;

    // Find an entry in the IndexMap that contains block2 hash
    let (_, invalidated_blocks_state_descendants) = invalidated_blocks_state
        .iter()
        .find_map(|(height, blocks)| {
            assert!(
                blocks.iter().any(|block| block.hash == block2.hash()),
                "invalidated_blocks should reference the hash of block2"
            );

            if blocks.iter().any(|block| block.hash == block2.hash()) {
                Some((height, blocks))
            } else {
                None
            }
        })
        .unwrap();

    match network {
        Network::Mainnet => assert!(
            invalidated_blocks_state_descendants
                .iter()
                .any(|block| block.height == block::Height(653601)),
            "invalidated descendants should contain block3"
        ),
        Network::Testnet(_parameters) => assert!(
            invalidated_blocks_state_descendants
                .iter()
                .any(|block| block.height == block::Height(584001)),
            "invalidated descendants should contain block3"
        ),
    }

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// Build an empty `NonFinalizedState` and an ephemeral `FinalizedState` with a
/// populated value pool — the shared setup for the invalidate/reconsider
/// regression tests below.
fn new_invalidate_test_state(network: &Network) -> (NonFinalizedState, FinalizedState) {
    let state = NonFinalizedState::new(network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");
    finalized_state.set_finalized_value_pool(ValueBalance::<NonNegative>::fake_populated_pool());
    (state, finalized_state)
}

#[test]
fn cached_proposal_respects_block_and_parent_invalidation() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let parent = Arc::new(network.test_block(653599, 583999).unwrap());
    let mut solved = parent.make_fake_child().set_work(10);
    Arc::make_mut(&mut Arc::make_mut(&mut solved).header).nonce = [1; 32].into();
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(parent.clone().prepare(), &finalized_state)
        .unwrap();

    // Retain the successful proposal payload before committing its solved counterpart.
    let mut proposal = (*solved).clone();
    Arc::make_mut(&mut proposal.header).nonce = [0; 32].into();
    let mut proposal_state = state.clone();
    proposal_state.disable_metrics();
    proposal_state
        .commit_block(Arc::new(proposal).prepare(), &finalized_state)
        .unwrap();
    let mut cached = proposal_state.best_tip_block().unwrap().clone();
    assert_ne!(cached.hash, solved.hash());
    cached.rebind(solved.clone(), std::time::Instant::now());
    state
        .commit_block(solved.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(solved.hash()).unwrap();

    let (committed, _commits) = tokio::sync::mpsc::channel(1);
    let (validated, bodies) = tokio::sync::watch::channel(None);
    let notifications = crate::MinedBlockNotifications::new(committed, validated);

    let normal_error = state
        .commit_block(solved.clone().prepare(), &finalized_state)
        .unwrap_err();
    assert_eq!(
        normal_error,
        crate::ValidateContextError::BlockPreviouslyInvalidated {
            block_hash: solved.hash(),
        }
    );
    assert_eq!(
        state.commit_prevalidated_contextual(
            cached.clone(),
            &finalized_state,
            Some(&notifications),
        ),
        Err(normal_error),
    );
    assert_eq!(state.best_tip().unwrap().1, parent.hash());
    assert!(!state.any_chain_contains(&solved.hash()));
    assert!(bodies.borrow().is_none());

    // A different solved hash is not itself invalidated, but cannot restore its removed parent.
    let mut sibling = (*solved).clone();
    Arc::make_mut(&mut sibling.header).nonce = [2; 32].into();
    let sibling = Arc::new(sibling);
    cached.rebind(sibling.clone(), std::time::Instant::now());
    // Keep another root live: invalidating the only non-finalized root is rejected.
    let other_root = parent.clone().set_work(20);
    state
        .commit_new_chain(other_root.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(parent.hash()).unwrap();
    let normal_error = state
        .commit_block(sibling.prepare(), &finalized_state)
        .unwrap_err();
    assert_eq!(
        normal_error,
        crate::ValidateContextError::NotReadyToBeCommitted
    );
    assert_eq!(
        state.commit_prevalidated_contextual(cached, &finalized_state, Some(&notifications)),
        Err(normal_error),
    );
    assert_eq!(state.best_tip().unwrap().1, other_root.hash());
    assert!(!state.any_chain_contains(&parent.hash()));
    assert!(bodies.borrow().is_none());
}

#[test]
fn compact_header_screening_respects_local_invalidation() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let parent = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = parent.make_fake_child();
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(parent.prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(child.hash()).unwrap();
    assert_eq!(
        crate::service::read::difficulty::candidate_header_context(
            &state,
            &finalized_state.db,
            &network,
            &child.header,
            child.coinbase_height().unwrap(),
            true,
        )
        .unwrap_err(),
        crate::ValidateContextError::BlockPreviouslyInvalidated {
            block_hash: child.hash()
        },
    );
}

/// Reject invalidating the only root before changing either live chains or invalidation records.
#[test]
fn invalidating_only_non_finalized_root_preserves_state() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let child = root.make_fake_child().set_work(10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    // Keep an existing invalidation record to check that rejection preserves it too.
    let grandchild = child.make_fake_child().set_work(1);
    state
        .commit_block(grandchild.clone().prepare(), &finalized_state)
        .unwrap();
    state.invalidate_block(grandchild.hash()).unwrap();
    let before = state.clone();

    let result = state.invalidate_block(root.hash());
    assert!(matches!(result, Err(InvalidateError::OnlyChainRoot(hash)) if hash == root.hash()));
    assert!(state.eq_internal_state(&before));
    assert_eq!(
        state.invalidated_blocks.len(),
        before.invalidated_blocks.len()
    );
    for (height, blocks) in &before.invalidated_blocks {
        assert!(Arc::ptr_eq(
            blocks,
            state.invalidated_blocks.get(height).unwrap()
        ));
    }
}

/// A root can still be invalidated when another chain remains available.
#[test]
fn invalidating_non_finalized_root_preserves_other_chain() {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let root: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let other_root = root.clone().set_work(20);
    let child = root.make_fake_child().set_work(10);
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state
        .commit_new_chain(root.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_block(child.clone().prepare(), &finalized_state)
        .unwrap();
    state
        .commit_new_chain(other_root.clone().prepare(), &finalized_state)
        .unwrap();

    assert_eq!(state.invalidate_block(root.hash()).unwrap(), root.hash());
    assert_eq!(state.chain_count(), 1);
    assert_eq!(state.best_tip_block().unwrap().hash, other_root.hash());
    assert!(!state.any_chain_contains(&root.hash()));
    assert!(!state.any_chain_contains(&child.hash()));
    assert_eq!(state.invalidated_blocks.values().next().unwrap().len(), 2);
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// Sequentially invalidating two same-height sibling fork tips with the same
/// parent previously panicked. The second invalidation produced a shortened
/// parent chain whose tip hash matched an existing entry, and `BTreeSet::insert`
/// reached `Chain::cmp`'s `unreachable!()`. After the fix, `Chain::cmp` returns
/// `Equal` for matching tip hashes, so the duplicate insert is a no-op and the
/// pre-existing parent chain is retained.
#[test]
fn invalidating_same_height_fork_tips_is_idempotent() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2a = block1.make_fake_child().set_work(10);
    let block2b = block1.make_fake_child().set_work(11);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

    state
        .commit_new_chain(block1.clone().prepare(), &finalized_state)
        .expect("fake root block should commit to an empty non-finalized state");
    state
        .commit_block(block2a.clone().prepare(), &finalized_state)
        .expect("first fork tip should extend the root chain");
    state
        .commit_block(block2b.clone().prepare(), &finalized_state)
        .expect("second fork tip should fork from the root chain");

    state
        .invalidate_block(block2a.hash())
        .expect("first fork tip should invalidate cleanly");
    state
        .invalidate_block(block2b.hash())
        .expect("second sibling fork tip should not panic on collapse to shared parent");

    let best_chain = state
        .best_chain()
        .expect("the parent chain should remain after both sibling tips are invalidated");
    assert!(best_chain.contains_block_hash(block1.hash()));
    assert!(!best_chain.contains_block_hash(block2a.hash()));
    assert!(!best_chain.contains_block_hash(block2b.hash()));
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// `reconsider_block` previously removed the invalidation record from a clone
/// of `invalidated_blocks` rather than from the live map. A second
/// `reconsider_block` for the same hash then replayed the same chain suffix
/// into a chain set that already contained the restored tip, panicking in
/// `Chain::cmp`'s duplicate-tip check. After the fix, the live entry is
/// removed and the second call returns `MissingInvalidatedBlock`.
#[test]
fn reconsider_block_removes_live_entry_and_second_call_returns_missing() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

    state
        .commit_new_chain(block1.prepare(), &finalized_state)
        .expect("fake root block should commit");
    state
        .commit_block(block2.clone().prepare(), &finalized_state)
        .expect("fake child should extend the root chain");
    state
        .commit_block(block3.prepare(), &finalized_state)
        .expect("fake grandchild should extend the child chain");

    state
        .invalidate_block(block2.hash())
        .expect("invalidating the child should succeed");

    state
        .reconsider_block(block2.hash(), &finalized_state.db)
        .expect("first reconsider should restore the invalidated chain");

    assert!(
        state.invalidated_blocks().values().all(|blocks| {
            blocks
                .first()
                .map(|block| block.hash != block2.hash())
                .unwrap_or(true)
        }),
        "first reconsider should remove the invalidated entry from the live map"
    );

    let second = state.reconsider_block(block2.hash(), &finalized_state.db);
    assert!(
        matches!(second, Err(ReconsiderError::MissingInvalidatedBlock(_))),
        "a second reconsider for the same hash should return MissingInvalidatedBlock; got {second:?}"
    );
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/10586.
///
/// `reconsider_block` must not destroy the invalidation record when a fallible
/// step fails. We invalidate child `B` (which has a grandchild), then invalidate
/// its parent/root `A`. Calling `reconsider_block(B)` now must fail with
/// `ParentChainNotFound` (because `A` is no longer in the non-finalized state)
/// without removing `B`'s record. A subsequent `reconsider_block(A)` must
/// restore `A`, after which the original `B` record remains available for a
/// later reconsider.
#[test]
fn reconsider_block_preserves_record_when_parent_chain_missing() {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let (mut state, finalized_state) = new_invalidate_test_state(&network);

    state
        .commit_new_chain(block1.clone().prepare(), &finalized_state)
        .expect("fake root should commit");
    state
        .commit_block(block2.clone().prepare(), &finalized_state)
        .expect("fake child should extend the root chain");
    state
        .commit_block(block3.prepare(), &finalized_state)
        .expect("fake grandchild should extend the child chain");

    // Keep another root live so invalidating block1 does not empty the state.
    state
        .commit_new_chain(block1.clone().set_work(20).prepare(), &finalized_state)
        .expect("independent root should commit");

    state
        .invalidate_block(block2.hash())
        .expect("invalidating the child should succeed");
    state
        .invalidate_block(block1.hash())
        .expect("invalidating the root should not panic and should succeed");

    // Now block2's parent (block1) is no longer in the non-finalized state, so
    // reconsidering block2 must fail. The record for block2 must survive the
    // failure so a later reconsider can still restore it.
    let result = state.reconsider_block(block2.hash(), &finalized_state.db);
    assert!(
        matches!(result, Err(ReconsiderError::ParentChainNotFound(_))),
        "reconsider with missing parent chain should fail, got {result:?}"
    );

    assert!(
        state.invalidated_blocks().values().any(|blocks| {
            blocks
                .first()
                .map(|block| block.hash == block2.hash())
                .unwrap_or(false)
        }),
        "a failed reconsider must not destroy the invalidation record"
    );
}

#[test]
fn reconsider_block_and_reconsider_chain_correctly_reconsiders_blocks_and_descendants() -> Result<()>
{
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        reconsider_block_inserts_block_and_descendants_into_chain_for_network(network.clone())?;
    }

    Ok(())
}

fn reconsider_block_inserts_block_and_descendants_into_chain_for_network(
    network: Network,
) -> Result<()> {
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    state.commit_block(block2.clone().prepare(), &finalized_state)?;
    state.commit_block(block3.clone().prepare(), &finalized_state)?;

    assert_eq!(
        state
            .best_chain()
            .unwrap_or(&Arc::new(Chain::default()))
            .blocks
            .len(),
        3
    );

    // Invalidate block2 to update the invalidated_blocks NonFinalizedState
    let _ = state.invalidate_block(block2.hash());

    // Perform checks to ensure the invalidated_block and descendants were added to the invalidated_block
    // state
    let post_invalidated_chain = state.best_chain().unwrap();

    assert_eq!(post_invalidated_chain.blocks.len(), 1);
    assert!(
        post_invalidated_chain.contains_block_hash(block1.hash()),
        "the new modified chain should contain block1"
    );

    assert!(
        !post_invalidated_chain.contains_block_hash(block2.hash()),
        "the new modified chain should not contain block2"
    );
    assert!(
        !post_invalidated_chain.contains_block_hash(block3.hash()),
        "the new modified chain should not contain block3"
    );

    // Reconsider block2 and check that both block2 and block3 were `reconsidered` into the
    // best chain
    state.reconsider_block(block2.hash(), &finalized_state.db)?;

    let best_chain = state.best_chain().unwrap();

    assert!(
        best_chain.contains_block_hash(block2.hash()),
        "the best chain should again contain block2"
    );
    assert!(
        best_chain.contains_block_hash(block3.hash()),
        "the best chain should again contain block3"
    );

    Ok(())
}

#[test]
// This test gives full coverage for `take_chain_if`
fn commit_block_extending_best_chain_doesnt_drop_worst_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(network)?;
    }

    Ok(())
}

fn commit_block_extending_best_chain_doesnt_drop_worst_chains_for_network(
    network: Network,
) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let block2 = block1.make_fake_child().set_work(10);
    let child1 = block1.make_fake_child().set_work(1);
    let child2 = block2.make_fake_child().set_work(1);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    assert_eq!(0, state.chain_set.len());
    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(block2.prepare(), &finalized_state)?;
    assert_eq!(1, state.chain_set.len());
    state.commit_block(child1.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());
    state.commit_block(child2.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    Ok(())
}

#[test]
fn shorter_chain_can_be_best_chain() -> Result<()> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        shorter_chain_can_be_best_chain_for_network(network)?;
    }
    Ok(())
}

fn shorter_chain_can_be_best_chain_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(Some(2), state.best_chain_len());

    Ok(())
}

#[test]
fn longer_chain_with_more_work_wins() -> Result<()> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        longer_chain_with_more_work_wins_for_network(network)?;
    }

    Ok(())
}

fn longer_chain_with_more_work_wins_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let long_chain_block1 = block1.make_fake_child().set_work(1);
    let long_chain_block2 = long_chain_block1.make_fake_child().set_work(1);
    let long_chain_block3 = long_chain_block2.make_fake_child().set_work(1);
    let long_chain_block4 = long_chain_block3.make_fake_child().set_work(1);

    let short_chain_block = block1.make_fake_child().set_work(3);

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block1.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block2.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block3.prepare(), &finalized_state)?;
    state.commit_block(long_chain_block4.prepare(), &finalized_state)?;
    state.commit_block(short_chain_block.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    assert_eq!(Some(5), state.best_chain_len());

    Ok(())
}

#[test]
fn equal_length_goes_to_more_work() -> Result<()> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        equal_length_goes_to_more_work_for_network(network)?;
    }

    Ok(())
}
fn equal_length_goes_to_more_work_for_network(network: Network) -> Result<()> {
    // Since the brand new FinalizedState below will pass a None history tree
    // to the NonFinalizedState, we must use pre-Heartwood blocks since
    // they won't trigger the history tree update in the NonFinalizedState.
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    let less_work_child = block1.make_fake_child().set_work(1);
    let more_work_child = block1.make_fake_child().set_work(3);
    let expected_hash = more_work_child.hash();

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let fake_value_pool = ValueBalance::<NonNegative>::fake_populated_pool();
    finalized_state.set_finalized_value_pool(fake_value_pool);

    state.commit_new_chain(block1.prepare(), &finalized_state)?;
    state.commit_block(less_work_child.prepare(), &finalized_state)?;
    state.commit_block(more_work_child.prepare(), &finalized_state)?;
    assert_eq!(2, state.chain_set.len());

    let tip_hash = state.best_tip().unwrap().1;
    assert_eq!(expected_hash, tip_hash);

    Ok(())
}

#[test]
fn history_tree_is_updated() -> Result<()> {
    for network in Network::iter() {
        history_tree_is_updated_for_network_upgrade(network, NetworkUpgrade::Heartwood)?;
    }
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
    Ok(())
}

fn history_tree_is_updated_for_network_upgrade(
    network: Network,
    network_upgrade: NetworkUpgrade,
) -> Result<()> {
    let blocks = network.block_map();

    let height = network_upgrade.activation_height(&network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    if network_upgrade == NetworkUpgrade::Heartwood {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_none(),
            "history tree must not exist yet"
        );
    } else {
        assert!(
            chain.history_block_commitment_tree().as_ref().is_some(),
            "history tree must already exist"
        );
    }

    // The Heartwood activation block has an all-zero commitment
    let activation_block = prev_block.make_fake_child().set_block_commitment([0u8; 32]);

    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    let chain = state.best_chain().unwrap();
    assert!(
        chain.history_block_commitment_tree().as_ref().is_some(),
        "history tree must have been (re)created"
    );
    assert_eq!(
        chain
            .history_block_commitment_tree()
            .as_ref()
            .as_ref()
            .unwrap()
            .size(),
        1,
        "history tree must have a single node"
    );

    // To fix the commitment in the next block we must recreate the history tree
    let tree = NonEmptyHistoryTree::from_block(
        &Network::Mainnet,
        activation_block.clone(),
        BlockCommitmentTreeRoots {
            sapling: &chain.sapling_note_commitment_tree_for_tip().root(),
            orchard: &chain.orchard_note_commitment_tree_for_tip().root(),
            ironwood: &chain.ironwood_note_commitment_tree_for_tip().root(),
        },
    )
    .unwrap();

    let next_block = activation_block
        .make_fake_child()
        .set_block_commitment(tree.hash().into());

    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();

    assert!(
        state
            .best_chain()
            .unwrap()
            .history_block_commitment_tree()
            .as_ref()
            .is_some(),
        "history tree must still exist"
    );

    Ok(())
}

#[test]
fn commitment_is_validated() {
    for network in Network::iter() {
        commitment_is_validated_for_network_upgrade(network, NetworkUpgrade::Heartwood);
    }
    // TODO: we can't test other upgrades until we have a method for creating a FinalizedState
    // with a HistoryTree.
}

fn commitment_is_validated_for_network_upgrade(network: Network, network_upgrade: NetworkUpgrade) {
    let blocks = network.block_map();
    let height = network_upgrade.activation_height(&network).unwrap().0;

    let prev_block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    let mut state = NonFinalizedState::new(&network);
    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    state
        .commit_new_chain(prev_block.clone().prepare(), &finalized_state)
        .unwrap();

    // The Heartwood activation block must have an all-zero commitment.
    // Test error return when committing the block with the wrong commitment
    let activation_block = prev_block.make_fake_child();
    let err = state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryActivationReserved { .. },
        ) => {},
        _ => panic!("Error must be InvalidBlockCommitment::InvalidChainHistoryActivationReserved instead of {err:?}"),
    };

    // Test committing the Heartwood activation block with the correct commitment
    let activation_block = activation_block.set_block_commitment([0u8; 32]);
    state
        .commit_block(activation_block.clone().prepare(), &finalized_state)
        .unwrap();

    // To fix the commitment in the next block we must recreate the history tree
    let chain = state.best_chain().unwrap();
    let tree = NonEmptyHistoryTree::from_block(
        &Network::Mainnet,
        activation_block.clone(),
        BlockCommitmentTreeRoots {
            sapling: &chain.sapling_note_commitment_tree_for_tip().root(),
            orchard: &chain.orchard_note_commitment_tree_for_tip().root(),
            ironwood: &chain.ironwood_note_commitment_tree_for_tip().root(),
        },
    )
    .unwrap();

    // Test committing the next block with the wrong commitment
    let next_block = activation_block.make_fake_child();
    let err = state
        .commit_block(next_block.clone().prepare(), &finalized_state)
        .unwrap_err();
    match err {
        crate::ValidateContextError::InvalidBlockCommitment(
            zebra_chain::block::CommitmentError::InvalidChainHistoryRoot { .. },
        ) => {}
        _ => panic!(
            "Error must be InvalidBlockCommitment::InvalidChainHistoryRoot instead of {err:?}"
        ),
    };

    // Test committing the next block with the correct commitment
    let next_block = next_block.set_block_commitment(tree.hash().into());
    state
        .commit_block(next_block.prepare(), &finalized_state)
        .unwrap();
}

#[tokio::test]
async fn non_finalized_state_writes_blocks_to_and_restores_blocks_from_backup_cache() {
    let network = Network::Mainnet;

    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )
    .expect("opening an ephemeral database should succeed");

    let backup_dir_path = tempfile::Builder::new()
        .prefix("zebra-non-finalized-state-backup-cache")
        .tempdir()
        .expect("temporary directory is created successfully")
        .keep();

    let (mut non_finalized_state, non_finalized_state_sender, _receiver) =
        NonFinalizedState::new(&network)
            .with_backup(
                Some(backup_dir_path.clone()),
                &finalized_state.db,
                false,
                false,
            )
            .await;

    let blocks = network.block_map();
    let height = NetworkUpgrade::Heartwood
        .activation_height(&network)
        .unwrap()
        .0;
    let block = Arc::new(
        blocks
            .get(&(height - 1))
            .expect("test vector exists")
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid"),
    );

    non_finalized_state
        .commit_new_chain(block.into(), &finalized_state.db)
        .expect("committing test block should succeed");

    non_finalized_state_sender
        .send(non_finalized_state.clone())
        .expect("backup task should have a receiver, channel should be open");

    // Poll restoration until the rate-limited writer has finished writing a readable backup.
    let non_finalized_state = tokio::time::timeout(
        MIN_DURATION_BETWEEN_BACKUP_UPDATES + Duration::from_secs(30),
        async {
            loop {
                let (restored_state, _sender, _receiver) = NonFinalizedState::new(&network)
                    .with_backup(
                        Some(backup_dir_path.clone()),
                        &finalized_state.db,
                        true,
                        // Only the original state should write backups while restoration is polled.
                        true,
                    )
                    .await;
                if restored_state.best_chain_len().is_some() {
                    break restored_state;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        },
    )
    .await
    .expect("the backup task should write a restorable non-finalized block");

    assert_eq!(
        non_finalized_state.best_chain_len(),
        Some(1),
        "non-finalized state should have restored the block committed \
        to the previous non-finalized state"
    );
}

/// Regression test for
/// [GHSA-2gf8-q9rr-jq3h](https://github.com/ZcashFoundation/zebra/security/advisories/GHSA-2gf8-q9rr-jq3h).
///
/// `Chain::pop_tip` used to leave entries in `sapling_subtrees` and `orchard_subtrees`,
/// so a chain forked below a subtree boundary inherited stale subtrees from the
/// abandoned branch. Forking should drop any subtree whose `end_height` is above the
/// new tip.
#[test]
fn fork_drops_subtrees_above_fork_point() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let block2 = block1.make_fake_child().set_work(10);
    let block3 = block2.make_fake_child().set_work(1);

    let mut chain = Chain::new(
        &network,
        (block1.coinbase_height().unwrap() - 1).unwrap(),
        NoteCommitmentTrees::default(),
        Default::default(),
        ValueBalance::fake_populated_pool(),
    );
    chain = chain.push(block1.clone().prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(block2.clone().prepare().test_with_zero_spent_utxos())?;
    chain = chain.push(block3.clone().prepare().test_with_zero_spent_utxos())?;

    // Inject a Sapling and an Orchard subtree whose `end_height` is the chain's tip.
    // The block-push pipeline above doesn't complete subtrees on its own for these
    // fixture blocks, so we install them directly via the test-only helpers.
    let tip_height = block3.coinbase_height().unwrap();
    let sapling_node = sapling_crypto::Node::from_bytes([0; 32]).unwrap();
    chain.insert_sapling_subtree(NoteCommitmentSubtree::new(0u16, tip_height, sapling_node));
    let orchard_node = orchard::tree::Node::default();
    chain.insert_orchard_subtree(NoteCommitmentSubtree::new(0u16, tip_height, orchard_node));

    assert_eq!(chain.sapling_subtrees.len(), 1);
    assert_eq!(chain.orchard_subtrees.len(), 1);

    // Fork at `block1`, so `pop_tip` runs twice (for block3 then block2). The
    // subtree at block3's height must be removed; without the fix, it would survive
    // into the forked chain.
    let forked = chain.fork(block1.hash()).expect("block1 is in the chain");

    assert_eq!(forked.non_finalized_tip_hash(), block1.hash());
    assert!(
        forked.sapling_subtrees.is_empty(),
        "fork should have dropped the Sapling subtree completed above the fork point"
    );
    assert!(
        forked.orchard_subtrees.is_empty(),
        "fork should have dropped the Orchard subtree completed above the fork point"
    );

    Ok(())
}

/// From the NSM reissuance height, the deferred (lockbox) funding stream is a
/// fraction of the whole block subsidy, including the additional subsidy that depends on the parent
/// block's chain value pools.
#[test]
fn deferred_pool_balance_change_uses_nsm_subsidy() -> Result<()> {
    use zebra_chain::parameters::{
        subsidy::{
            block_subsidy_with_parent_pools, funding_stream_values, scheduled_block_subsidy,
            FundingStreamReceiver,
        },
        testnet::{
            ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
            ConfiguredFundingStreams, RegtestParameters,
        },
    };

    let _init_guard = zebra_test::init();

    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(10),
            ..Default::default()
        },
        funding_streams: Some(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(100)),
            recipients: Some(vec![ConfiguredFundingStreamRecipient {
                receiver: FundingStreamReceiver::Deferred,
                numerator: 12,
                addresses: None,
            }]),
        }]),
        nsm_reissuance_height: Some(Height(10)),
        ..Default::default()
    });

    let deferred = |height, subsidy| {
        funding_stream_values(height, &network, subsidy)
            .unwrap()
            .remove(&FundingStreamReceiver::Deferred)
            .expect("the deferred funding stream is active")
    };

    let height = Height(12);
    let mut parent_pools = ValueBalance::<NonNegative>::zero();
    parent_pools.set_nsm_amount(Amount::try_from(1_000_000_000_i64)?);

    let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)?;
    let expected = deferred(height, subsidy);
    assert_ne!(
        expected,
        deferred(height, scheduled_block_subsidy(height, &network)?)
    );

    assert_eq!(
        calculate_deferred_pool_balance_change(height, &network, parent_pools)?.value(),
        expected.constrain::<zebra_chain::amount::NegativeAllowed>()?,
    );

    // Before activation the parent's chain value pools don't matter.
    let height = Height(9);
    assert_eq!(
        calculate_deferred_pool_balance_change(height, &network, parent_pools)?,
        calculate_deferred_pool_balance_change(height, &network, ValueBalance::zero())?,
    );

    Ok(())
}

/// Committed blocks track the NSM value balance: the NU7 activation block seeds it, blocks before
/// the NSM reissuance height leave it unchanged, and from the deployment height each block
/// reissues from the balance after its parent. A block paying the scheduled subsidy instead is
/// rejected from the deployment height.
///
/// Each block after activation credits the balance with the fees it removes from circulation.
/// From the deployment height a block whose coinbase claims those fees is rejected.
#[test]
fn nsm_subsidy_tracks_the_nsm_value_balance() -> Result<()> {
    use chrono::Duration;
    use zebra_chain::{
        block::{
            ChainHistoryBlockTxAuthCommitmentHash, ChainHistoryMmrRootHash,
            CHAIN_HISTORY_ACTIVATION_RESERVED,
        },
        parameters::{
            subsidy::{
                additional_block_subsidy, block_subsidy_with_parent_pools, nsm_fee_contribution,
                scheduled_block_subsidy, CoinbaseTransactionError, SubsidyError,
            },
            testnet::{ConfiguredActivationHeights, RegtestParameters},
        },
        transaction::{self, LockTime, Transaction},
    };

    use crate::ValidateContextError;

    let _init_guard = zebra_test::init();

    // Every upgrade from Heartwood activates together, so the activation block's parent is
    // pre-Heartwood and its block commitment only needs the reserved chain history root.
    const ACTIVATION_HEIGHT: u32 = 11;
    const DEPLOYMENT_HEIGHT: u32 = ACTIVATION_HEIGHT + 2;
    const INITIAL_NSM_VALUE_BALANCE: i64 = 1_000_000_000;
    // Each block after activation spends two outputs of `FUNDING_VALUE`, paying these fees. ZIP 235
    // rounds the fees it removes once per block: `floor(1_002 * 6 / 10)` is 601, but rounding each
    // transaction's fee would remove 600.
    const FUNDING_VALUE: i64 = 10_000;
    const FEES: [i64; 2] = [1_001, 1];

    let zats = |zats: i64| Amount::<NonNegative>::try_from(zats).expect("valid amount");

    let network = Network::new_regtest(RegtestParameters {
        activation_heights: ConfiguredActivationHeights {
            sapling: Some(1),
            blossom: Some(1),
            heartwood: Some(ACTIVATION_HEIGHT),
            canopy: Some(ACTIVATION_HEIGHT),
            nu5: Some(ACTIVATION_HEIGHT),
            nu6: Some(ACTIVATION_HEIGHT),
            nu6_3: Some(ACTIVATION_HEIGHT),
            nu7: Some(ACTIVATION_HEIGHT),
            ..Default::default()
        },
        funding_streams: Some(Vec::new()),
        initial_nsm_value_balance: Some(zats(INITIAL_NSM_VALUE_BALANCE)),
        nsm_reissuance_height: Some(Height(DEPLOYMENT_HEIGHT)),
        ..Default::default()
    });

    // The outputs that the transactions in the block at `height` spend. They aren't created by any
    // transaction, so the test adds them to the outputs of the block that spends them.
    let funding_outpoints = |height: Height| {
        (0..FEES.len() as u32).map(move |index| transparent::OutPoint {
            hash: transaction::Hash([height.0 as u8; 32]),
            index,
        })
    };
    let funding_output = || transparent::Output {
        value: zats(FUNDING_VALUE),
        lock_script: transparent::Script::new(&[]),
    };
    let prepare_with_funding = |block: Arc<Block>, height: Height| {
        let mut prepared = block.prepare();
        prepared
            .new_outputs
            .extend(funding_outpoints(height).map(|outpoint| {
                let utxo = transparent::Utxo::new(funding_output(), height, false);
                (outpoint, transparent::OrderedUtxo::from_utxo(utxo, 0))
            }));
        prepared
    };

    // The transactions in the block at `height`, which spend that block's funding outputs and pay
    // `FEES`.
    let spends = |height: Height| -> Vec<Arc<Transaction>> {
        funding_outpoints(height)
            .zip(FEES)
            .map(|(outpoint, fee)| {
                Arc::new(Transaction::test_v4(
                    vec![transparent::Input::PrevOut {
                        outpoint,
                        unlock_script: transparent::Script::new(&[]),
                        sequence: u32::MAX,
                    }],
                    vec![transparent::Output {
                        value: zats(FUNDING_VALUE - fee),
                        lock_script: transparent::Script::new(&[]),
                    }],
                    LockTime::unlocked(),
                    height,
                ))
            })
            .collect()
    };
    let fees = zats(FEES.iter().sum());

    // A block at `height` whose coinbase pays `coinbase_value`, followed by `transactions`. Block
    // times increase with height, so each block's time is after the median time of the blocks
    // before it.
    let block = |height: Height,
                 previous_block_hash,
                 coinbase_value,
                 transactions: Vec<Arc<Transaction>>|
     -> Arc<Block> {
        let mut block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Block>()
            .expect("block should deserialize");

        let header = Arc::make_mut(&mut block.header);
        header.time += Duration::minutes(height.0.into());

        block.transactions = vec![Arc::new(Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output {
                value: coinbase_value,
                lock_script: transparent::Script::new(&[]),
            }],
            LockTime::unlocked(),
            height,
        ))];
        block.transactions.extend(transactions);
        Arc::make_mut(&mut block.header).previous_block_hash = previous_block_hash;

        Arc::new(block)
    };

    // A block at `height` whose coinbase pays `coinbase_value`, followed by `transactions`,
    // committing to `history_root`.
    let committed_block = |height: Height,
                           previous_block_hash,
                           coinbase_value,
                           transactions,
                           history_root: &ChainHistoryMmrRootHash| {
        let child = block(height, previous_block_hash, coinbase_value, transactions);
        let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
            history_root,
            &child.auth_data_root(),
        );

        child.set_block_commitment(commitment.into())
    };

    // A block at `height` paying the ZIP's subsidy for `parent_pools`, and the part of `fees` that
    // ZIP 235 leaves to the miner, committing to `history_root`.
    let subsidy_block = |height: Height,
                         previous_block_hash,
                         parent_pools,
                         fees: Amount<NonNegative>,
                         transactions,
                         history_root: &_| {
        let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)
            .expect("the block subsidy is valid");
        let miner_fees = (fees - nsm_fee_contribution(height, &network, fees))
            .expect("the contribution is at most the fees");

        committed_block(
            height,
            previous_block_hash,
            (subsidy + miner_fees).expect("valid amount"),
            transactions,
            history_root,
        )
    };

    let finalized_state = FinalizedState::new(
        &Config::ephemeral(),
        &network,
        #[cfg(feature = "elasticsearch")]
        false,
    )?;

    let parent_height = Height(ACTIVATION_HEIGHT - 1);
    let activation = Height(ACTIVATION_HEIGHT);

    // The block before activation pays the scheduled subsidy, and leaves the NSM value balance empty.
    let parent = block(
        parent_height,
        block::Hash([1; 32]),
        scheduled_block_subsidy(parent_height, &network)?,
        Vec::new(),
    );

    let mut state = NonFinalizedState::new(&network);
    state.commit_new_chain(parent.clone().prepare(), &finalized_state)?;
    let pools_before_activation = state
        .best_chain()
        .expect("the chain was just committed")
        .chain_value_pools;
    assert_eq!(pools_before_activation.nsm_amount(), zats(0));

    // The activation block seeds the balance, and pays only the scheduled subsidy because the
    // deployment height is later.
    let initial = zats(INITIAL_NSM_VALUE_BALANCE);
    let activation_block = subsidy_block(
        activation,
        parent.hash(),
        pools_before_activation,
        zats(0),
        Vec::new(),
        &CHAIN_HISTORY_ACTIVATION_RESERVED.into(),
    );
    assert_eq!(
        activation_block.transactions[0].outputs()[0].value(),
        scheduled_block_subsidy(activation, &network)?,
    );
    state.commit_block(activation_block.clone().prepare(), &finalized_state)?;

    let chain = state
        .best_chain()
        .expect("the activation block was committed");
    assert_eq!(chain.chain_value_pools.nsm_amount(), initial);

    // Later blocks commit to the chain history tree, which starts at the activation block. The
    // blocks have no shielded transactions, so the note commitment tree roots never change.
    let sapling_root = chain.sapling_note_commitment_tree_for_tip().root();
    let orchard_root = chain.orchard_note_commitment_tree_for_tip().root();
    let ironwood_root = chain.ironwood_note_commitment_tree_for_tip().root();
    let roots = || BlockCommitmentTreeRoots {
        sapling: &sapling_root,
        orchard: &orchard_root,
        ironwood: &ironwood_root,
    };
    let mut tree = NonEmptyHistoryTree::from_block(&network, activation_block, roots())?;

    let mut previous = state.best_tip().expect("the chain has a tip");
    for height in (ACTIVATION_HEIGHT + 1..=DEPLOYMENT_HEIGHT + 2).map(Height) {
        let parent_pools = state
            .best_chain()
            .expect("the chain has blocks")
            .chain_value_pools;
        let reissued = additional_block_subsidy(height, &network, parent_pools.nsm_amount());
        let contributed = nsm_fee_contribution(height, &network, fees);
        let miner_fees = (fees - contributed)?;
        assert_eq!(contributed, zats(601));

        if height < Height(DEPLOYMENT_HEIGHT) {
            assert_eq!(
                reissued,
                zats(0),
                "no reissuance before the deployment height"
            );
        } else {
            assert!(reissued > zats(0), "reissuance from the deployment height");

            // From the deployment height, a block paying only the scheduled subsidy is rejected,
            // and so is one claiming the fees that ZIP 235 removes from circulation.
            let subsidy = block_subsidy_with_parent_pools(height, &network, parent_pools)?;
            let mut invalid_coinbase_values =
                vec![(scheduled_block_subsidy(height, &network)? + miner_fees)?];
            if !contributed.is_zero() {
                invalid_coinbase_values.push((subsidy + fees)?);
            }

            for coinbase_value in invalid_coinbase_values {
                let invalid = committed_block(
                    height,
                    previous.1,
                    coinbase_value,
                    spends(height),
                    &tree.hash(),
                );
                assert!(matches!(
                    state.commit_block(prepare_with_funding(invalid, height), &finalized_state),
                    Err(ValidateContextError::InvalidSubsidy {
                        subsidy_error: CoinbaseTransactionError::Subsidy(
                            SubsidyError::InvalidMinerFees
                        ),
                        ..
                    })
                ));
            }
        }

        let next_block = subsidy_block(
            height,
            previous.1,
            parent_pools,
            fees,
            spends(height),
            &tree.hash(),
        );
        state.commit_block(
            prepare_with_funding(next_block.clone(), height),
            &finalized_state,
        )?;

        // The block debits exactly what it reissued, and credits the fees it removed from
        // circulation.
        let chain = state.best_chain().expect("the block was committed");
        assert_eq!(
            chain.chain_value_pools.nsm_amount(),
            ((parent_pools.nsm_amount() - reissued)? + contributed)?,
        );

        tree.push(next_block, roots())?;
        previous = state.best_tip().expect("the chain has a tip");
    }

    Ok(())
}

/// Regression test for the equal-work tie-break in `Chain::cmp`.
///
/// Sibling blocks always have equal work on Zcash (`nBits` is fully determined by their
/// ancestors), and the spec breaks equal-work ties by preferring the block received
/// first. Commits two equal-work siblings with controlled receipt times in both receipt
/// orders and checks that the first-received sibling stays best both times; one of the
/// two orders fails under hash tie-breaking by construction. Then extends the losing
/// sibling and checks that strictly more cumulative work still overrides receipt order.
#[test]
fn equal_work_ties_prefer_first_received() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());

    // Same parent and work, different commitment bytes: the siblings tie on cumulative
    // work with distinct hashes.
    let sibling_a = block1
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([0x01; 32]);
    let sibling_b = block1
        .make_fake_child()
        .set_work(10)
        .set_block_commitment([0x02; 32]);
    assert_ne!(
        sibling_a.hash(),
        sibling_b.hash(),
        "siblings must have distinct hashes"
    );

    for (first, second) in [(&sibling_a, &sibling_b), (&sibling_b, &sibling_a)] {
        let (mut state, finalized_state) = new_invalidate_test_state(&network);

        state
            .commit_new_chain(block1.clone().prepare(), &finalized_state)
            .expect("fake root block should commit to an empty non-finalized state");

        // Control the verifier receipt stamp so construction order can't leak in:
        // `first` was received before `second`, regardless of commit order below.
        let earlier = std::time::Instant::now();
        let later = earlier + Duration::from_secs(1);

        let mut first_prepared = (*first).clone().prepare();
        first_prepared.received_time = Some(earlier);
        let mut second_prepared = (*second).clone().prepare();
        second_prepared.received_time = Some(later);

        state
            .commit_block(first_prepared, &finalized_state)
            .expect("first sibling should commit");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            first.hash(),
            "the first-received sibling is adopted as the best tip"
        );

        state
            .commit_block(second_prepared, &finalized_state)
            .expect("second sibling should commit");
        assert_eq!(2, state.chain_set.len(), "both sibling chains are tracked");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            first.hash(),
            "a later equal-work sibling must not displace the adopted tip"
        );

        // Strictly more cumulative work still overrides receipt order.
        let second_child = second.make_fake_child().set_work(10);
        state
            .commit_block(second_child.clone().prepare(), &finalized_state)
            .expect("child of the second sibling should commit");
        assert_eq!(
            state.best_chain().unwrap().non_finalized_tip_hash(),
            second_child.hash(),
            "more cumulative work must override receipt order"
        );
    }

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// At the fork limit, a newly inserted chain is kept even if it has the lowest work, and
/// equal-work ties evict the earliest-received chain other than the best chain. So a late
/// sibling survives both its own insertion and the insertion of another sibling after it.
#[test]
fn fork_limit_keeps_new_chains_and_evicts_earliest_received_ties() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;

    let received = std::time::Instant::now();
    let mut commit = |block: &Arc<Block>, secs: u64| {
        let mut prepared = block.clone().prepare();
        prepared.received_time = Some(received + Duration::from_secs(secs));
        state.commit_block(prepared, &finalized_state)
    };
    let sibling = |i: u8| {
        block1
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32])
    };

    let max_forks = u8::try_from(crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .expect("the fork limit fits in a u8");
    let siblings: Vec<_> = (0..=max_forks + 1).map(sibling).collect();
    for (secs, block) in (0..).zip(&siblings) {
        commit(block, secs)?;
    }
    let late = &siblings[usize::from(max_forks)];

    // A chain with strictly less work than all the others.
    let low = block1.make_fake_child().set_work(1);
    commit(&low, u64::from(max_forks) + 2)?;

    let late_child = late.make_fake_child().set_work(10);
    commit(&late_child, u64::from(max_forks) + 3)?;

    assert_eq!(
        crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        state.chain_count()
    );
    for (i, block) in siblings.iter().enumerate() {
        assert_eq!(
            !(1..=3).contains(&i),
            state.any_chain_contains(&block.hash()),
            "only the earliest-received siblings after the best one are evicted, sibling {i}",
        );
    }
    assert!(state.any_chain_contains(&low.hash()));
    assert_eq!(
        state.best_chain().unwrap().non_finalized_tip_hash(),
        late_child.hash(),
    );

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// After a restart that restores 10 equal-work forks from the non-finalized backup, a live
/// sibling is kept at the fork limit, and its child becomes the best chain tip. Restored
/// blocks have no receipt time, so they rank as received first.
#[test]
fn fork_limit_keeps_live_sibling_after_backup_restore() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    let sibling = |i: u8| {
        block1
            .make_fake_child()
            .set_work(10)
            .set_block_commitment([i; 32])
    };

    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    let max_forks = u8::try_from(crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS)
        .expect("the fork limit fits in a u8");
    for i in 0..max_forks {
        state.commit_block(sibling(i).prepare(), &finalized_state)?;
    }

    let backup_dir = tempfile::tempdir()?;
    state.write_to_backup(backup_dir.path());
    let mut state = super::super::backup::restore_backup(
        NonFinalizedState::new(&network),
        backup_dir.path(),
        &finalized_state.db,
    );
    assert_eq!(
        crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        state.chain_count()
    );
    assert!(state
        .chain_iter()
        .all(|chain| chain.tip_block().unwrap().received_time.is_none()));

    let live = sibling(max_forks);
    let mut prepared = live.clone().prepare();
    prepared.received_time = Some(std::time::Instant::now());
    state.commit_block(prepared, &finalized_state)?;
    assert!(state.any_chain_contains(&live.hash()));

    let live_child = live.make_fake_child().set_work(10);
    state.commit_block(live_child.clone().prepare(), &finalized_state)?;
    assert_eq!(
        state.best_chain().unwrap().non_finalized_tip_hash(),
        live_child.hash(),
    );

    Ok(())
}

/// Regression test for https://github.com/ZcashFoundation/zebra/issues/11133.
///
/// Test networks keep more equal-work sibling forks than Mainnet before evicting any.
#[test]
fn fork_limit_is_higher_on_test_networks() -> Result<()> {
    let _init_guard = zebra_test::init();

    let siblings = 30u8;
    for (network, expected_forks) in [
        (
            Network::Mainnet,
            crate::constants::MAX_NON_FINALIZED_CHAIN_FORKS,
        ),
        (Network::new_default_testnet(), usize::from(siblings)),
    ] {
        let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
        let (mut state, finalized_state) = new_invalidate_test_state(&network);
        state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;

        for i in 0..siblings {
            let sibling = block1
                .make_fake_child()
                .set_work(10)
                .set_block_commitment([i; 32]);
            state.commit_block(sibling.prepare(), &finalized_state)?;
        }

        assert_eq!(expected_forks, state.chain_count(), "{network}");
    }

    Ok(())
}

/// The backup task writes each block once, even when several chains share it.
#[test]
fn backup_update_skips_blocks_shared_by_chains() -> Result<()> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let block1: Arc<Block> = Arc::new(network.test_block(653599, 583999).unwrap());
    let (mut state, finalized_state) = new_invalidate_test_state(&network);
    state.commit_new_chain(block1.clone().prepare(), &finalized_state)?;
    for i in 0..3u8 {
        let sibling = block1.make_fake_child().set_block_commitment([i; 32]);
        state.commit_block(sibling.prepare(), &finalized_state)?;
    }
    assert_eq!(3, state.chain_count());

    // Claim every block is already backed up, so nothing should be written.
    let backup_dir = tempfile::tempdir()?;
    let backup_blocks = state
        .chain_iter()
        .flat_map(|chain| chain.blocks.values())
        .map(|block| (block.hash, backup_dir.path().join("unused")))
        .collect();
    super::super::backup::update_non_finalized_state_backup(
        backup_dir.path(),
        &state,
        backup_blocks,
    );

    assert_eq!(0, std::fs::read_dir(backup_dir.path())?.count());

    Ok(())
}
