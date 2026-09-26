//! Fixed test vectors for state contextual validation checks.

use zebra_chain::serialization::ZcashDeserializeInto;

use super::super::*;

#[test]
fn test_orphan_consensus_check() {
    let _init_guard = zebra_test::init();

    let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap()
        .coinbase_height()
        .unwrap();

    block_is_not_orphaned(block::Height(0), height).expect("tip is lower so it should be fine");
    block_is_not_orphaned(block::Height(347498), height)
        .expect("tip is lower so it should be fine");
    block_is_not_orphaned(block::Height(347499), height)
        .expect_err("tip is equal so it should error");
    block_is_not_orphaned(block::Height(500000), height)
        .expect_err("tip is higher so it should error");
}

#[test]
fn test_sequential_height_check() {
    let _init_guard = zebra_test::init();

    let height = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap()
        .coinbase_height()
        .unwrap();

    height_one_more_than_parent_height(block::Height(0), height)
        .expect_err("block is much lower, should panic");
    height_one_more_than_parent_height(block::Height(347497), height)
        .expect_err("parent height is 2 less, should panic");
    height_one_more_than_parent_height(block::Height(347498), height)
        .expect("parent height is 1 less, should be good");
    height_one_more_than_parent_height(block::Height(347499), height)
        .expect_err("parent height is equal, should panic");
    height_one_more_than_parent_height(block::Height(347500), height)
        .expect_err("parent height is way more, should panic");
    height_one_more_than_parent_height(block::Height(500000), height)
        .expect_err("parent height is way more, should panic");
}

/// A valid NU6.3 block carrying a non-empty Ironwood bundle must pass the authorizing data
/// commitment check, and the same block with one authorizing-only byte changed must fail it
/// with the error the forged-body scoring and the syncer's re-request key off.
///
/// Both directions matter for this change:
///
/// - the honest Ironwood block is accepted unchanged, so the classification has no
///   consensus-side effect, even on the newest shielded pool, and
/// - the forged body — same block hash, different authorizing data — is caught, classified as
///   a forgery, and scored at the ban threshold.
#[test]
#[cfg(feature = "proptest-impl")]
fn ironwood_block_auth_commitment_accepts_honest_body_and_detects_a_forgery() {
    use zebra_chain::{
        primitives::zcash_history::BlockCommitmentTreeRoots,
        serialization::BytesInDisplayOrder as _, transaction::Transaction, transparent,
        LedgerState,
    };
    use zebra_test::prelude::{
        prop::{strategy::ValueTree as _, test_runner::TestRunner},
        *,
    };

    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let nu6_3_height = NetworkUpgrade::Nu6_3
        .activation_height(&network)
        .expect("Mainnet has an NU6.3 activation height");
    let heartwood_height = NetworkUpgrade::Heartwood
        .activation_height(&network)
        .expect("Mainnet has a Heartwood activation height");

    let mut runner = TestRunner::deterministic();
    let mut generate = |height, network_upgrade, transaction_version| {
        LedgerState::height_strategy(height, network_upgrade, transaction_version, false)
            .prop_flat_map(Block::arbitrary_with)
            .new_tree(&mut runner)
            .expect("the block strategy must generate a block")
            .current()
    };

    // The check only reads `history_tree.hash()`, so any Heartwood-onward tree works, as long
    // as the block's commitment is computed against that same root.
    let history_tree = HistoryTree::from_block(
        &network,
        Arc::new(generate(heartwood_height, NetworkUpgrade::Heartwood, None)),
        // The roots only have to be consistent between the tree and the commitment below.
        BlockCommitmentTreeRoots {
            sapling: &Default::default(),
            orchard: &Default::default(),
            ironwood: &Default::default(),
        },
    )
    .expect("a Heartwood block must seed a history tree");
    let history_tree_root = history_tree
        .hash()
        .expect("a tree seeded from a Heartwood block has a root");

    // v6 transactions carry optional Ironwood bundles, so generate until one is non-empty:
    // an empty-bundle block would make this guard vacuous.
    let has_ironwood_bundle = |block: &Block| {
        block
            .transactions
            .iter()
            .any(|tx| tx.ironwood_note_commitments().next().is_some())
    };
    let mut block = (0..100)
        .map(|_| generate(nu6_3_height, NetworkUpgrade::Nu6_3, Some(6)))
        .find(has_ironwood_bundle)
        .expect("the NU6.3 block strategy must generate a non-empty Ironwood bundle");

    // Set the commitment the honest body has, the same way a block builder does.
    let commitment = ChainHistoryBlockTxAuthCommitmentHash::from_commitments(
        &history_tree_root,
        &block.auth_data_root(),
    );
    Arc::make_mut(&mut block.header).commitment_bytes =
        commitment.bytes_in_serialized_order().into();

    block_commitment_is_valid_for_chain_history(Arc::new(block.clone()), &network, &history_tree)
        .expect("a valid NU6.3 block with an Ironwood bundle must pass the commitment check");

    // Change one authorizing-only byte. Under ZIP-244 the coinbase scriptSig is excluded from
    // the txid, so this leaves the block hash unchanged: exactly the body an unauthenticated
    // peer can serve under a canonical header.
    let mut forged = block.clone();
    let coinbase: &mut Transaction = Arc::make_mut(
        forged
            .transactions
            .first_mut()
            .expect("generated blocks have a coinbase transaction"),
    );
    let mut inputs = coinbase.inputs();
    let transparent::Input::Coinbase { data, .. } = inputs
        .first_mut()
        .expect("coinbase transactions have a transparent input")
    else {
        panic!("the first coinbase transaction input must be a coinbase input");
    };
    // Append to the end, so the coinbase height at the start of the scriptSig is unchanged.
    data.push(0x5a);
    *coinbase = coinbase.clone().with_transparent_inputs(inputs);

    assert_eq!(
        forged.hash(),
        block.hash(),
        "changing a coinbase scriptSig must not change the block header hash"
    );

    let error =
        block_commitment_is_valid_for_chain_history(Arc::new(forged), &network, &history_tree)
            .expect_err("a body that doesn't match its header commitment must be rejected");

    assert!(
        error.is_auth_commitment_mismatch(),
        "a forged Ironwood body must be classified as an authorizing data commitment mismatch: \
         got {error:?}"
    );
    assert_eq!(
        error.misbehavior_score(),
        100,
        "a forged Ironwood body must score the serving peer at the ban threshold"
    );
}

/// Reserve-funded payout checks start at reissuance, not at NU7, and use the exact parent.
#[test]
fn reserve_funded_payouts_follow_reissuance_and_parent() -> Result<(), BoxError> {
    use std::collections::HashMap;

    use zebra_chain::{
        amount::DeferredPoolBalanceChange,
        parameters::{
            subsidy::FundingStreamReceiver,
            testnet::{
                ConfiguredActivationHeights, ConfiguredFundingStreamRecipient,
                ConfiguredFundingStreams, Parameters,
            },
        },
        transaction::{Hash as TransactionHash, LockTime, Transaction},
        transparent,
        value_balance::ValueBalance,
    };

    use crate::service::finalized_state::calculate_deferred_pool_balance_change;

    let reissuance = block::Height(1_001);
    let network = Parameters::build()
        .with_slow_start_interval(block::Height(0))
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(1_000),
            ..Default::default()
        })?
        .with_nsm_reissuance_height(Some(reissuance))
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(block::Height(1_000)..block::Height(1_010)),
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
    let reserve = Amount::<NonNegative>::try_from(10_000_000_000u64)?;
    let mut parent_pools = ValueBalance::zero();
    parent_pools.set_nsm_reserve_amount(reserve);
    let header = zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
    let outpoint = transparent::OutPoint::from_usize(TransactionHash([0; 32]), 0);
    let utxos = HashMap::from([(
        outpoint,
        transparent::Utxo::new(
            transparent::Output::new(100.try_into()?, transparent::Script::new(&[])),
            block::Height(999),
            false,
        ),
    )]);
    let spend = Arc::new(Transaction::test_v1(
        vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[]),
            sequence: 0,
        }],
        vec![transparent::Output::new(
            89.try_into()?,
            transparent::Script::new(&[]),
        )],
        LockTime::unlocked(),
    ));
    let make_block = |height, miner, grant| Block {
        header: Arc::new(header),
        transactions: vec![
            Arc::new(Transaction::test_v1(
                vec![transparent::Input::Coinbase {
                    height,
                    data: vec![0],
                    sequence: u32::MAX,
                }],
                vec![
                    transparent::Output::new(miner, transparent::Script::new(&[])),
                    transparent::Output::new(
                        grant,
                        subsidy::funding_stream_address(
                            height,
                            &network,
                            FundingStreamReceiver::MajorGrants,
                        )
                        .expect("the funding stream has a configured address")
                        .script(),
                    ),
                ],
                LockTime::unlocked(),
            )),
            spend.clone(),
        ],
    };

    for height in [block::Height(1_000), reissuance, block::Height(1_002)] {
        let total = subsidy::block_subsidy(height, &network, reserve)?;
        let funding = subsidy::funding_stream_values(height, &network, total)?;
        let grant = funding[&FundingStreamReceiver::MajorGrants];
        let deferred = funding[&FundingStreamReceiver::Deferred];
        let deferred_change = calculate_deferred_pool_balance_change(height, &network, reserve)?;
        assert_eq!(
            deferred_change,
            DeferredPoolBalanceChange::new(deferred.constrain()?),
        );
        // Of the 11 zatoshi in gross fees, 6 go to the reserve and 5 to the miner.
        let miner = (total - grant - deferred + Amount::try_from(5)?)?;
        for delta in [-1, 0, 1] {
            let block = make_block(height, (miner.zatoshis() + delta).try_into()?, grant);
            let (_, fees) = block.chain_value_pool_change_and_fees(
                &utxos,
                deferred_change,
                &network,
                parent_pools,
            )?;
            let result = reserve_funded_subsidy_is_valid(&block, height, &network, reserve, fees);
            if height < reissuance || delta == 0 {
                result?;
            } else {
                assert!(matches!(
                    result,
                    Err(ValidateContextError::Subsidy(
                        SubsidyError::InvalidMinerFees
                    )),
                ));
            }

            if height >= reissuance && delta == 0 {
                assert!(matches!(
                    reserve_funded_subsidy_is_valid(&block, height, &network, (reserve * 2)?, fees,),
                    Err(ValidateContextError::Subsidy(
                        SubsidyError::FundingStreamNotFound
                    )),
                ));
            }
        }

        if height >= reissuance {
            // Preserve the total payout, but omit the reserve-funded part of the grant.
            let scheduled_grant = subsidy::funding_stream_values(
                height,
                &network,
                subsidy::scheduled_block_subsidy(height, &network)?,
            )?[&FundingStreamReceiver::MajorGrants];
            let block = make_block(height, (miner + grant - scheduled_grant)?, scheduled_grant);
            let (_, fees) = block.chain_value_pool_change_and_fees(
                &utxos,
                deferred_change,
                &network,
                parent_pools,
            )?;
            assert!(matches!(
                reserve_funded_subsidy_is_valid(&block, height, &network, reserve, fees),
                Err(ValidateContextError::Subsidy(
                    SubsidyError::FundingStreamNotFound
                )),
            ));
        }
    }

    Ok(())
}
