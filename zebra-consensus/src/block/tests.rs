//! Tests for block verification

#![allow(clippy::unwrap_in_result)]

use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    amount::{DeferredPoolBalanceChange, MAX_MONEY},
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
        },
        Block, Height,
    },
    parameters::NetworkUpgrade,
    serialization::{BytesInDisplayOrder, ZcashDeserialize, ZcashDeserializeInto},
    transaction::{arbitrary::transaction_to_fake_v5, LockTime, Transaction},
    work::difficulty::{ParameterDifficulty as _, INVALID_COMPACT_DIFFICULTY},
};
use zebra_script::Sigops;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::transaction;

use super::check::{miner_fees_are_valid, subsidy_is_valid};
use super::*;

mod nu7;

/// A proposal error must retain transaction siblings whose Rayon work is already dispatched.
#[tokio::test]
async fn proposal_error_waits_for_dispatched_transaction_siblings() {
    use std::time::Duration;
    use tokio::sync::{oneshot, Mutex};
    use tokio::time::timeout;

    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let (_, bytes) = network
        .block_iter()
        .find(|(height, _)| **height == 982_681)
        .expect("the historical fixture contains a block with multiple transactions");
    let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
    assert!(block.transactions.len() >= 2);
    let failed_id = block.transactions[0].hash();
    let held_id = block.transactions[1].hash();
    let deadline = Duration::from_secs(10);
    let (started, entered) = oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let (finished, mut completed) = oneshot::channel();
    let worker = Arc::new(Mutex::new(Some((started, held, finished))));
    let (fail, failure) = oneshot::channel();
    let (reported, error_reported) = oneshot::channel();
    let failure = Arc::new(Mutex::new(Some((failure, reported))));
    let transactions = tower::service_fn(move |request: transaction::BlockRequest| {
        let worker = worker.clone();
        let failure = failure.clone();
        async move {
            if request.transaction_hash == failed_id {
                let (failure, reported) = failure.lock().await.take().unwrap();
                failure.await?;
                let _ = reported.send(());
                return Err::<transaction::BlockResponse, BoxError>(
                    TransactionError::BadBalance.into(),
                );
            }
            if request.transaction_hash == held_id {
                let (started, held, finished) = worker.lock().await.take().unwrap();
                crate::primitives::spawn_fifo(move || {
                    let _ = started.send(());
                    // Disconnect or a failing test must not strand a Rayon worker.
                    let released = held.recv_timeout(deadline).is_ok();
                    let _ = finished.send(released);
                })
                .await?;
            }
            Ok(transaction::BlockResponse {
                tx_id: request.transaction.unmined_id(),
                miner_fee: None,
                sigops: 0,
            })
        }
    });
    let state = tower::service_fn(|request| async move {
        assert!(
            matches!(request, zs::Request::KnownBlock(_)),
            "failed semantic verification must not reach contextual proposal validation"
        );
        Ok::<_, BoxError>(zs::Response::KnownBlock(None))
    });
    let checking = SemanticBlockVerifier::new(&network, state, transactions)
        .oneshot(Request::CheckProposal(block));
    tokio::pin!(checking);
    tokio::select! {
        result = &mut checking => panic!("proposal completed before worker dispatch: {result:?}"),
        result = timeout(deadline, entered) => result.expect("the worker starts").unwrap(),
    }
    fail.send(()).unwrap();
    let premature = timeout(deadline, async {
        tokio::select! {
            biased;
            result = &mut checking => Some(result),
            result = error_reported => {
                result.expect("the failing transaction reports its error");
                None
            }
        }
    })
    .await
    .expect("the transaction error is processed");
    let returned_early = premature.is_some();
    assert!(matches!(
        completed.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    release.send(()).unwrap();
    let result = match premature {
        Some(result) => result,
        None => timeout(deadline, &mut checking)
            .await
            .expect("the drained proposal finishes"),
    };
    assert!(timeout(deadline, completed)
        .await
        .expect("the worker finishes")
        .unwrap());
    assert!(matches!(
        result,
        Err(VerifyBlockError::Transaction(TransactionError::BadBalance))
    ));
    assert!(
        !returned_early,
        "a proposal error must retain dispatched sibling transaction work"
    );
}

static VALID_BLOCK_TRANSCRIPT: Lazy<Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let hash = Ok(block.as_ref().into());
        vec![(Request::Commit(block), hash)]
    });

static INVALID_TIME_BLOCK_TRANSCRIPT: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let mut block: Block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap();

    // Modify the block's time
    // Changing the block header also invalidates the header hashes, but
    // those checks should be performed later in validation, because they
    // are more expensive.
    let three_hours_in_the_future = Utc::now()
        .checked_add_signed(chrono::Duration::hours(3))
        .ok_or_else(|| eyre!("overflow when calculating 3 hours in the future"))
        .unwrap();
    Arc::make_mut(&mut block.header).time = three_hours_in_the_future;

    vec![(
        Request::Commit(Arc::new(block)),
        Err(ExpectedTranscriptError::Any),
    )]
});

static INVALID_HEADER_SOLUTION_TRANSCRIPT: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let mut block: Block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap();

    // Change nonce to something invalid
    Arc::make_mut(&mut block.header).nonce = [0; 32].into();

    vec![(
        Request::Commit(Arc::new(block)),
        Err(ExpectedTranscriptError::Any),
    )]
});

static INVALID_COINBASE_TRANSCRIPT: Lazy<
    Vec<(Request, Result<block::Hash, ExpectedTranscriptError>)>,
> = Lazy::new(|| {
    let header = block::Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap();

    // Test 1: Empty transaction
    let block1 = Block {
        header: header.into(),
        transactions: Vec::new(),
    };

    // Test 2: Transaction at first position is not coinbase
    let mut transactions = Vec::new();
    let tx = zebra_test::vectors::DUMMY_TX1
        .zcash_deserialize_into()
        .unwrap();
    transactions.push(tx);
    let block2 = Block {
        header: header.into(),
        transactions,
    };

    // Test 3: Invalid coinbase position
    let mut block3 =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap();
    assert_eq!(block3.transactions.len(), 1);

    // Extract the coinbase transaction from the block
    let coinbase_transaction = block3.transactions.first().unwrap().clone();

    // Add another coinbase transaction to block
    block3.transactions.push(coinbase_transaction);
    assert_eq!(block3.transactions.len(), 2);

    vec![
        (
            Request::Commit(Arc::new(block1)),
            Err(ExpectedTranscriptError::Any),
        ),
        (
            Request::Commit(Arc::new(block2)),
            Err(ExpectedTranscriptError::Any),
        ),
        (
            Request::Commit(Arc::new(block3)),
            Err(ExpectedTranscriptError::Any),
        ),
    ]
});

// TODO: enable this test after implementing contextual verification
// #[tokio::test]
// #[ignore]
#[allow(dead_code)]
async fn check_transcripts_test() -> Result<(), Report> {
    check_transcripts().await
}

#[allow(dead_code)]
#[spandoc::spandoc]
async fn check_transcripts() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;
    let state_service = zebra_state::init_test(&network).await;

    let transaction = transaction::BlockTxVerifier::new(&network, state_service.clone());
    let transaction = Buffer::new(BoxService::new(transaction), 1);
    let block_verifier = Buffer::new(
        SemanticBlockVerifier::new(&network, state_service.clone(), transaction),
        1,
    );

    for transcript_data in &[
        &VALID_BLOCK_TRANSCRIPT,
        &INVALID_TIME_BLOCK_TRANSCRIPT,
        &INVALID_HEADER_SOLUTION_TRANSCRIPT,
        &INVALID_COINBASE_TRANSCRIPT,
    ] {
        let transcript = Transcript::from(transcript_data.iter().cloned());
        transcript.check(block_verifier.clone()).await.unwrap();
    }
    Ok(())
}

#[test]
fn coinbase_is_first_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block_iter = zebra_test::vectors::BLOCKS.iter();

    for block in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        check::coinbase_is_first(&block)
            .expect("the coinbase in a historical block should be valid");
    }

    Ok(())
}

#[test]
fn difficulty_is_valid_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        difficulty_is_valid_for_network(network)?;
    }

    Ok(())
}

fn difficulty_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (&height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        check::difficulty_is_valid(&block.header, &network, &Height(height), &block.hash())
            .expect("the difficulty from a historical block should be valid");
    }

    Ok(())
}

#[test]
fn difficulty_validation_failure() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Get a block in the mainnet, and mangle its difficulty field
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");
    let height = block.coinbase_height().unwrap();
    let hash = block.hash();

    // Set the difficulty field to an invalid value
    Arc::make_mut(&mut block.header).difficulty_threshold = INVALID_COMPACT_DIFFICULTY;

    // Validate the block
    let result =
        check::difficulty_is_valid(&block.header, &Network::Mainnet, &height, &hash).unwrap_err();
    let expected = BlockError::InvalidDifficulty(height, hash);
    assert_eq!(expected, result);

    // Get a block in the testnet, but tell the validator it's from the mainnet
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_TESTNET_925483_BYTES[..])
            .expect("block should deserialize");
    let block = Arc::try_unwrap(block).expect("block should unwrap");
    let height = block.coinbase_height().unwrap();
    let hash = block.hash();
    let difficulty_threshold = block.header.difficulty_threshold.to_expanded().unwrap();

    // Validate the block as if it is a mainnet block
    let result =
        check::difficulty_is_valid(&block.header, &Network::Mainnet, &height, &hash).unwrap_err();
    let expected = BlockError::TargetDifficultyLimit(
        height,
        hash,
        difficulty_threshold,
        Network::Mainnet,
        Network::Mainnet.target_difficulty_limit(),
    );
    assert_eq!(expected, result);

    // Get a block in the mainnet, but pass an easy hash to the validator
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let block = Arc::try_unwrap(block).expect("block should unwrap");
    let height = block.coinbase_height().unwrap();
    let bad_hash = block::Hash([0xff; 32]);
    let difficulty_threshold = block.header.difficulty_threshold.to_expanded().unwrap();

    // Validate the block
    let result = check::difficulty_is_valid(&block.header, &Network::Mainnet, &height, &bad_hash)
        .unwrap_err();
    let expected =
        BlockError::DifficultyFilter(height, bad_hash, difficulty_threshold, Network::Mainnet);
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn equihash_is_valid_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block_iter = zebra_test::vectors::BLOCKS.iter();

    for block in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        check::equihash_solution_is_valid(&block.header)
            .expect("the equihash solution from a historical block should be valid");
    }

    Ok(())
}

#[test]
fn subsidy_is_valid_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        subsidy_is_valid_for_network(network)?;
    }

    Ok(())
}

fn subsidy_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (&height, block) in block_iter {
        let height = block::Height(height);
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let canopy_activation_height = NetworkUpgrade::Canopy
            .activation_height(&network)
            .expect("Canopy activation height is known");

        // TODO: first halving, second halving, third halving, and very large halvings
        if height >= canopy_activation_height {
            let expected_block_subsidy =
                zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
                    height,
                    &network,
                    Amount::zero(),
                )
                .expect("valid block subsidy");

            subsidy_is_valid(&block, &network, expected_block_subsidy)
                .expect("subsidies should pass for this block");
        }
    }

    Ok(())
}

#[test]
fn coinbase_validation_failure() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // Get a block in the mainnet that is inside the funding stream period,
    // and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    let expected_block_subsidy =
        zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
            block
                .coinbase_height()
                .expect("block should have coinbase height"),
            &network,
            Amount::zero(),
        )
        .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::NoTransactions;
    assert_eq!(expected, result);

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = BlockError::from(SubsidyError::NoCoinbase);
    assert_eq!(expected, result);

    // Get another funding stream block, and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    let expected_block_subsidy =
        zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
            block
                .coinbase_height()
                .expect("block should have coinbase height"),
            &network,
            Amount::zero(),
        )
        .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::CoinbasePosition);
    assert_eq!(expected, result);

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = BlockError::from(SubsidyError::NoCoinbase);
    assert_eq!(expected, result);

    // Get another funding stream, and duplicate the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    // Remove coinbase transaction
    block.transactions.push(
        block
            .transactions
            .first()
            .expect("block has coinbase")
            .clone(),
    );

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::CoinbaseAfterFirst);
    assert_eq!(expected, result);

    let expected_block_subsidy =
        zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
            block
                .coinbase_height()
                .expect("block should have coinbase height"),
            &network,
            Amount::zero(),
        )
        .expect("valid block subsidy");

    subsidy_is_valid(&block, &network, expected_block_subsidy)
        .expect("subsidy does not check for extra coinbase transactions");

    Ok(())
}

#[test]
fn funding_stream_validation() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        funding_stream_validation_for_network(network)?;
    }

    Ok(())
}

fn funding_stream_validation_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(&network)
        .expect("Canopy activation height is known");

    for (&height, block) in block_iter {
        let height = Height(height);

        if height >= canopy_activation_height {
            let block = Block::zcash_deserialize(&block[..]).expect("block should deserialize");
            let expected_block_subsidy =
                zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
                    height,
                    &network,
                    Amount::zero(),
                )
                .expect("valid block subsidy");

            // Validate
            let result = subsidy_is_valid(&block, &network, expected_block_subsidy);
            assert!(result.is_ok());
        }
    }

    Ok(())
}

#[test]
fn funding_stream_validation_failure() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    // Get a block in the mainnet that is inside the funding stream period.
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
            .expect("block should deserialize");

    // Build the new transaction with modified coinbase outputs
    let tx = block
        .transactions
        .first()
        .map(|transaction| {
            let mut output = transaction.outputs()[0].clone();
            output.value = Amount::try_from(i32::MAX).unwrap();
            Transaction::test_v4(
                transaction.inputs().to_vec(),
                vec![output],
                transaction.lock_time().unwrap_or_else(LockTime::unlocked),
                Height(0),
            )
        })
        .unwrap();

    // Build new block
    let transactions: Vec<Arc<zebra_chain::transaction::Transaction>> = vec![Arc::new(tx)];
    let block = Block {
        header: block.header.clone(),
        transactions,
    };

    // Validate it
    let expected_block_subsidy =
        zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
            block
                .coinbase_height()
                .expect("block should have coinbase height"),
            &network,
            Amount::zero(),
        )
        .expect("valid block subsidy");

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy);
    let expected = Err(BlockError::from(SubsidyError::FundingStreamNotFound));
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn miner_fees_validation_success() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    for network in Network::iter() {
        miner_fees_validation_for_network(network)?;
    }

    Ok(())
}

fn miner_fees_validation_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (&height, block) in block_iter {
        let height = Height(height);
        if height > network.slow_start_shift() {
            let block = Block::zcash_deserialize(&block[..]).expect("block should deserialize");
            let coinbase_tx = check::coinbase_is_first(&block)?;

            let expected_block_subsidy =
                zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
                    height,
                    &network,
                    Amount::zero(),
                )?;
            // See [ZIP-1015](https://zips.z.cash/zip-1015).
            let deferred_pool_balance_change =
                match NetworkUpgrade::Canopy.activation_height(&network) {
                    Some(activation_height) if height >= activation_height => {
                        subsidy_is_valid(&block, &network, expected_block_subsidy)?
                    }
                    _other => DeferredPoolBalanceChange::zero(),
                };

            assert!(miner_fees_are_valid(
                &coinbase_tx,
                height,
                // Set the miner fees to a high-enough amount.
                Amount::try_from(MAX_MONEY / 2).unwrap(),
                expected_block_subsidy,
                deferred_pool_balance_change,
                &network,
            )
            .is_ok(),);
        }
    }

    Ok(())
}

#[test]
fn miner_fees_validation_failure() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_347499_BYTES[..])
        .expect("block should deserialize");
    let height = block.coinbase_height().expect("valid coinbase height");
    let expected_block_subsidy =
        zebra_chain::parameters::subsidy::block_subsidy_with_parent_nsm_value_balance(
            height,
            &network,
            Amount::zero(),
        )?;
    // See [ZIP-1015](https://zips.z.cash/zip-1015).
    let deferred_pool_balance_change = match NetworkUpgrade::Canopy.activation_height(&network) {
        Some(activation_height) if height >= activation_height => {
            subsidy_is_valid(&block, &network, expected_block_subsidy)?
        }
        _other => DeferredPoolBalanceChange::zero(),
    };

    assert_eq!(
        miner_fees_are_valid(
            check::coinbase_is_first(&block)?.as_ref(),
            height,
            // Set the miner fee to an invalid amount.
            Amount::zero(),
            expected_block_subsidy,
            deferred_pool_balance_change,
            &network
        ),
        Err(BlockError::from(SubsidyError::InvalidMinerFees))
    );

    Ok(())
}

/// From NU7 activation, the coinbase transaction must claim exactly the fees ZIP 235 leaves in
/// circulation, which are 40% of the block's fees, rounded up. Before it, it must claim them all.

#[test]
fn zip235_coinbase_cant_claim_the_nsm_fee_contribution() -> Result<(), Report> {
    use zebra_chain::{
        amount::{Amount, NonNegative},
        parameters::testnet::{self, ConfiguredActivationHeights},
        transparent,
    };

    let _init_guard = zebra_test::init();

    const NU7_HEIGHT: u32 = 10;

    // Keep this early-upgrade schedule within the monetary cap.
    let network = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(1),
            nu6: Some(1),
            nu6_3: Some(1),
            nu7: Some(NU7_HEIGHT),
            ..Default::default()
        })?
        .clear_funding_streams()
        .to_network()?;

    let fees = Amount::<NonNegative>::try_from(1_001)?;

    let miner_fees_are_valid = |height: Height, claimed_fees: i64| {
        // The rule doesn't depend on the block subsidy.
        let block_subsidy = Amount::<NonNegative>::try_from(625_000_000).expect("valid amount");
        let claimed_fees = Amount::<NonNegative>::try_from(claimed_fees).expect("valid amount");

        let coinbase = Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output {
                value: (block_subsidy + claimed_fees).expect("valid amount"),
                lock_script: transparent::Script::new(&[]),
            }],
            LockTime::unlocked(),
            height,
        );

        check::miner_fees_are_valid(
            &coinbase,
            height,
            fees,
            block_subsidy,
            DeferredPoolBalanceChange::zero(),
            &network,
        )
    };

    let invalid_miner_fees = Err(BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::InvalidMinerFees,
    )));

    let before_nu7 = Height(NU7_HEIGHT - 1);
    assert_eq!(miner_fees_are_valid(before_nu7, 1_001), Ok(()));
    assert_eq!(miner_fees_are_valid(before_nu7, 401), invalid_miner_fees);

    for height in [Height(NU7_HEIGHT), Height(NU7_HEIGHT + 1)] {
        assert_eq!(miner_fees_are_valid(height, 401), Ok(()));

        for claimed_fees in [400, 402, 1_001] {
            assert_eq!(
                miner_fees_are_valid(height, claimed_fees),
                invalid_miner_fees
            );
        }
    }

    Ok(())
}

#[test]
fn time_is_valid_for_historical_blocks() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let block_iter = zebra_test::vectors::BLOCKS.iter();
    let now = Utc::now();

    for block in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // This check is non-deterministic, but the block test vectors are
        // all in the past. So it's unlikely that the test machine will have
        // a clock that's far enough in the past for the test to fail.
        check::time_is_valid_at(
            &block.header,
            now,
            &block
                .coinbase_height()
                .expect("block test vector height should be valid"),
            &block.hash(),
        )
        .expect("the header time from a historical block should be valid, based on the test machine's local clock. Hint: check the test machine's time, date, and timezone.");
    }

    Ok(())
}

#[test]
fn merkle_root_is_valid() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // test all original blocks available, all blocks validate
    for network in Network::iter() {
        merkle_root_is_valid_for_network(network)?;
    }

    // create and test fake blocks with v5 transactions, all blocks fail validation
    for network in Network::iter() {
        merkle_root_fake_v5_for_network(network)?;
    }

    Ok(())
}

fn merkle_root_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (_height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let transaction_hashes = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();

        check::merkle_root_validity(&network, &block, &transaction_hashes)
            .expect("merkle root should be valid for this block");
    }

    Ok(())
}

fn merkle_root_fake_v5_for_network(network: Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (height, block) in block_iter {
        let mut block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // skip blocks that are before overwinter as they will not have a valid consensus branch id
        if *height
            < NetworkUpgrade::Overwinter
                .activation_height(&network)
                .expect("a valid overwinter activation height")
                .0
        {
            continue;
        }

        // convert all transactions from the block to V5
        let transactions: Vec<Arc<Transaction>> = block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(|t| transaction_to_fake_v5(t, &network, Height(*height)))
            .map(Into::into)
            .collect();

        block.transactions = transactions;

        let transaction_hashes = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();

        // Replace the merkle root so that it matches the modified transactions.
        // This test provides some transaction id and merkle root coverage,
        // but we also need to test against zcashd test vectors.
        Arc::make_mut(&mut block.header).merkle_root = transaction_hashes.iter().cloned().collect();

        check::merkle_root_validity(&network, &block, &transaction_hashes)
            .expect("merkle root should be valid for this block");
    }

    Ok(())
}

/// A block whose transaction list contains the same `transaction::Hash`
/// twice must be rejected with [`BlockError::DuplicateTransaction`].
///
/// This guards against Bitcoin's Merkle tree malleability (CVE-2012-2459),
/// where two identical transaction hashes can produce the same Merkle root
/// as the underlying unique list.
#[test]
fn merkle_root_validity_rejects_duplicate_transaction_hash() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES[..])
        .expect("block should deserialize");

    // Duplicate the coinbase transaction so the block contains two
    // transactions with the same hash.
    let duplicate = block
        .transactions
        .first()
        .expect("block has coinbase")
        .clone();
    block.transactions.push(duplicate);

    let transaction_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();

    // Recompute the Merkle root from the duplicated transaction list so
    // that `merkle_root_validity` passes the `BadMerkleRoot` check and
    // reaches the duplicate-hash check.
    Arc::make_mut(&mut block.header).merkle_root = transaction_hashes.iter().cloned().collect();

    let result = check::merkle_root_validity(&network, &block, &transaction_hashes);
    assert_eq!(result, Err(BlockError::DuplicateTransaction));

    Ok(())
}

#[test]
fn legacy_sigops_count_for_large_generated_blocks() {
    let _init_guard = zebra_test::init();

    // We can't test sigops using the transaction verifier, because it looks up UTXOs.

    let block = large_single_transaction_block_many_inputs();
    let mut legacy_sigop_count = 0;
    for tx in block.transactions {
        let tx_sigop_count = tx.sigops().expect("unexpected invalid sigop count");
        assert_eq!(tx_sigop_count, 0);
        legacy_sigop_count += tx_sigop_count;
    }
    // We know this block has no sigops.
    assert_eq!(legacy_sigop_count, 0);

    let block = large_multi_transaction_block();
    let mut sigops = 0;
    for tx in block.transactions {
        let tx_sigop_count = tx.sigops().expect("unexpected invalid sigop count");
        assert_eq!(tx_sigop_count, 1);
        sigops += tx_sigop_count;
    }
    // Test that large blocks can actually fail the sigops check.
    assert!(sigops > MAX_BLOCK_SIGOPS);
}

#[test]
fn legacy_sigops_count_for_historic_blocks() {
    let _init_guard = zebra_test::init();

    // We can't test sigops using the transaction verifier, because it looks up UTXOs.

    for block in zebra_test::vectors::BLOCKS.iter() {
        let mut sigops = 0;

        let block: Block = block
            .zcash_deserialize_into()
            .expect("block test vector is valid");
        for tx in block.transactions {
            sigops += tx.sigops().expect("unexpected invalid sigop count");
        }

        // Test that historic blocks pass the sigops check.
        assert!(sigops <= MAX_BLOCK_SIGOPS);
    }
}

#[test]
fn transaction_expiration_height_validation() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    for network in Network::iter() {
        transaction_expiration_height_for_network(&network)?;
    }

    Ok(())
}

fn transaction_expiration_height_for_network(network: &Network) -> Result<(), Report> {
    let block_iter = network.block_iter();

    for (&height, block) in block_iter {
        let block = Block::zcash_deserialize(&block[..]).expect("block should deserialize");

        for (n, transaction) in block.transactions.iter().enumerate() {
            if n == 0 {
                // coinbase
                let result = transaction::check::coinbase_expiry_height(
                    &Height(height),
                    transaction,
                    network,
                );
                assert!(result.is_ok());
            } else {
                // non coinbase
                let result =
                    transaction::check::non_coinbase_expiry_height(&Height(height), transaction);
                assert!(result.is_ok());
            }
        }
    }

    Ok(())
}

#[test]
fn block_error_misbehavior_scores() {
    use crate::error::BlockError;

    assert_eq!(BlockError::NoTransactions.misbehavior_score(), 100);
    assert_eq!(
        BlockError::BadMerkleRoot {
            actual: zebra_chain::block::merkle::Root([0; 32]),
            expected: zebra_chain::block::merkle::Root([1; 32]),
        }
        .misbehavior_score(),
        100
    );
    assert_eq!(
        BlockError::WrongTransactionConsensusBranchId.misbehavior_score(),
        100
    );
    assert_eq!(
        BlockError::MissingHeight(zebra_chain::block::Hash([0; 32])).misbehavior_score(),
        100
    );
    assert_eq!(BlockError::DuplicateTransaction.misbehavior_score(), 100);
}

/// A block with duplicate transaction hashes must score misbehaviour through both
/// verifier paths, because both of them run `merkle_root_validity()`: the semantic
/// verifier at `block.rs`, and the checkpoint verifier before it queues a block.
///
/// The syncer only forwards a score to the misbehaviour channel when it is
/// non-zero, and it reads that score from the wrapping error, not from
/// [`BlockError`] itself. So the score has to survive the wrappers to have any
/// effect on the peer that advertised the block.
#[test]
fn duplicate_transaction_scores_misbehavior_through_both_verifier_paths() {
    use crate::{checkpoint::VerifyCheckpointError, router::RouterError};

    let _init_guard = zebra_test::init();

    let network = Network::Mainnet;

    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1180900_BYTES[..])
        .expect("block should deserialize");

    // Duplicate the coinbase transaction, then recompute the Merkle root from the
    // duplicated list, so the block passes the `BadMerkleRoot` check and reaches the
    // duplicate-hash check.
    let duplicate = block
        .transactions
        .first()
        .expect("block has coinbase")
        .clone();
    block.transactions.push(duplicate);

    let transaction_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
    Arc::make_mut(&mut block.header).merkle_root = transaction_hashes.iter().cloned().collect();

    let block_error = check::merkle_root_validity(&network, &block, &transaction_hashes)
        .expect_err("duplicate transaction hashes must be rejected");
    assert_eq!(block_error, BlockError::DuplicateTransaction);

    // The semantic verifier path: `BlockError` -> `VerifyBlockError` -> `RouterError`.
    let verify_block_error = VerifyBlockError::from(block_error.clone());
    assert_eq!(verify_block_error.misbehavior_score(), 100);
    assert_eq!(
        RouterError::from(verify_block_error).misbehavior_score(),
        100
    );

    // The checkpoint verifier path: `BlockError` -> `VerifyCheckpointError` -> `RouterError`.
    let checkpoint_error = VerifyCheckpointError::from(block_error);
    assert_eq!(checkpoint_error.misbehavior_score(), 100);
    assert_eq!(RouterError::from(checkpoint_error).misbehavior_score(), 100);
}

#[test]
fn verify_block_error_misbehavior_scores() {
    let dup_err = zebra_state::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zebra_state::KnownBlock::BestChain,
    };
    assert_eq!(VerifyBlockError::Commit(dup_err).misbehavior_score(), 0);
}

/// Duplicate block errors must stay classified as duplicate requests after the
/// state wraps them, so they don't restart the syncer or turn `submitblock`
/// duplicates into rejections.
#[test]
fn state_commit_duplicate_errors_are_duplicate_requests() {
    let duplicate = zs::CommitBlockError::Duplicate {
        hash_or_height: None,
        location: zs::KnownBlock::BestChain,
    };

    // Box the error the same way the state's `CommitSemanticallyVerifiedBlock`
    // handler does. This mirrors the wrapping manually, so it won't fail
    // automatically if the state changes its error type — keep it in sync by hand.
    let source: BoxError = Box::new(zs::CommitSemanticallyVerifiedError::from(duplicate));

    let err = map_commit_error(source, block::Hash([0; 32]));

    assert!(
        matches!(err, VerifyBlockError::Commit(_)),
        "state commit errors must be unwrapped into VerifyBlockError::Commit, got: {err:?}"
    );
    assert!(err.is_duplicate_request());
    assert_eq!(err.misbehavior_score(), 0);
}

/// From the NSM reissuance height, the block verifier leaves the block subsidy,
/// funding stream, and miner fee checks to contextual validation in the state, so it sends a block
/// whose coinbase pays the wrong subsidy to the state to be committed. Before activation it rejects
/// the same block.

#[tokio::test]
async fn nsm_block_verifier_leaves_subsidy_checks_to_the_state() -> Result<(), Report> {
    use zebra_chain::{
        amount::{Amount, NonNegative},
        parameters::testnet::{self, ConfiguredActivationHeights},
        transaction::UnminedTxId,
        transparent,
    };
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();

    const NU7_HEIGHT: u32 = 10;

    // A network reissuing from `deployment_height`.
    let network = |deployment_height: Height| -> Result<Network, Report> {
        let params = testnet::Parameters::build()
            .with_activation_heights(ConfiguredActivationHeights {
                canopy: Some(1),
                nu5: Some(1),
                nu6: Some(1),
                nu6_3: Some(1),
                nu7: Some(NU7_HEIGHT),
                ..Default::default()
            })?
            .with_slow_start_interval(Height::MIN)
            .with_nsm_reissuance_height(deployment_height)
            .with_disable_pow(true)
            .clear_funding_streams()
            .with_lockbox_disbursements(Vec::new());
        Ok(params.to_network()?)
    };

    // A block at `height` whose coinbase pays 1 zatoshi, which is less than the block subsidy.
    let block_at = |height: Height| -> Arc<Block> {
        let mut block =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();

        let coinbase = Transaction::test_v4(
            vec![transparent::Input::Coinbase {
                height,
                data: vec![0],
                sequence: u32::MAX,
            }],
            vec![transparent::Output {
                value: Amount::<NonNegative>::try_from(1).unwrap(),
                lock_script: transparent::Script::new(&[]),
            }],
            LockTime::unlocked(),
            height,
        );
        block.transactions = vec![Arc::new(coinbase)];

        let merkle_root = block.transactions.iter().map(|tx| tx.hash()).collect();
        Arc::make_mut(&mut block.header).merkle_root = merkle_root;

        Arc::new(block)
    };

    let deployed_at_nu7 = network(Height(NU7_HEIGHT))?;
    let deployed_later = network(Height(NU7_HEIGHT + 2))?;

    for (network, height, is_active) in [
        (&deployed_at_nu7, Height(NU7_HEIGHT - 1), false),
        (&deployed_at_nu7, Height(NU7_HEIGHT), true),
        // Between NU7 activation and the deployment height the verifier still checks the subsidy.
        (&deployed_later, Height(NU7_HEIGHT), false),
        (&deployed_later, Height(NU7_HEIGHT + 1), false),
        (&deployed_later, Height(NU7_HEIGHT + 2), true),
    ] {
        let block = block_at(height);
        let hash = block.hash();

        let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
        let mut transaction_verifier: MockService<_, _, _, BoxError> =
            MockService::build().for_unit_tests();

        let verifier =
            SemanticBlockVerifier::new(network, state.clone(), transaction_verifier.clone());
        let verify = tokio::spawn(verifier.oneshot(Request::Commit(block.clone())));

        state
            .expect_request(zs::Request::KnownBlock(hash))
            .await
            .respond(zs::Response::KnownBlock(None));
        state
            .expect_request(zs::Request::MiningCandidate(block.clone()))
            .await
            .respond_error("early mining is disabled in this fixture".into());

        transaction_verifier
            .expect_request_that(|_| true)
            .await
            .respond(transaction::BlockResponse {
                tx_id: UnminedTxId::from(block.transactions[0].as_ref()),
                miner_fee: None,
                sigops: 0,
            });

        if is_active {
            state
                .expect_request_that(|request| {
                    matches!(request, zs::Request::CommitSemanticallyVerifiedBlock(_))
                })
                .await
                .respond(zs::Response::Committed(hash));

            assert_eq!(
                verify
                    .await?
                    .expect("the block verifier leaves the subsidy checks to the state"),
                hash
            );
        } else {
            let err = verify
                .await?
                .expect_err("the coinbase pays the wrong subsidy");
            assert!(
                matches!(
                    err,
                    VerifyBlockError::Block {
                        source: BlockError::Transaction(TransactionError::Subsidy(
                            SubsidyError::InvalidMinerFees
                        ))
                    }
                ),
                "unexpected error: {err:?}"
            );
            state.expect_no_requests().await;
        }
    }

    Ok(())
}

/// Blocks beyond the future-time limit must not become early mining work.
#[tokio::test]
async fn future_time_is_rejected_before_mining_candidate() {
    use zebra_chain::parameters::testnet::Parameters;
    use zebra_test::mock_service::MockService;

    let _init_guard = zebra_test::init();
    // Changing the timestamp invalidates proof of work, so isolate the remaining header checks.
    let network = Parameters::build()
        .with_disable_pow(true)
        .to_network()
        .unwrap();
    let mut block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into::<Block>()
        .unwrap();
    Arc::make_mut(&mut block.header).time = Utc::now() + chrono::Duration::hours(3);
    let block = Arc::new(block);
    let hash = block.hash();
    let mut state: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let transactions: MockService<_, _, _, BoxError> = MockService::build().for_unit_tests();
    let verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions);
    let verify = tokio::spawn(verifier.oneshot(Request::Commit(block)));

    state
        .expect_request(zs::Request::KnownBlock(hash))
        .await
        .respond(zs::Response::KnownBlock(None));
    state.expect_no_requests().await;
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), verify)
        .await
        .expect("future-time rejection must not wait for further verification")
        .expect("the verifier task must not panic");
    assert!(
        matches!(result, Err(VerifyBlockError::Time(_))),
        "{result:?}"
    );
}

/// A same-hash forged block body is only rejected by the state's contextual
/// authorizing-data commitment check, so that rejection is the one signal that
/// attributes the forgery to the peer that served it. Check the suggested
/// misbehaviour score survives every wrapper between the state and the syncer.
#[test]
fn state_auth_commitment_errors_score_the_serving_peer() {
    let commitment_error =
        zebra_chain::block::CommitmentError::InvalidChainHistoryBlockTxAuthCommitment {
            expected: [1; 32],
            actual: [2; 32],
        };
    let commit_error = zs::CommitBlockError::ValidateContextError(Box::new(
        zs::ValidateContextError::InvalidBlockCommitment(commitment_error),
    ));

    // Box the error the same way the state's `CommitSemanticallyVerifiedBlock`
    // handler does. This mirrors the wrapping manually, so it won't fail
    // automatically if the state changes its error type — keep it in sync by hand.
    let source: BoxError = Box::new(zs::CommitSemanticallyVerifiedError::from(commit_error));

    let err = map_commit_error(source, block::Hash([0; 32]));

    assert!(
        matches!(err, VerifyBlockError::Commit(_)),
        "state commit errors must be unwrapped into VerifyBlockError::Commit, got: {err:?}"
    );
    assert!(
        !err.is_duplicate_request(),
        "a forged body is not a duplicate request, so it must not be treated as benign"
    );
    assert_eq!(
        err.misbehavior_score(),
        100,
        "an authorizing-data commitment mismatch must score the peer that served the body"
    );

    // The syncer reads the score through `RouterError`, so it must survive that
    // wrapper too, otherwise the serving peer is never banned.
    let router_err = crate::router::RouterError::from(err);
    assert_eq!(
        router_err.misbehavior_score(),
        100,
        "the score must reach the syncer through RouterError"
    );
}

/// Returns a Regtest network with NU7 activating at `nu7_height`.
fn nu7_network(nu7_height: u32) -> Network {
    Network::new_regtest(
        zebra_chain::parameters::testnet::ConfiguredActivationHeights {
            canopy: Some(1),
            nu5: Some(2),
            nu6: Some(3),
            nu6_1: Some(4),
            nu6_2: Some(5),
            nu6_3: Some(6),
            nu7: Some(nu7_height),
            ..Default::default()
        }
        .into(),
    )
}

/// ZIP 218 limits each shielded pool and their combined cost from NU7 onward.
#[test]
fn zip_218_shielded_action_limits() {
    use zebra_chain::transaction::arbitrary::{fake_bundle_for_branch, fake_v6_transaction};

    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
        .expect("block test vector is valid");

    let orchard_tx =
        zebra_chain::transaction::arbitrary::v5_transactions(Network::Mainnet.block_iter())
            .find(|transaction| {
                transaction.orchard_actions().count() == 2
                    && transaction.joinsplit_count() == 0
                    && transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_none()
            })
            .map(Arc::new)
            .expect("a V5 transaction whose only shielded data is Orchard actions");
    let ironwood_tx = Arc::new(fake_v6_transaction(
        NetworkUpgrade::Nu7,
        None,
        Some(
            fake_bundle_for_branch(
                zcash_protocol::consensus::BranchId::Nu7,
                orchard::ValuePool::Ironwood,
                2,
                1,
            )
            .expect("NU7 supports Ironwood"),
        ),
    ));
    let sapling_tx = zebra_chain::transaction::arbitrary::transactions_from_blocks(
        Network::Mainnet.block_iter(),
    )
    .map(|(_, transaction)| transaction)
    .find(|transaction| {
        transaction.orchard_actions().count() == 0
            && transaction.joinsplit_count() == 0
            && transaction.sapling_spends_count() + transaction.sapling_outputs().count() > 0
    })
    .expect("a transaction whose only shielded data is Sapling");

    let orchard_actions = orchard_tx.orchard_actions().count();
    let sapling_io = sapling_tx.sapling_spends_count() + sapling_tx.sapling_outputs().count();
    let orchard_limit_count = ORCHARD_BLOCK_ACTION_LIMIT / orchard_actions;

    // Exceed the global budget while remaining below each pool's own limit.
    let orchard_count = (ORCHARD_BLOCK_ACTION_LIMIT * 3 / 5).div_ceil(orchard_actions);
    let sapling_count = (SAPLING_BLOCK_IO_LIMIT * 3 / 5).div_ceil(sapling_io);
    assert!(orchard_count * orchard_actions <= ORCHARD_BLOCK_ACTION_LIMIT);
    assert!(sapling_count * sapling_io <= SAPLING_BLOCK_IO_LIMIT);
    assert!(orchard_count * orchard_actions + sapling_count * sapling_io > GLOBAL_SHIELDED_BUDGET);

    for (orchard_count, ironwood_count, sapling_count, height, valid) in [
        // Orchard-only: over the limit before/at activation, and within it at activation.
        (orchard_limit_count + 1, 0, 0, Height(999), true),
        (orchard_limit_count + 1, 0, 0, Height(1_000), false),
        (orchard_limit_count, 0, 0, Height(1_000), true),
        // Ironwood-only: 332 actions before/at activation, then exactly 330 actions.
        (0, 166, 0, Height(999), true),
        (0, 166, 0, Height(1_000), false),
        (0, 165, 0, Height(1_000), true),
        // 200 Orchard plus 200 Ironwood actions exceed their shared global budget.
        (100, 100, 0, Height(999), true),
        (100, 100, 0, Height(1_000), false),
        // Orchard and Sapling: over the budget before/at activation, then remove Sapling.
        (orchard_count, 0, sapling_count, Height(999), true),
        (orchard_count, 0, sapling_count, Height(1_000), false),
        (orchard_count, 0, 0, Height(1_000), true),
    ] {
        // Only the transaction components matter here, not the block's merkle root.
        block.transactions = std::iter::repeat_n(orchard_tx.clone(), orchard_count)
            .chain(std::iter::repeat_n(ironwood_tx.clone(), ironwood_count))
            .chain(std::iter::repeat_n(sapling_tx.clone(), sapling_count))
            .collect();
        let result =
            check::shielded_action_limits_are_valid(&block, &network, height, block.hash());
        if valid {
            assert_eq!(
                result,
                Ok(()),
                "counts {orchard_count}/{ironwood_count}/{sapling_count}, {height:?}"
            );
        } else {
            assert!(
                matches!(result, Err(BlockError::TooManyShieldedActions { .. })),
                "counts {orchard_count}/{ironwood_count}/{sapling_count}, {height:?}: {result:?}",
            );
        }
    }
}

/// Oversized shielded coinbases must be rejected before attempting output recovery.
#[tokio::test]
async fn shielded_limits_precede_coinbase_recovery() {
    use zebra_chain::{transaction::arbitrary::fake_bundle_for_branch, transparent::Input};

    let _init_guard = zebra_test::init();
    let network = nu7_network(1_000);
    let height = Height(1_001);
    let bundle = fake_bundle_for_branch(
        zcash_protocol::consensus::BranchId::Nu7,
        orchard::ValuePool::Ironwood,
        ORCHARD_BLOCK_ACTION_LIMIT + 2,
        1,
    )
    .expect("NU7 supports Ironwood");
    let transaction = Transaction::test_v6_with_bundles(
        NetworkUpgrade::Nu7,
        vec![Input::Coinbase {
            height,
            data: vec![],
            sequence: u32::MAX,
        }],
        vec![],
        LockTime::unlocked(),
        height,
        None,
        Some(bundle),
    );
    assert!(transaction.has_shielded_outputs());
    assert_eq!(
        tx::check::coinbase_outputs_are_decryptable(&transaction, &network, height),
        Err(TransactionError::CoinbaseOutputsNotDecryptable),
    );

    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
        .expect("block test vector is valid");
    block.transactions = vec![Arc::new(transaction)];
    Arc::make_mut(&mut block.header).merkle_root = block
        .transactions
        .iter()
        .map(|transaction| transaction.hash())
        .collect();
    let state = tower::service_fn(|request| async move {
        assert!(matches!(request, zs::Request::KnownBlock(_)));
        Ok::<_, BoxError>(zs::Response::KnownBlock(None))
    });
    let transactions = tower::service_fn(|_| async {
        unreachable!("an oversized block must not queue transaction verification")
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        SemanticBlockVerifier::new(&network, state, transactions)
            .oneshot(Request::CheckProposal(Arc::new(block))),
    )
    .await
    .expect("cheap shielded limit rejection must not stall");
    assert!(
        matches!(
            result,
            Err(VerifyBlockError::Block {
                source: BlockError::TooManyShieldedActions { .. },
            })
        ),
        "{result:?}",
    );
}

/// Pin each ZIP 218 limit's boundary and the global budget's weighting.
#[test]
fn shielded_action_counts_limits() {
    let _init_guard = zebra_test::init();

    for (orchard_actions, ironwood_actions, sapling_io, joinsplits, expected_limit) in [
        (330, 0, 0, 0, None),
        (331, 0, 0, 0, Some(330)),
        (0, 330, 0, 0, None),
        (0, 331, 0, 0, Some(330)),
        (0, 0, 300, 0, None),
        (0, 0, 301, 0, Some(300)),
        (0, 0, 0, 0, None),
        (0, 0, 0, 1, Some(0)),
        // Neither pool is over its own limit, but together they exceed the global budget.
        (200, 200, 0, 0, Some(330)),
        (200, 0, 200, 0, Some(330)),
    ] {
        let counts = ShieldedActionCounts {
            orchard_actions,
            ironwood_actions,
            sapling_io,
            joinsplits,
        };
        assert_eq!(
            counts.exceeded_limit().map(|(_, _, limit)| limit),
            expected_limit,
            "{counts:?}",
        );
    }

    // Each JoinSplit costs two, because it produces two shielded outputs.
    assert_eq!(
        ShieldedActionCounts {
            joinsplits: 1,
            ..Default::default()
        }
        .shielded_cost(),
        2,
    );

    let saturated = ShieldedActionCounts {
        orchard_actions: usize::MAX,
        ironwood_actions: usize::MAX,
        sapling_io: usize::MAX,
        joinsplits: usize::MAX,
    };
    assert_eq!(
        saturated.saturating_add(ShieldedActionCounts {
            orchard_actions: 1,
            ironwood_actions: 1,
            sapling_io: 1,
            joinsplits: 1,
        }),
        saturated,
        "counts must not wrap below a limit",
    );
    assert_eq!(saturated.shielded_cost(), usize::MAX);
}

type ProposalTestState = Buffer<BoxService<zs::Request, zs::Response, BoxError>, zs::Request>;
type ProposalTestVerifier = SemanticBlockVerifier<
    ProposalTestState,
    tower::util::BoxCloneService<tx::BlockRequest, tx::BlockResponse, BoxError>,
>;

/// Populate the cache with a historical block, real state and a counted transaction verifier.
async fn verified_proposal() -> (
    ProposalTestVerifier,
    ProposalTestState,
    Arc<Block>,
    Arc<std::sync::atomic::AtomicUsize>,
    zs::ReadStateService,
) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let network = Network::Mainnet;
    let (state, read_state, _, _) = zs::init(zs::Config::ephemeral(), &network, Height(8), 0).await;
    let state = Buffer::new(state, 10);
    for (_, bytes) in zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS.range(..=8) {
        let block: Arc<Block> = bytes.zcash_deserialize_into().unwrap();
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(block.into()))
            .await
            .unwrap();
    }
    let solved: Arc<Block> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS[&9]
        .zcash_deserialize_into()
        .unwrap();
    let mut proposal = (*solved).clone();
    Arc::make_mut(&mut proposal.header).nonce = [0; 32].into();
    Arc::make_mut(&mut proposal.header).solution = equihash::Solution::for_proposal();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = tower::service_fn({
        let calls = calls.clone();
        move |request: tx::BlockRequest| {
            assert_ne!(
                calls.fetch_add(1, Ordering::SeqCst),
                usize::MAX,
                "a cache hit must not call the transaction verifier"
            );
            std::future::ready(Ok::<_, BoxError>(tx::BlockResponse {
                tx_id: request.transaction.unmined_id(),
                miner_fee: None,
                sigops: 0,
            }))
        }
    });
    let mut verifier = SemanticBlockVerifier::new(
        &network,
        state.clone(),
        tower::util::BoxCloneService::new(counted),
    );
    verifier
        .ready()
        .await
        .unwrap()
        .call(Request::CheckProposal(Arc::new(proposal)))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), solved.transactions.len());
    (verifier, state, solved, calls, read_state)
}

#[tokio::test(flavor = "multi_thread")]
async fn exact_proposal_commits_without_transaction_verification() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let network = Network::new_regtest(Default::default());
        let (state, read_state, _, _) =
            zs::init(zs::Config::ephemeral(), &network, Height(0), 0).await;
        let state = Buffer::new(state, 10);
        let genesis = block::genesis::regtest_genesis_block();
        state
            .clone()
            .oneshot(zs::Request::CommitCheckpointVerifiedBlock(
                genesis.clone().into(),
            ))
            .await
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let transaction_verifier = Buffer::new(
            transaction::BlockTxVerifier::new(&network, state.clone()),
            1,
        );
        let transactions = tower::service_fn({
            let calls = calls.clone();
            move |request: tx::BlockRequest| {
                assert_ne!(
                    calls.fetch_add(1, Ordering::SeqCst),
                    usize::MAX,
                    "a cache hit must not call the transaction verifier"
                );
                transaction_verifier.clone().oneshot(request)
            }
        });
        let mut verifier = SemanticBlockVerifier::new(&network, state.clone(), transactions);
        // Cache hits extend finalized and non-finalized parents; changed time forces full checks.
        for height in [Height(1), Height(2), Height(3)] {
            let zs::ReadResponse::ChainInfo(info) = read_state
                .clone()
                .oneshot(zs::ReadRequest::ChainInfo)
                .await
                .unwrap()
            else {
                unreachable!()
            };
            let coinbase = Arc::new(Transaction::test_v4(
                vec![transparent::Input::Coinbase {
                    height,
                    data: vec![0],
                    sequence: u32::MAX,
                }],
                vec![transparent::Output::new(
                    zebra_chain::parameters::subsidy::block_subsidy(height, &network).unwrap(),
                    transparent::Script::new(&[0x51]),
                )],
                LockTime::unlocked(),
                height,
            ));
            let proposal = Block {
                header: Arc::new(block::Header {
                    version: 4,
                    previous_block_hash: info.tip_hash,
                    merkle_root: [coinbase.hash()].into_iter().collect(),
                    commitment_bytes: info
                        .chain_history_root
                        .map(|root| root.bytes_in_serialized_order())
                        .unwrap_or([0; 32])
                        .into(),
                    time: info.cur_time.into(),
                    difficulty_threshold: info.expected_difficulty,
                    nonce: [0; 32].into(),
                    solution: equihash::Solution::for_proposal(),
                }),
                transactions: vec![coinbase],
            };
            let mut solved = proposal.clone();
            Arc::make_mut(&mut solved.header).nonce = [1; 32].into();
            assert_ne!(proposal.hash(), solved.hash());
            let solved = Arc::new(solved);
            calls.store(0, Ordering::SeqCst);
            verifier
                .ready()
                .await
                .unwrap()
                .call(Request::CheckProposal(Arc::new(proposal)))
                .await
                .unwrap();
            let cached = height != Height(3);
            let solved = if cached {
                solved
            } else {
                let mut changed = (*solved).clone();
                Arc::make_mut(&mut changed.header).time += chrono::Duration::seconds(1);
                Arc::new(changed)
            };
            calls.store(if cached { usize::MAX } else { 0 }, Ordering::SeqCst);
            assert_eq!(
                verifier
                    .ready()
                    .await
                    .unwrap()
                    .call(Request::Commit(solved.clone()))
                    .await
                    .unwrap(),
                solved.hash()
            );
            if !cached {
                assert_eq!(calls.load(Ordering::SeqCst), solved.transactions.len());
            }
            assert_eq!(
                state
                    .clone()
                    .oneshot(zs::Request::Block(solved.hash().into()))
                    .await
                    .unwrap(),
                zs::Response::Block(Some(solved)),
            );
        }
    })
    .await
    .expect("proposal reuse must not stall");
}

/// Cached solves publish early work while committing, then release the candidate guard.
#[tokio::test(flavor = "multi_thread")]
async fn cached_proposal_announces_candidate_until_commit_finishes() {
    let _init_guard = zebra_test::init();
    // Stay below the candidate lifetime so expiry cannot imitate guard release.
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let (mut verifier, state, solved, calls, read_state) = verified_proposal().await;
        let zs::ReadResponse::MiningCandidateChanges(mut candidates) = read_state
            .clone()
            .oneshot(zs::ReadRequest::MiningCandidateChanges)
            .await
            .unwrap()
        else {
            panic!("state must return a candidate subscription");
        };
        let mut requests: zebra_test::mock_service::MockService<
            zs::Request,
            zs::Response,
            _,
            BoxError,
        > = zebra_test::mock_service::MockService::build().for_unit_tests();
        verifier.state_service = Buffer::new(BoxService::new(requests.clone()), 10);
        calls.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        let verification = tokio::spawn(verifier.oneshot(Request::Commit(solved.clone())));
        requests
            .expect_request(zs::Request::KnownBlock(solved.hash()))
            .await
            .respond(zs::Response::KnownBlock(None));
        let announcement = requests
            .expect_request(zs::Request::MiningCandidate(solved.clone()))
            .await;
        announcement.respond(
            state
                .clone()
                .oneshot(zs::Request::MiningCandidate(solved.clone()))
                .await
                .unwrap(),
        );
        let commit = requests
            .expect_request_that(|request| {
                matches!(request, zs::Request::CommitContextuallyVerifiedBlock(_))
            })
            .await;
        candidates.changed().await;
        assert!(matches!(
            read_state
                .clone()
                .oneshot(zs::ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            zs::ReadResponse::MiningCandidate(Some(info)) if info.tip_hash == solved.hash()
        ));
        assert!(
            !verification.is_finished(),
            "the cached commit is still held"
        );
        candidates.mark_seen();
        commit.respond(zs::Response::Committed(solved.hash()));
        assert_eq!(verification.await.unwrap().unwrap(), solved.hash());
        candidates.changed().await;
        assert!(matches!(
            read_state
                .oneshot(zs::ReadRequest::MiningCandidate)
                .await
                .unwrap(),
            zs::ReadResponse::MiningCandidate(None)
        ));
    })
    .await
    .expect("cached candidate publication and guard release must precede candidate expiry");
}

#[tokio::test(flavor = "multi_thread")]
async fn cached_proposal_still_requires_proof_of_work() {
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (verifier, _, solved, calls, _) = verified_proposal().await;
        calls.store(usize::MAX, std::sync::atomic::Ordering::SeqCst);
        let mut invalid = (*solved).clone();
        Arc::make_mut(&mut invalid.header).solution = equihash::Solution::for_proposal();
        assert_eq!(
            proposal_key_and_size(&invalid).0,
            proposal_key_and_size(&solved).0,
        );
        let error = verifier
            .oneshot(Request::Commit(Arc::new(invalid)))
            .await
            .unwrap_err();
        assert_eq!(error.misbehavior_score(), 100);
        assert!(matches!(
            error,
            VerifyBlockError::Equihash { .. }
                | VerifyBlockError::Block {
                    source: BlockError::DifficultyFilter(..)
                }
        ));
    })
    .await
    .expect("invalid proof of work must not stall");
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_proposals_require_transaction_verification() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _init_guard = zebra_test::init();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (cached, _, _, _, _) = verified_proposal().await;
        let contextual = cached.proposals.borrow().front().unwrap().2.clone();
        let network = zebra_chain::parameters::testnet::Parameters::build()
            .with_disable_pow(true)
            .to_network()
            .unwrap();
        let original = network
            .block_iter()
            .map(|(_, bytes)| bytes.zcash_deserialize_into::<Block>().unwrap())
            .find(|block| {
                block
                    .transactions
                    .iter()
                    .any(|tx| tx.version() == 5 && !tx.is_coinbase() && tx.has_transparent_inputs())
            })
            .expect("vectors contain a V5 transparent spend");
        let key = proposal_key_and_size(&original).0;
        for change in 0..4 {
            let mut block = original.clone();
            match change {
                0 => {
                    let mut inputs = block.transactions[0].inputs();
                    let transparent::Input::Coinbase { data, .. } = &mut inputs[0] else {
                        panic!("first transaction is coinbase")
                    };
                    data.push(1);
                    block.transactions[0] = Arc::new(
                        (*block.transactions[0])
                            .clone()
                            .with_transparent_inputs(inputs),
                    );
                }
                1 => Arc::make_mut(&mut block.header).time += chrono::Duration::seconds(1),
                2 => {
                    let tx = block
                        .transactions
                        .iter_mut()
                        .find(|tx| {
                            tx.version() == 5 && !tx.is_coinbase() && tx.has_transparent_inputs()
                        })
                        .unwrap();
                    let hash = tx.hash();
                    let mut inputs = tx.inputs();
                    let transparent::Input::PrevOut { unlock_script, .. } = &mut inputs[0] else {
                        panic!("non-coinbase inputs spend outputs")
                    };
                    *unlock_script = transparent::Script::new(&[0]);
                    *tx = Arc::new((**tx).clone().with_transparent_inputs(inputs));
                    assert_eq!(
                        tx.hash(),
                        hash,
                        "authorizing bytes are excluded from V5 txids"
                    );
                }
                _ => Arc::make_mut(&mut block.header).previous_block_hash = block::Hash([9; 32]),
            }
            Arc::make_mut(&mut block.header).merkle_root =
                block.transactions.iter().map(|tx| tx.hash()).collect();
            assert_ne!(proposal_key_and_size(&block).0, key);
            let count = block.transactions.len();
            let calls = Arc::new(AtomicUsize::new(0));
            let transactions = tower::service_fn({
                let calls = calls.clone();
                move |request: tx::BlockRequest| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::future::ready(Ok::<_, BoxError>(tx::BlockResponse {
                        tx_id: request.transaction.unmined_id(),
                        miner_fee: None,
                        sigops: 0,
                    }))
                }
            });
            let state = tower::service_fn(|request| async move {
                match request {
                    zs::Request::KnownBlock(_) => Ok(zs::Response::KnownBlock(None)),
                    zs::Request::MiningCandidate(_) => Ok(zs::Response::MiningCandidate(None)),
                    zs::Request::CommitSemanticallyVerifiedBlock(_) => {
                        Err::<_, BoxError>("fully verified".into())
                    }
                    _ => panic!("a changed proposal must miss"),
                }
            });
            let verifier = SemanticBlockVerifier::new(&network, state, transactions);
            verifier.proposals.send_modify(|proposals| {
                proposals.push_front((
                    key,
                    original.header.previous_block_hash,
                    contextual.clone(),
                    contextual.retained_bytes(),
                ))
            });
            assert!(verifier
                .oneshot(Request::Commit(Arc::new(block)))
                .await
                .is_err());
            assert_eq!(
                calls.load(Ordering::SeqCst),
                count,
                "mutation {change} must fully verify"
            );
        }
    })
    .await
    .expect("cache miss verification must not stall");
}

/// Full-sized proposals of one-input/one-P2PKH-output payments keep the eight-entry cache window.
#[test]
fn full_size_transparent_proposals_keep_eight_entries() {
    use std::collections::HashMap;

    use zebra_chain::amount::DeferredPoolBalanceChange;

    let _init_guard = zebra_test::init();
    let mut block: Block = zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES
        .zcash_deserialize_into()
        .unwrap();
    block.transactions = vec![block.transactions[0].clone()];
    // A minimal P2PKH payment: one input spending OP_TRUE with no scriptSig, one standard output.
    let mut p2pkh = vec![0x76, 0xa9, 20];
    p2pkh.extend([0; 20]);
    p2pkh.extend([0x88, 0xac]);
    let output = transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&p2pkh));
    let payment = |index, padding| {
        Transaction::test_v4(
            vec![transparent::Input::PrevOut {
                outpoint: transparent::OutPoint {
                    hash: [7; 32].into(),
                    index,
                },
                unlock_script: transparent::Script::new(&vec![0x61; padding]), // OP_NOP
                sequence: u32::MAX,
            }],
            vec![output.clone()],
            LockTime::unlocked(),
            Height(0),
        )
    };
    let transaction_bytes = payment(0, 0).zcash_serialized_size();
    assert_eq!(transaction_bytes, 104);
    let limit = usize::try_from(block::MAX_BLOCK_BYTES).unwrap();
    // The transaction count grows from a one-byte CompactSize to three bytes.
    let available = limit - block.zcash_serialized_size() - 2;
    let count = available / transaction_bytes;
    assert!((253..=u16::MAX.into()).contains(&(count + 1)));
    block.transactions.reserve_exact(count);
    let mut spent = HashMap::with_capacity(count);
    for index in 0..count {
        let index = u32::try_from(index).unwrap();
        let padding = if index == 0 {
            available % transaction_bytes
        } else {
            0
        };
        let transaction = payment(index, padding);
        spent.insert(
            transaction.spent_outpoints().next().unwrap(),
            transparent::OrderedUtxo::new(
                transparent::Output::new(1.try_into().unwrap(), transparent::Script::new(&[0x51])),
                Height(1_046_399),
                1,
            ),
        );
        block.transactions.push(Arc::new(transaction));
    }
    Arc::make_mut(&mut block.header).merkle_root =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    assert_eq!(block.zcash_serialized_size(), limit);
    let block = Arc::new(block);
    let prepared = zs::SemanticallyVerifiedBlock::with_hash(block.clone(), block.hash());
    let contextual = zs::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
        prepared,
        spent,
        DeferredPoolBalanceChange::zero(),
        &Network::Mainnet,
        Default::default(),
    )
    .unwrap();
    let retained_bytes = contextual.retained_bytes();
    let proposals = watch::channel(VecDeque::new()).0;
    for key in 0..8 {
        cache_proposal(
            &proposals,
            [key; 32],
            block.header.previous_block_hash,
            Box::new(contextual.clone()),
        );
    }
    assert_eq!(
        proposals.borrow().len(),
        8,
        "full-size P2PKH proposals keep eight slots: {count} payments, {retained_bytes} charged bytes each"
    );
}

/// Prior-output scripts share the proposal budget even when their spending inputs are small.
#[test]
fn proposal_cache_spent_outputs_share_the_byte_bound() {
    use std::collections::HashMap;

    use zebra_chain::amount::DeferredPoolBalanceChange;

    let _init_guard = zebra_test::init();
    let ordinary: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let contextual = |block: Arc<Block>, spent| {
        zs::ContextuallyVerifiedBlock::with_block_and_spent_utxos(
            zs::SemanticallyVerifiedBlock::with_hash(block.clone(), block.hash()),
            spent,
            DeferredPoolBalanceChange::zero(),
            &Network::Mainnet,
            Default::default(),
        )
        .unwrap()
    };
    let proposals = watch::channel(VecDeque::new()).0;
    let small = contextual(ordinary.clone(), HashMap::new());
    for key in 0..8 {
        cache_proposal(
            &proposals,
            [key; 32],
            ordinary.header.previous_block_hash,
            Box::new(small.clone()),
        );
    }
    assert_eq!(
        proposals.borrow().len(),
        8,
        "ordinary proposals keep eight slots"
    );

    // Spendable scripts below the 10,000-byte limit, as in the orphan queue retention regression.
    let mut script = Vec::new();
    for _ in 0..19 {
        script.extend([0x4d, 0xf4, 0x01]); // OP_PUSHDATA2, 500 bytes
        script.extend([0; 500]);
        script.push(0x75); // OP_DROP
    }
    script.push(0x51); // OP_TRUE
    let spent_count = MAX_PROPOSAL_BYTES / (8 * script.len()) + 1;
    let spent: HashMap<_, _> = (0..spent_count)
        .map(|index| {
            let index = u32::try_from(index).unwrap();
            (
                transparent::OutPoint {
                    hash: [7; 32].into(),
                    index,
                },
                transparent::OrderedUtxo::new(
                    transparent::Output::new(
                        1.try_into().unwrap(),
                        transparent::Script::new(&script),
                    ),
                    Height(0),
                    1,
                ),
            )
        })
        .collect();
    let spending = ordinary.transactions[0]
        .as_ref()
        .clone()
        .with_transparent_inputs(
            spent
                .keys()
                .map(|outpoint| transparent::Input::PrevOut {
                    outpoint: *outpoint,
                    unlock_script: transparent::Script::new(&[]),
                    sequence: u32::MAX,
                })
                .collect(),
        )
        .with_transparent_outputs(vec![transparent::Output::new(
            i64::try_from(spent_count).unwrap().try_into().unwrap(),
            transparent::Script::new(&[0x51]),
        )]);
    let mut large = (*ordinary).clone();
    large.transactions.push(Arc::new(spending));
    Arc::make_mut(&mut large.header).merkle_root =
        large.transactions.iter().map(|tx| tx.hash()).collect();
    let large = Arc::new(large);
    assert!(8 * large.zcash_serialized_size() < MAX_PROPOSAL_BYTES);
    assert!(8 * spent.len() * script.len() > MAX_PROPOSAL_BYTES);
    let large_contextual = contextual(large.clone(), spent);
    let charged = large_contextual.retained_bytes();
    for key in 0..8 {
        cache_proposal(
            &proposals,
            [key; 32],
            large.header.previous_block_hash,
            Box::new(large_contextual.clone()),
        );
    }
    let proposals = proposals.borrow();
    assert!(!proposals.is_empty());
    assert!(
        proposals.len() < 8,
        "spent scripts must trigger eviction before the count cap"
    );
    assert!(proposals.len() * charged <= MAX_PROPOSAL_BYTES);
    assert_eq!(
        proposals.front().unwrap().0,
        [7; 32],
        "retain the newest proposal"
    );
}

#[test]
fn proposal_key_counts_actual_wire_size() {
    let _init_guard = zebra_test::init();
    let mainnet: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    for block in [mainnet, block::genesis::regtest_genesis_block()] {
        let (key, size) = proposal_key_and_size(&block);
        assert_eq!(size, block.zcash_serialized_size());
        let mut normalized = (*block).clone();
        let header = Arc::make_mut(&mut normalized.header);
        header.nonce = [0; 32].into();
        header.solution = equihash::Solution::for_proposal();
        let (normalized_key, normalized_size) = proposal_key_and_size(&normalized);
        assert_eq!(key, normalized_key);
        assert_eq!(normalized_size, normalized.zcash_serialized_size());
    }
}
