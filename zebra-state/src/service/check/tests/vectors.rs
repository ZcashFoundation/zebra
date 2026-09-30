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

/// Header ancestry must use the candidate's parent, not the best tip's height.
#[test]
fn contextual_header_ancestry_preserves_parent_height_and_error_order() {
    use crate::{
        arbitrary::Prepare,
        tests::{setup::new_state_with_mainnet_genesis, FakeChainHelper},
    };

    let _init_guard = zebra_test::init();
    let (finalized_state, mut non_finalized_state, genesis) = new_state_with_mainnet_genesis();
    let db = &finalized_state.db;
    let base = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Arc<Block>>()
        .unwrap();

    initial_contextual_validity(db, &non_finalized_state, &base.clone().prepare())
        .expect("genesis supplies the parent height for the first block");
    non_finalized_state
        .commit_new_chain(base.clone().prepare(), db)
        .unwrap();
    let best = base.make_fake_child().set_work(100);
    let best_tip = best.make_fake_child();
    let side = base.make_fake_child().set_work(50);
    for block in [best, best_tip.clone(), side.clone()] {
        non_finalized_state
            .commit_block(block.prepare(), db)
            .unwrap();
    }
    assert_eq!(
        crate::service::read::best_tip(&non_finalized_state, db),
        Some((block::Height(3), best_tip.hash()))
    );

    let candidate = side.make_fake_child().prepare();
    initial_contextual_validity(db, &non_finalized_state, &candidate)
        .expect("a candidate can extend a shorter side chain with partial history");

    let mut non_sequential = candidate.clone();
    non_sequential.height = block::Height(4);
    assert!(matches!(
        initial_contextual_validity(db, &non_finalized_state, &non_sequential),
        Err(ValidateContextError::NonSequentialBlock {
            candidate_height: block::Height(4),
            parent_height: block::Height(2),
        })
    ));

    let mut missing_parent = candidate.block.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut missing_parent).header).previous_block_hash =
        block::Hash([0xff; 32]);
    assert!(matches!(
        initial_contextual_validity(db, &non_finalized_state, &missing_parent.prepare()),
        Err(ValidateContextError::NotReadyToBeCommitted)
    ));

    // Genesis has an unknown parent, but the orphan error takes precedence.
    assert!(matches!(
        initial_contextual_validity(db, &non_finalized_state, &genesis.block.clone().prepare()),
        Err(ValidateContextError::OrphanedBlock {
            candidate_height: block::Height(0),
            finalized_tip_height: block::Height(0),
        })
    ));
}

/// Tests for the contextual block subsidy checks once the [halving-preserving issuance ZIP][zip] is
/// active.
///
/// [zip]: https://zips.z.cash/zip-0237
mod nsm {
    use std::collections::HashMap;

    use zebra_chain::{
        amount::{Amount, DeferredPoolBalanceChange, NegativeAllowed, NonNegative},
        block::Height,
        parameters::{
            subsidy::{
                additional_block_subsidy, nsm_fee_contribution, scheduled_block_subsidy,
                CoinbaseTransactionError, SubsidyError,
            },
            testnet::{ConfiguredActivationHeights, RegtestParameters},
        },
        serialization::ZcashDeserializeInto,
        transaction::{self, LockTime, Transaction},
        transparent,
        value_balance::ValueBalance,
    };

    use crate::{arbitrary::Prepare, request::ContextuallyVerifiedBlock};

    use super::super::super::*;

    const NU7_HEIGHT: u32 = 10;
    const FEE: i64 = 100;
    /// The NSM value balance this network seeds at NU7 activation.
    const INITIAL_NSM_VALUE_BALANCE: i64 = 1_000_000_000;

    fn amount(zatoshis: i64) -> Amount<NonNegative> {
        zatoshis.try_into().expect("valid amount")
    }

    /// Returns all of `FEE` before NU7, or the miner's share after fee contributions activate.
    fn miner_fees(height: Height, network: &Network) -> Amount<NonNegative> {
        (amount(FEE) - nsm_fee_contribution(height, network, amount(FEE))).unwrap()
    }

    fn network() -> Network {
        Network::new_regtest(RegtestParameters {
            activation_heights: ConfiguredActivationHeights {
                nu5: Some(1),
                nu6: Some(1),
                nu6_3: Some(1),
                nu7: Some(NU7_HEIGHT),
                ..Default::default()
            },
            initial_nsm_value_balance: Some(amount(INITIAL_NSM_VALUE_BALANCE)),
            nsm_reissuance_height: Some(Height(NU7_HEIGHT)),
            ..Default::default()
        })
    }

    /// Returns chain value pools whose NSM value balance holds `nsm_value_balance` zatoshis.
    fn pools(nsm_value_balance: i64) -> ValueBalance<NonNegative> {
        let mut pools = ValueBalance::<NonNegative>::zero();
        pools.set_nsm_amount(amount(nsm_value_balance));
        pools
    }

    /// Returns a contextually verified block at `height` whose coinbase pays `coinbase_value`, and
    /// which contains a transaction paying a fee of `FEE`.
    fn block(
        height: Height,
        coinbase_value: Amount<NonNegative>,
        parent_pools: ValueBalance<NonNegative>,
        network: &Network,
    ) -> ContextuallyVerifiedBlock {
        let mut block = zebra_test::vectors::BLOCK_MAINNET_347499_BYTES
            .zcash_deserialize_into::<Block>()
            .unwrap();

        let coinbase = Transaction::test_v4(
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
        );

        let outpoint = transparent::OutPoint {
            hash: transaction::Hash([1; 32]),
            index: 0,
        };
        let spent_output = transparent::Output {
            value: amount(1_000),
            lock_script: transparent::Script::new(&[]),
        };
        let spend = Transaction::test_v4(
            vec![transparent::Input::PrevOut {
                outpoint,
                unlock_script: transparent::Script::new(&[]),
                sequence: u32::MAX,
            }],
            vec![transparent::Output {
                value: amount(1_000 - FEE),
                lock_script: transparent::Script::new(&[]),
            }],
            LockTime::unlocked(),
            height,
        );

        block.transactions = vec![Arc::new(coinbase), Arc::new(spend)];

        let spent_outputs = HashMap::from([(
            outpoint,
            transparent::OrderedUtxo::new(spent_output, Height(1), 1),
        )]);

        ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            Arc::new(block).prepare(),
            spent_outputs,
            DeferredPoolBalanceChange::zero(),
            network,
            parent_pools,
        )
        .expect("valid value balances")
    }

    fn subsidy_error(result: Result<(), ValidateContextError>) -> SubsidyError {
        match result {
            Err(ValidateContextError::InvalidSubsidy {
                subsidy_error: CoinbaseTransactionError::Subsidy(subsidy_error),
                ..
            }) => subsidy_error,
            other => panic!("expected an invalid subsidy error, got {other:?}"),
        }
    }

    /// The coinbase must pay the scheduled subsidy plus the additional subsidy for the NSM value balance
    /// balance after the parent block, plus the block's transaction fees.
    #[test]
    fn subsidy_depends_on_parent_chain_value_pools() {
        let _init_guard = zebra_test::init();

        let network = network();
        let height = Height(NU7_HEIGHT + 2);

        let nsm_value_balance = 1_000_000_000;
        let parent_pools = pools(nsm_value_balance);

        let scheduled = scheduled_block_subsidy(height, &network).unwrap();
        let subsidy = (scheduled
            + additional_block_subsidy(height, &network, amount(nsm_value_balance)))
        .unwrap();
        assert_ne!(scheduled, subsidy);

        let valid = block(
            height,
            (subsidy + miner_fees(height, &network)).unwrap(),
            parent_pools,
            &network,
        );
        check::nsm_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
            .expect("the coinbase pays the subsidy and the fees");

        // The same block is invalid after a parent with an empty NSM value balance.
        assert_eq!(
            subsidy_error(check::nsm_subsidy_is_valid(
                &valid,
                &network,
                ValueBalance::zero(),
                amount(FEE),
            )),
            SubsidyError::InvalidMinerFees,
        );

        // A coinbase paying only the scheduled subsidy is invalid.
        let invalid = block(
            height,
            (scheduled + miner_fees(height, &network)).unwrap(),
            parent_pools,
            &network,
        );
        assert_eq!(
            subsidy_error(check::nsm_subsidy_is_valid(
                &invalid,
                &network,
                parent_pools,
                amount(FEE),
            )),
            SubsidyError::InvalidMinerFees,
        );

        // A coinbase that doesn't claim the fees is invalid from NU6 onward.
        let invalid = block(height, subsidy, parent_pools, &network);
        assert_eq!(
            subsidy_error(check::nsm_subsidy_is_valid(
                &invalid,
                &network,
                parent_pools,
                amount(FEE),
            )),
            SubsidyError::InvalidMinerFees,
        );
    }

    /// The activation block reissues from `INITIAL_NSM_VALUE_BALANCE`, which is in no chain value
    /// pool beforehand, and its block seeds the NSM value balance with it.
    #[test]
    fn activation_block_seeds_the_nsm_value_balance() {
        let _init_guard = zebra_test::init();

        let network = network();
        let height = Height(NU7_HEIGHT);
        let initial = amount(INITIAL_NSM_VALUE_BALANCE);

        let scheduled = scheduled_block_subsidy(height, &network).unwrap();
        let reissued = additional_block_subsidy(height, &network, initial);
        let subsidy = (scheduled + reissued).unwrap();

        // The parent's pools hold no NSM balance, but the activation block still reissues.
        let parent_pools = ValueBalance::<NonNegative>::zero();
        let valid = block(
            height,
            (subsidy + miner_fees(height, &network)).unwrap(),
            parent_pools,
            &network,
        );
        check::nsm_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
            .expect("the activation block reissues from INITIAL_NSM_VALUE_BALANCE");

        // The block credits the balance with the seed and the fees removed from circulation, and
        // debits what it reissued.
        let contributed = nsm_fee_contribution(height, &network, amount(FEE));
        assert_eq!(
            valid.chain_value_pool_change.nsm_amount(),
            ((initial.constrain::<NegativeAllowed>().unwrap()
                - reissued.constrain::<NegativeAllowed>().unwrap())
            .unwrap()
                + contributed.constrain::<NegativeAllowed>().unwrap())
            .unwrap(),
        );
    }

    /// From NU7 activation the coinbase can only claim the fees ZIP 235 leaves to the miner, and the
    /// rest is credited to the NSM value balance.
    #[test]
    fn fee_contribution_is_removed_from_the_coinbase() {
        let _init_guard = zebra_test::init();

        let network = network();
        let height = Height(NU7_HEIGHT + 1);
        let parent_pools = pools(INITIAL_NSM_VALUE_BALANCE);

        let subsidy = (scheduled_block_subsidy(height, &network).unwrap()
            + additional_block_subsidy(height, &network, amount(INITIAL_NSM_VALUE_BALANCE)))
        .unwrap();

        assert_eq!(miner_fees(height, &network), amount(FEE * 4 / 10));

        // A coinbase claiming all the fees, or one zatoshi more or less than its share, is invalid.
        for claimed in [FEE, FEE * 4 / 10 + 1, FEE * 4 / 10 - 1] {
            let invalid = block(
                height,
                (subsidy + amount(claimed)).unwrap(),
                parent_pools,
                &network,
            );
            assert_eq!(
                subsidy_error(check::nsm_subsidy_is_valid(
                    &invalid,
                    &network,
                    parent_pools,
                    amount(FEE),
                )),
                SubsidyError::InvalidMinerFees,
            );
        }

        let valid = block(
            height,
            (subsidy + miner_fees(height, &network)).unwrap(),
            parent_pools,
            &network,
        );
        check::nsm_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
            .expect("the coinbase claims the miner's share of the fees");

        // The issued supply and the NSM value balance together grow by the scheduled subsidy.
        assert_eq!(
            (valid.chain_value_pool_change.total().unwrap()
                + valid.chain_value_pool_change.nsm_amount())
            .unwrap(),
            scheduled_block_subsidy(height, &network)
                .unwrap()
                .constrain::<NegativeAllowed>()
                .unwrap(),
        );
    }
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

    use crate::{
        service::finalized_state::calculate_deferred_pool_balance_change, ContextuallyVerifiedBlock,
    };

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
        .with_nsm_reissuance_height(reissuance)
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
    parent_pools.set_nsm_amount(reserve);
    let header = zebra_test::vectors::DUMMY_HEADER.zcash_deserialize_into()?;
    let outpoint = transparent::OutPoint::from_usize(TransactionHash([0; 32]), 0);
    let utxos = HashMap::from([(
        outpoint,
        transparent::OrderedUtxo::from_utxo(
            transparent::Utxo::new(
                transparent::Output::new(100.try_into()?, transparent::Script::new(&[])),
                block::Height(999),
                false,
            ),
            0,
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
        let total = subsidy::block_subsidy_with_parent_pools(height, &network, parent_pools)?;
        let funding = subsidy::funding_stream_values(height, &network, total)?;
        let grant = funding[&FundingStreamReceiver::MajorGrants];
        let deferred = funding[&FundingStreamReceiver::Deferred];
        let deferred_change =
            calculate_deferred_pool_balance_change(height, &network, parent_pools)?;
        assert_eq!(
            deferred_change,
            DeferredPoolBalanceChange::new(deferred.constrain()?),
        );
        // Of the 11 zatoshi in gross fees, 6 go to the reserve and 5 to the miner.
        let miner = (total - grant - deferred + Amount::try_from(5)?)?;
        for delta in [-1, 0, 1] {
            let block = make_block(height, (miner.zatoshis() + delta).try_into()?, grant);
            let (contextual, fees) = ContextuallyVerifiedBlock::with_block_spent_utxos_and_fees(
                Arc::new(block).into(),
                utxos.clone(),
                deferred_change,
                &network,
                parent_pools,
            )?;
            let fees = fees.expect("all test blocks are at or after NU7");
            if height < reissuance {
                // Before reissuance the semantic verifier owns the payout checks.
                continue;
            }
            let result = nsm_subsidy_is_valid(&contextual, &network, parent_pools, fees);
            if delta == 0 {
                result?;
            } else {
                assert!(matches!(
                    result,
                    Err(ValidateContextError::InvalidSubsidy {
                        subsidy_error: CoinbaseTransactionError::Subsidy(
                            SubsidyError::InvalidMinerFees
                        ),
                        ..
                    }),
                ));
            }

            if delta == 0 {
                let mut other_parent = parent_pools;
                other_parent.set_nsm_amount((reserve * 2)?);
                assert!(matches!(
                    nsm_subsidy_is_valid(&contextual, &network, other_parent, fees),
                    Err(ValidateContextError::InvalidSubsidy {
                        subsidy_error: CoinbaseTransactionError::Subsidy(
                            SubsidyError::FundingStreamNotFound
                        ),
                        ..
                    }),
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
            let (contextual, fees) = ContextuallyVerifiedBlock::with_block_spent_utxos_and_fees(
                Arc::new(block).into(),
                utxos.clone(),
                deferred_change,
                &network,
                parent_pools,
            )?;
            assert!(matches!(
                nsm_subsidy_is_valid(
                    &contextual,
                    &network,
                    parent_pools,
                    fees.expect("all test blocks are at or after NU7"),
                ),
                Err(ValidateContextError::InvalidSubsidy {
                    subsidy_error: CoinbaseTransactionError::Subsidy(
                        SubsidyError::FundingStreamNotFound
                    ),
                    ..
                }),
            ));
        }
    }

    Ok(())
}
