//! StateService test vectors.
//!
//! TODO: move these tests into tests::vectors and tests::prop modules.

use std::{env, sync::Arc, time::Duration};

use tokio::runtime::Runtime;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    block::{self, Block, CountedHeader, Height},
    chain_tip::ChainTip,
    fmt::SummaryDebug,
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction, transparent,
    value_balance::ValueBalance,
};

use zebra_test::{prelude::*, transcript::Transcript};

use crate::{
    arbitrary::Prepare,
    init_test,
    service::{arbitrary::populated_state, chain_tip::TipAction, StateService},
    tests::setup::{partial_nu5_chain_strategy, transaction_v4_from_coinbase},
    BoxError, CheckpointVerifiedBlock, Config, Request, Response, SemanticallyVerifiedBlock,
};

const LAST_BLOCK_HEIGHT: u32 = 10;

async fn test_populated_state_responds_correctly(
    mut state: Buffer<BoxService<Request, Response, BoxError>, Request>,
) -> Result<()> {
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let block_hashes: Vec<block::Hash> = blocks.iter().map(|block| block.hash()).collect();
    let block_headers: Vec<CountedHeader> = blocks
        .iter()
        .map(|block| CountedHeader {
            header: block.header.clone(),
        })
        .collect();

    for (ind, block) in blocks.into_iter().enumerate() {
        let mut transcript = vec![];
        let height = block.coinbase_height().unwrap();
        let hash = block.hash();

        transcript.push((
            Request::Depth(block.hash()),
            Ok(Response::Depth(Some(LAST_BLOCK_HEIGHT - height.0))),
        ));

        // these requests don't have any arguments, so we just do them once
        if ind == LAST_BLOCK_HEIGHT as usize {
            transcript.push((Request::Tip, Ok(Response::Tip(Some((height, hash))))));

            let locator_hashes = vec![
                block_hashes[LAST_BLOCK_HEIGHT as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 1) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 2) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 4) as usize],
                block_hashes[(LAST_BLOCK_HEIGHT - 8) as usize],
                block_hashes[0],
            ];

            transcript.push((
                Request::BlockLocator,
                Ok(Response::BlockLocator(locator_hashes)),
            ));
        }

        // Spec: transactions in the genesis block are ignored.
        if height.0 != 0 {
            for transaction in &block.transactions {
                let transaction_hash = transaction.hash();

                transcript.push((
                    Request::Transaction(transaction_hash),
                    Ok(Response::Transaction(Some(transaction.clone()))),
                ));
            }
        }

        transcript.push((
            Request::Block(hash.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        transcript.push((
            Request::Block(height.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        // Spec: transactions in the genesis block are ignored.
        if height.0 != 0 {
            for transaction in &block.transactions {
                let transaction_hash = transaction.hash();

                let from_coinbase = transaction.is_coinbase();
                for (index, output) in transaction.outputs().iter().cloned().enumerate() {
                    let outpoint = transparent::OutPoint::from_usize(transaction_hash, index);

                    let utxo = transparent::Utxo {
                        output,
                        height,
                        from_coinbase,
                    };

                    transcript.push((Request::AwaitUtxo(outpoint), Ok(Response::Utxo(utxo))));
                }
            }
        }

        let mut append_locator_transcript = |split_ind| {
            let block_hashes = block_hashes.clone();
            let (known_hashes, next_hashes) = block_hashes.split_at(split_ind);

            let block_headers = block_headers.clone();
            let (_, next_headers) = block_headers.split_at(split_ind);

            // no stop
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: None,
                },
                Ok(Response::BlockHashes(next_hashes.to_vec())),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: None,
                },
                Ok(Response::BlockHeaders(next_headers.to_vec())),
            ));

            // stop at the next block
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: next_hashes.first().cloned(),
                },
                Ok(Response::BlockHashes(
                    next_hashes.first().iter().cloned().cloned().collect(),
                )),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: next_hashes.first().cloned(),
                },
                Ok(Response::BlockHeaders(
                    next_headers.first().iter().cloned().cloned().collect(),
                )),
            ));

            // stop at a block that isn't actually in the chain
            // tests bug #2789
            transcript.push((
                Request::FindBlockHashes {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: Some(block::Hash([0xff; 32])),
                },
                Ok(Response::BlockHashes(next_hashes.to_vec())),
            ));

            transcript.push((
                Request::FindBlockHeaders {
                    known_blocks: known_hashes.iter().rev().cloned().collect(),
                    stop: Some(block::Hash([0xff; 32])),
                },
                Ok(Response::BlockHeaders(next_headers.to_vec())),
            ));
        };

        // split before the current block, and locate the current block
        append_locator_transcript(ind);

        // split after the current block, and locate the next block
        append_locator_transcript(ind + 1);

        let transcript = Transcript::from(transcript);
        transcript.check(&mut state).await?;
    }

    Ok(())
}

#[tokio::main]
async fn populate_and_check(blocks: Vec<Arc<Block>>) -> Result<()> {
    let (state, _, _, _) = populated_state(blocks, &Network::Mainnet).await;
    test_populated_state_responds_correctly(state).await?;
    Ok(())
}

fn out_of_order_committing_strategy() -> BoxedStrategy<Vec<Arc<Block>>> {
    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect::<Vec<_>>();

    Just(blocks).prop_shuffle().boxed()
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_state_still_responds_to_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    let block =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into::<Arc<Block>>()?;

    let iter = vec![
        // No checks for SemanticallyVerifiedBlock or CommitCheckpointVerifiedBlock because empty state
        // precondition doesn't matter to them
        (Request::Depth(block.hash()), Ok(Response::Depth(None))),
        (Request::Tip, Ok(Response::Tip(None))),
        (Request::BlockLocator, Ok(Response::BlockLocator(vec![]))),
        (
            Request::Transaction(transaction::Hash([0; 32])),
            Ok(Response::Transaction(None)),
        ),
        (
            Request::Block(block.hash().into()),
            Ok(Response::Block(None)),
        ),
        (
            Request::Block(block.coinbase_height().unwrap().into()),
            Ok(Response::Block(None)),
        ),
        // No check for AwaitUTXO because it will wait if the UTXO isn't present
        (
            Request::FindBlockHashes {
                known_blocks: vec![block.hash()],
                stop: None,
            },
            Ok(Response::BlockHashes(Vec::new())),
        ),
        (
            Request::FindBlockHeaders {
                known_blocks: vec![block.hash()],
                stop: None,
            },
            Ok(Response::BlockHeaders(Vec::new())),
        ),
    ]
    .into_iter();
    let transcript = Transcript::from(iter);

    let network = Network::Mainnet;
    let state = init_test(&network).await;

    transcript.check(state).await?;

    Ok(())
}

#[test]
fn state_behaves_when_blocks_are_committed_in_order() -> Result<()> {
    let _init_guard = zebra_test::init();

    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    populate_and_check(blocks)?;

    Ok(())
}

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 2;

/// The legacy chain limit for tests.
const TEST_LEGACY_CHAIN_LIMIT: usize = 100;

/// Check more blocks than the legacy chain limit.
const OVER_LEGACY_CHAIN_LIMIT: u32 = TEST_LEGACY_CHAIN_LIMIT as u32 + 10;

/// Check fewer blocks than the legacy chain limit.
const UNDER_LEGACY_CHAIN_LIMIT: u32 = TEST_LEGACY_CHAIN_LIMIT as u32 - 10;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES))
    )]

    /// Test out of order commits of continuous block test vectors from genesis onward.
    #[test]
    fn state_behaves_when_blocks_are_committed_out_of_order(blocks in out_of_order_committing_strategy()) {
        let _init_guard = zebra_test::init();

        populate_and_check(blocks).unwrap();
    }

    /// Test blocks that are less than the NU5 activation height.
    #[test]
    fn some_block_less_than_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(4, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }

    /// Test the maximum amount of blocks to check before chain is declared to be legacy.
    #[test]
    fn no_transaction_with_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(4, true, OVER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let tip_height = chain
            .last()
            .expect("chain contains at least one block")
            .coinbase_height()
            .expect("chain contains valid blocks");

        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err(format!(
                "could not find any transactions in recent blocks: checked {TEST_LEGACY_CHAIN_LIMIT} blocks back from {tip_height:?}",
            ))
        );
    }

    /// Test the `Block.check_transaction_network_upgrade()` error inside the legacy check.
    #[test]
    fn at_least_one_transaction_with_inconsistent_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, false, OVER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        // this test requires that an invalid block is encountered
        // before a valid block (and before the check gives up),
        // but setting `transaction_has_valid_network_upgrade` to false
        // sometimes generates blocks with all valid (or missing) network upgrades

        // we must check at least one block, and the first checked block must be invalid
        let first_checked_block = chain
            .iter()
            .rev()
            .take_while(|block| block.coinbase_height().unwrap() >= nu_activation_height)
            .take(100)
            .next();
        prop_assume!(first_checked_block.is_some());
        prop_assume!(
            first_checked_block
                .unwrap()
                .check_transaction_network_upgrade_consistency(&network)
                .is_err()
        );

        let response = crate::service::check::legacy_chain(
            nu_activation_height,
            chain.clone().into_iter().rev(),
            &network,
            TEST_LEGACY_CHAIN_LIMIT,
        ).map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("inconsistent network upgrade found in transaction: WrongTransactionConsensusBranchId".into()),
            "first: {:?}, last: {:?}",
            chain.first().map(|block| block.coinbase_height()),
            chain.last().map(|block| block.coinbase_height()),
        );
    }

    /// Test there is at least one transaction with a valid `network_upgrade` in the legacy check.
    #[test]
    fn at_least_one_transaction_with_valid_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, true, UNDER_LEGACY_CHAIN_LIMIT, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::check::legacy_chain(nu_activation_height, chain.into_iter().rev(), &network, TEST_LEGACY_CHAIN_LIMIT)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }

    /// Test that the value pool is updated accordingly.
    ///
    /// 1. Generate a finalized chain and some non-finalized blocks.
    /// 2. Check that initially the value pool is empty.
    /// 3. Commit the finalized blocks and check that the value pool is updated accordingly.
    /// 4. Commit the non-finalized blocks and check that the value pool is also updated
    ///    accordingly.
    #[test]
    fn value_pool_is_updated(
        (network, finalized_blocks, non_finalized_blocks)
            in continuous_empty_blocks_from_test_vectors(),
    ) {
        let _init_guard = zebra_test::init();
        let (mut state_service, _, _, _) = Runtime::new().unwrap().block_on(async {
            // We're waiting to verify each block here, so we don't need the maximum checkpoint height.
            StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await
        });

        prop_assert_eq!(state_service.read_service.db.finalized_value_pool(), ValueBalance::zero());
        prop_assert_eq!(
            state_service.read_service.latest_non_finalized_state().best_chain().map(|chain| chain.chain_value_pools).unwrap_or_else(ValueBalance::zero),
            ValueBalance::zero()
        );

        // the slow start rate for the first few blocks, as in the spec
        const SLOW_START_RATE: i64 = 62500;
        // the expected transparent pool value, calculated using the slow start rate
        let mut expected_transparent_pool = ValueBalance::zero();

        let mut expected_finalized_value_pool = Ok(ValueBalance::zero());
        for block in finalized_blocks {
            // the genesis block has a zero-valued transparent output,
            // which is not included in the UTXO set
            if block.height > block::Height(0) {
                let utxos = &block.new_outputs.iter().map(|(k, ordered_utxo)| (*k, ordered_utxo.utxo.clone())).collect();
                let block_value_pool = &block.block.chain_value_pool_change(utxos, None)?;
                expected_finalized_value_pool += *block_value_pool;
            }

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block.clone());
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed finalized block commit: {:?}", result);

            prop_assert_eq!(
                state_service.read_service.db.finalized_value_pool(),
                expected_finalized_value_pool.clone()?.constrain()?
            );

            let transparent_value = SLOW_START_RATE * i64::from(block.height.0);
            let transparent_value = transparent_value.try_into().unwrap();
            let transparent_value = ValueBalance::from_transparent_amount(transparent_value);
            expected_transparent_pool = (expected_transparent_pool + transparent_value).unwrap();
            prop_assert_eq!(
                state_service.read_service.db.finalized_value_pool(),
                expected_transparent_pool
            );
        }

        let mut expected_non_finalized_value_pool = Ok(expected_finalized_value_pool?);
        for block in non_finalized_blocks {
            let utxos = block.new_outputs.clone();
            let block_value_pool = &block.block.chain_value_pool_change(&transparent::utxos_from_ordered_utxos(utxos), None)?;
            expected_non_finalized_value_pool += *block_value_pool;

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block.clone());
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed non-finalized block commit: {:?}", result);

            prop_assert_eq!(
                state_service.read_service.latest_non_finalized_state().best_chain().unwrap().chain_value_pools,
                expected_non_finalized_value_pool.clone()?.constrain()?
            );

            let transparent_value = SLOW_START_RATE * i64::from(block.height.0);
            let transparent_value = transparent_value.try_into().unwrap();
            let transparent_value = ValueBalance::from_transparent_amount(transparent_value);
            expected_transparent_pool = (expected_transparent_pool + transparent_value).unwrap();
            prop_assert_eq!(
                state_service.read_service.latest_non_finalized_state().best_chain().unwrap().chain_value_pools,
                expected_transparent_pool
            );
        }
    }
}

// This test sleeps for every block, so we only ever want to run it once
proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(1)
    )]

    /// Test that the best tip height is updated accordingly.
    ///
    /// 1. Generate a finalized chain and some non-finalized blocks.
    /// 2. Check that initially the best tip height is empty.
    /// 3. Commit the finalized blocks and check that the best tip height is updated accordingly.
    /// 4. Commit the non-finalized blocks and check that the best tip height is also updated
    ///    accordingly.
    #[test]
    fn chain_tip_sender_is_updated(
        (network, finalized_blocks, non_finalized_blocks)
            in continuous_empty_blocks_from_test_vectors(),
    ) {
        let _init_guard = zebra_test::init();

        let (mut state_service, _read_only_state_service, latest_chain_tip, mut chain_tip_change) = Runtime::new().unwrap().block_on(async {
            // We're waiting to verify each block here, so we don't need the maximum checkpoint height.
            StateService::new(Config::ephemeral(), &network, Height::MAX, 0).await
        });

        prop_assert_eq!(latest_chain_tip.best_tip_height(), None);
        prop_assert_eq!(chain_tip_change.last_tip_change(), None);

        for block in finalized_blocks {
            let expected_block = block.clone();

            let expected_action = if expected_block.height <= block::Height(1) {
                // 0: reset by both initialization and the Genesis network upgrade
                // 1: reset by the BeforeOverwinter network upgrade
                TipAction::reset_with(expected_block.clone().into())
            } else {
                TipAction::grow_with(expected_block.clone().into())
            };

            let result_receiver = state_service.queue_and_commit_to_finalized_state(block);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed finalized block commit: {:?}", result);

            // Wait for the channels to be updated by the block commit task.
            // TODO: add a blocking method on ChainTipChange
            std::thread::sleep(Duration::from_secs(1));

            prop_assert_eq!(latest_chain_tip.best_tip_height(), Some(expected_block.height));
            prop_assert_eq!(chain_tip_change.last_tip_change(), Some(expected_action));
        }

        for block in non_finalized_blocks {
            let expected_block = block.clone();

            let expected_action = if expected_block.height == block::Height(1) {
                // 1: reset by the BeforeOverwinter network upgrade
                TipAction::reset_with(expected_block.clone().into())
            } else {
                TipAction::grow_with(expected_block.clone().into())
            };

            let result_receiver = state_service.queue_and_commit_to_non_finalized_state(block);
            let result = result_receiver.blocking_recv();

            prop_assert!(result.is_ok(), "unexpected failed non-finalized block commit: {:?}", result);

            // Wait for the channels to be updated by the block commit task.
            // TODO: add a blocking method on ChainTipChange
            std::thread::sleep(Duration::from_secs(1));

            prop_assert_eq!(latest_chain_tip.best_tip_height(), Some(expected_block.height));
            prop_assert_eq!(chain_tip_change.last_tip_change(), Some(expected_action));
        }
    }
}

/// Test strategy to generate a chain split in two from the test vectors.
///
/// Selects either the mainnet or testnet chain test vector and randomly splits the chain in two
/// lists of blocks. The first containing the blocks to be finalized (which always includes at
/// least the genesis block) and the blocks to be stored in the non-finalized state.
fn continuous_empty_blocks_from_test_vectors() -> impl Strategy<
    Value = (
        Network,
        SummaryDebug<Vec<CheckpointVerifiedBlock>>,
        SummaryDebug<Vec<SemanticallyVerifiedBlock>>,
    ),
> {
    any::<Network>()
        .prop_flat_map(|network| {
            // Select the test vector based on the network
            let raw_blocks = network.blockchain_map();

            // Transform the test vector's block bytes into a vector of `SemanticallyVerifiedBlock`s.
            let blocks: Vec<_> = raw_blocks
                .iter()
                .map(|(_height, &block_bytes)| {
                    let mut block_reader: &[u8] = block_bytes;
                    let mut block = Block::zcash_deserialize(&mut block_reader)
                        .expect("Failed to deserialize block from test vector");

                    let coinbase = transaction_v4_from_coinbase(&block.transactions[0]);
                    block.transactions = vec![Arc::new(coinbase)];

                    Arc::new(block).prepare()
                })
                .collect();

            // Always finalize the genesis block
            let finalized_blocks_count = 1..=blocks.len();

            (Just(network), Just(blocks), finalized_blocks_count)
        })
        .prop_map(|(network, mut blocks, finalized_blocks_count)| {
            let non_finalized_blocks = blocks.split_off(finalized_blocks_count);
            let finalized_blocks: Vec<_> =
                blocks.into_iter().map(CheckpointVerifiedBlock).collect();

            (
                network,
                finalized_blocks.into(),
                non_finalized_blocks.into(),
            )
        })
}
