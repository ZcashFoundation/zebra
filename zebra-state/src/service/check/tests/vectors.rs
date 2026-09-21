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

/// Tests for the contextual block subsidy checks once the [halving-preserving issuance ZIP][zip] is
/// active.
///
/// [zip]: https://github.com/zcash/zips/pull/1354
#[cfg(zcash_unstable = "zip234")]
mod zip234 {
    use std::collections::HashMap;

    use zebra_chain::{
        amount::{Amount, DeferredPoolBalanceChange, NegativeAllowed, NonNegative},
        block::Height,
        parameters::{
            subsidy::{
                additional_block_subsidy, nsm_fee_contribution, nsm_value_balance_change,
                scheduled_block_subsidy, CoinbaseTransactionError, SubsidyError,
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

    /// Returns the part of `FEE` the coinbase transaction at `height` can claim: all of it, or what
    /// ZIP 235 leaves in builds with the `zip235` cfg.
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

        let mut contextual = ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            Arc::new(block).prepare(),
            spent_outputs,
            DeferredPoolBalanceChange::zero(),
        )
        .expect("valid value balances");
        contextual.chain_value_pool_change.set_nsm_amount(
            nsm_value_balance_change(height, network, parent_pools, amount(FEE))
                .expect("valid NSM value balance change"),
        );

        contextual
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
        check::zip234_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
            .expect("the coinbase pays the subsidy and the fees");

        // The same block is invalid after a parent with an empty NSM value balance.
        assert_eq!(
            subsidy_error(check::zip234_subsidy_is_valid(
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
            subsidy_error(check::zip234_subsidy_is_valid(
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
            subsidy_error(check::zip234_subsidy_is_valid(
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
        check::zip234_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
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
    #[cfg(zcash_unstable = "zip235")]
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
                subsidy_error(check::zip234_subsidy_is_valid(
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
        check::zip234_subsidy_is_valid(&valid, &network, parent_pools, amount(FEE))
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
