//! Tests for block verification

#![allow(clippy::unwrap_in_result)]

use std::sync::Arc;

use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use proptest::{
    arbitrary::any,
    strategy::{Strategy, ValueTree},
    test_runner::TestRunner,
};
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    amount::{DeferredPoolBalanceChange, MAX_MONEY},
    block::{
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
        },
        Block, Height,
    },
    parameters::{
        subsidy::block_subsidy, testnet, Network, NetworkUpgrade, GLOBAL_SHIELDED_BUDGET,
        ORCHARD_BLOCK_ACTION_LIMIT, SAPLING_BLOCK_IO_LIMIT, SPROUT_BLOCK_JOINSPLIT_LIMIT,
    },
    primitives::Groth16Proof,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{
        arbitrary::{
            fake_v5_with_orchard_actions, fake_v5_with_sapling_outputs, transaction_to_fake_v5,
        },
        JoinSplitData, LockTime, Transaction,
    },
    work::difficulty::{ParameterDifficulty as _, INVALID_COMPACT_DIFFICULTY},
};
use zebra_script::Sigops;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{block::check::subsidy_is_valid, error::TransactionError, transaction};

use super::*;

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

    let transaction = transaction::Verifier::new_for_tests(&network, state_service.clone());
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
                zebra_chain::parameters::subsidy::block_subsidy(height, &network)
                    .expect("valid block subsidy");

            check::subsidy_is_valid(&block, &network, expected_block_subsidy)
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

    let expected_block_subsidy = zebra_chain::parameters::subsidy::block_subsidy(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::NoTransactions;
    assert_eq!(expected, result);

    let result = check::subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(SubsidyError::NoCoinbase));
    assert_eq!(expected, result);

    // Get another funding stream block, and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    let expected_block_subsidy = zebra_chain::parameters::subsidy::block_subsidy(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::CoinbasePosition);
    assert_eq!(expected, result);

    let result = check::subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(SubsidyError::NoCoinbase));
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

    let expected_block_subsidy = zebra_chain::parameters::subsidy::block_subsidy(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    check::subsidy_is_valid(&block, &network, expected_block_subsidy)
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
                zebra_chain::parameters::subsidy::block_subsidy(height, &network)
                    .expect("valid block subsidy");

            // Validate
            let result = check::subsidy_is_valid(&block, &network, expected_block_subsidy);
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
            Transaction::V4 {
                inputs: transaction.inputs().to_vec(),
                outputs: vec![output],
                lock_time: transaction.lock_time().unwrap_or_else(LockTime::unlocked),
                expiry_height: Height(0),
                joinsplit_data: None,
                sapling_shielded_data: None,
            }
        })
        .unwrap();

    // Build new block
    let transactions: Vec<Arc<zebra_chain::transaction::Transaction>> = vec![Arc::new(tx)];
    let block = Block {
        header: block.header.clone(),
        transactions,
    };

    // Validate it
    let expected_block_subsidy = zebra_chain::parameters::subsidy::block_subsidy(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    let result = check::subsidy_is_valid(&block, &network, expected_block_subsidy);
    let expected = Err(BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::FundingStreamNotFound,
    )));
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

            let expected_block_subsidy = block_subsidy(height, &network)?;
            // See [ZIP-1015](https://zips.z.cash/zip-1015).
            let deferred_pool_balance_change =
                match NetworkUpgrade::Canopy.activation_height(&network) {
                    Some(activation_height) if height >= activation_height => {
                        subsidy_is_valid(&block, &network, expected_block_subsidy)?
                    }
                    _other => DeferredPoolBalanceChange::zero(),
                };

            assert!(check::miner_fees_are_valid(
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
    let expected_block_subsidy = block_subsidy(height, &network)?;
    // See [ZIP-1015](https://zips.z.cash/zip-1015).
    let deferred_pool_balance_change = match NetworkUpgrade::Canopy.activation_height(&network) {
        Some(activation_height) if height >= activation_height => {
            subsidy_is_valid(&block, &network, expected_block_subsidy)?
        }
        _other => DeferredPoolBalanceChange::zero(),
    };

    assert_eq!(
        check::miner_fees_are_valid(
            check::coinbase_is_first(&block)?.as_ref(),
            height,
            // Set the miner fee to an invalid amount.
            Amount::zero(),
            expected_block_subsidy,
            deferred_pool_balance_change,
            &network
        ),
        Err(BlockError::Transaction(TransactionError::Subsidy(
            SubsidyError::InvalidMinerFees,
        )))
    );

    Ok(())
}

#[cfg(all(feature = "tx_v6", zcash_unstable = "zip235"))]
#[test]
fn miner_fees_validation_fails_when_zip233_amount_is_zero() -> Result<(), Report> {
    use zebra_chain::parameters::testnet::{
        self, ConfiguredActivationHeights, ConfiguredFundingStreams,
    };

    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let zip233_amount = Amount::zero();
    let expected_block_subsidy = 100_000_000.try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = DeferredPoolBalanceChange::new(Amount::zero());

    let regtest = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        })
        .unwrap()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(10)),
            recipients: None,
        }])
        .to_network()
        .unwrap();

    let network_upgrade = NetworkUpgrade::Nu7;
    let height = network_upgrade
        .activation_height(&regtest)
        .expect("failed to get the activation height for Nu7");

    let coinbase_tx = Transaction::V6 {
        network_upgrade,
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        zip233_amount,
        inputs: vec![],
        outputs: vec![transparent::Output::new(
            transparent_value_balance,
            zebra_chain::transparent::Script::new(&[]),
        )],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };
    assert_eq!(
        check::miner_fees_are_valid(
            &coinbase_tx,
            height,
            block_miner_fees,
            expected_block_subsidy,
            expected_deferred_amount,
            &regtest,
        ),
        Err(BlockError::Transaction(TransactionError::Subsidy(
            SubsidyError::InvalidZip233Amount
        )))
    );

    Ok(())
}

#[cfg(all(feature = "tx_v6", zcash_unstable = "zip235"))]
#[test]
fn miner_fees_validation_succeeds_when_zip233_amount_is_correct() -> Result<(), Report> {
    use zebra_chain::parameters::testnet::{
        self, ConfiguredActivationHeights, ConfiguredFundingStreams,
    };

    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let zip233_amount = 600.try_into().unwrap();
    let expected_block_subsidy = (100_000_600).try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = DeferredPoolBalanceChange::new(Amount::zero());

    let regtest = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        })
        .unwrap()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(10)),
            recipients: None,
        }])
        .to_network()
        .unwrap();

    let network_upgrade = NetworkUpgrade::Nu7;
    let height = network_upgrade
        .activation_height(&regtest)
        .expect("failed to get the activation height for Nu7");

    let coinbase_tx = Transaction::V6 {
        network_upgrade,
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        zip233_amount,
        inputs: vec![],
        outputs: vec![transparent::Output::new(
            transparent_value_balance,
            zebra_chain::transparent::Script::new(&[]),
        )],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    assert_eq!(
        check::miner_fees_are_valid(
            &coinbase_tx,
            height,
            block_miner_fees,
            expected_block_subsidy,
            expected_deferred_amount,
            &regtest,
        ),
        Ok(())
    );

    Ok(())
}

#[cfg(all(feature = "tx_v6", zcash_unstable = "zip235"))]
#[test]
fn miner_fees_validation_fails_when_zip233_amount_is_incorrect() -> Result<(), Report> {
    use zebra_chain::parameters::testnet::{
        self, ConfiguredActivationHeights, ConfiguredFundingStreams,
    };

    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let zip233_amount = 500.try_into().unwrap();
    let expected_block_subsidy = (100_000_500).try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = DeferredPoolBalanceChange::new(Amount::zero());

    let regtest = testnet::Parameters::build()
        .with_slow_start_interval(Height::MIN)
        .with_activation_heights(ConfiguredActivationHeights {
            nu7: Some(1),
            ..Default::default()
        })
        .unwrap()
        .with_funding_streams(vec![ConfiguredFundingStreams {
            height_range: Some(Height(1)..Height(10)),
            recipients: None,
        }])
        .to_network()
        .unwrap();

    let network_upgrade = NetworkUpgrade::Nu7;
    let height = network_upgrade
        .activation_height(&regtest)
        .expect("failed to get the activation height for Nu7");

    let coinbase_tx = Transaction::V6 {
        network_upgrade,
        lock_time: LockTime::unlocked(),
        expiry_height: height,
        zip233_amount,
        inputs: vec![],
        outputs: vec![transparent::Output::new(
            transparent_value_balance,
            zebra_chain::transparent::Script::new(&[]),
        )],
        sapling_shielded_data: None,
        orchard_shielded_data: None,
    };

    assert_eq!(
        check::miner_fees_are_valid(
            &coinbase_tx,
            height,
            block_miner_fees,
            expected_block_subsidy,
            expected_deferred_amount,
            &regtest,
        ),
        Err(BlockError::Transaction(TransactionError::Subsidy(
            SubsidyError::InvalidZip233Amount
        )))
    );

    Ok(())
}

/// Smoke test for the per-block shielded action limits introduced by the draft
/// "Shorter Block Target Spacing" ZIP. The limits only apply at and after NU7
/// activation; pre-NU7 the check is a no-op, so historical block test vectors
/// must all pass, and a (very small) historical block treated as if it were at
/// NU7 activation must still pass because its shielded counts are all zero or
/// well below the limits.
#[test]
fn shielded_action_limits_smoke() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    // Pre-NU7: the function is a no-op, so every historical block must pass.
    for block in zebra_test::vectors::BLOCKS.iter() {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");
        check::shielded_action_limits_are_valid(
            &block.transactions,
            block.coinbase_height().unwrap(),
            &Network::Mainnet,
        )
        .expect("historical pre-NU7 block must satisfy the shielded action limits");
    }

    // Post-NU7: on a testnet with NU7 active at height 1, the mainnet genesis
    // block (no shielded data) evaluated "as if" at NU7 activation must pass,
    // because its action counts are all zero.
    let nu7_testnet = nu7_active_testnet();
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
        .expect("mainnet genesis deserializes");

    check::shielded_action_limits_are_valid(&block.transactions, Height(1), &nu7_testnet)
        .expect("a block with no shielded data must satisfy the post-NU7 limits");

    Ok(())
}

#[test]
fn shielded_action_limits_activate_at_nu7_height() {
    let network = nu7_activation_testnet(2);
    let over_limit_tx = fake_v5_with_orchard_actions(limit_plus_one(ORCHARD_BLOCK_ACTION_LIMIT));

    check::shielded_action_limits_are_valid([over_limit_tx.clone()].iter(), Height(1), &network)
        .expect("pre-NU7 shielded action limits are not active");

    let err = check::shielded_action_limits_are_valid([over_limit_tx].iter(), Height(2), &network)
        .expect_err("NU7 activation height must enforce shielded action limits");

    assert_eq!(
        err,
        TransactionError::OrchardActionsExceedBlockLimit {
            actions: ORCHARD_BLOCK_ACTION_LIMIT.saturating_add(1),
            limit: ORCHARD_BLOCK_ACTION_LIMIT,
        }
    );
}

#[test]
fn shielded_action_limits_accept_per_pool_limits() {
    check::shielded_action_limits_are_valid(
        [fake_v5_with_orchard_actions(
            usize::try_from(ORCHARD_BLOCK_ACTION_LIMIT).expect("limit fits in usize"),
        )]
        .iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect("Orchard actions exactly at the NU7 limit must pass");

    check::shielded_action_limits_are_valid(
        [fake_v5_with_sapling_outputs(
            usize::try_from(SAPLING_BLOCK_IO_LIMIT).expect("limit fits in usize"),
        )]
        .iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect("Sapling inputs and outputs exactly at the NU7 limit must pass");

    check::shielded_action_limits_are_valid(
        [fake_v4_with_sprout_joinsplits(
            usize::try_from(SPROUT_BLOCK_JOINSPLIT_LIMIT).expect("limit fits in usize"),
        )]
        .iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect("Sprout JoinSplits exactly at the NU7 limit must pass");
}

#[test]
fn shielded_action_limits_accept_global_budget_limit() {
    let orchard_actions = usize::try_from(
        GLOBAL_SHIELDED_BUDGET
            .checked_sub(2)
            .expect("global shielded budget is at least one JoinSplit cost"),
    )
    .expect("limit fits in usize");

    check::shielded_action_limits_are_valid(
        [
            fake_v5_with_orchard_actions(orchard_actions),
            fake_v4_with_sprout_joinsplits(1),
        ]
        .iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect("combined shielded cost exactly at the NU7 global budget must pass");
}

#[test]
fn shielded_action_limits_reject_sapling_over_limit() {
    let count = limit_plus_one(SAPLING_BLOCK_IO_LIMIT);
    let err = check::shielded_action_limits_are_valid(
        [fake_v5_with_sapling_outputs(count)].iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect_err("Sapling spends and outputs above the NU7 per-block limit must fail");

    assert_eq!(
        err,
        TransactionError::SaplingIOsExceedBlockLimit {
            ios: SAPLING_BLOCK_IO_LIMIT + 1,
            limit: SAPLING_BLOCK_IO_LIMIT,
        }
    );
}

#[test]
fn shielded_action_limits_reject_sprout_over_limit() {
    let count = limit_plus_one(SPROUT_BLOCK_JOINSPLIT_LIMIT);
    let err = check::shielded_action_limits_are_valid(
        [fake_v4_with_sprout_joinsplits(count)].iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect_err("Sprout JoinSplits above the NU7 per-block limit must fail");

    assert_eq!(
        err,
        TransactionError::SproutJoinSplitsExceedBlockLimit {
            joinsplits: SPROUT_BLOCK_JOINSPLIT_LIMIT + 1,
            limit: SPROUT_BLOCK_JOINSPLIT_LIMIT,
        }
    );
}

#[test]
fn shielded_action_limits_reject_global_budget_over_limit() {
    let err = check::shielded_action_limits_are_valid(
        [
            fake_v5_with_orchard_actions(
                usize::try_from(ORCHARD_BLOCK_ACTION_LIMIT).expect("limit fits in usize"),
            ),
            fake_v5_with_sapling_outputs(1),
        ]
        .iter(),
        Height(1),
        &nu7_active_testnet(),
    )
    .expect_err("combined shielded cost above the NU7 global budget must fail");

    assert_eq!(
        err,
        TransactionError::ShieldedCostExceedsBlockBudget {
            cost: GLOBAL_SHIELDED_BUDGET + 1,
            limit: GLOBAL_SHIELDED_BUDGET,
        }
    );
}

fn nu7_active_testnet() -> Network {
    nu7_activation_testnet(1)
}

fn nu7_activation_testnet(nu7_activation_height: u32) -> Network {
    testnet::Parameters::build()
        .with_slow_start_interval(Height(0))
        .with_activation_heights(testnet::ConfiguredActivationHeights {
            nu7: Some(nu7_activation_height),
            ..Default::default()
        })
        .expect("activation heights are valid")
        .clear_funding_streams()
        .to_network()
        .expect("configured testnet is valid")
}

fn limit_plus_one(limit: u32) -> usize {
    limit
        .checked_add(1)
        .expect("test limit plus one fits in u32")
        .try_into()
        .expect("test limit fits in usize")
}

fn fake_v4_with_sprout_joinsplits(count: usize) -> Arc<Transaction> {
    let mut runner = TestRunner::default();
    let mut joinsplit_data = any::<JoinSplitData<Groth16Proof>>()
        .new_tree(&mut runner)
        .expect("sprout JoinSplit data strategy is valid")
        .current();
    let rest_len = count
        .checked_sub(1)
        .expect("sprout JoinSplit test count is at least one");
    joinsplit_data.rest = vec![joinsplit_data.first.clone(); rest_len];

    Arc::new(Transaction::V4 {
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: LockTime::unlocked(),
        expiry_height: Height(100),
        joinsplit_data: Some(joinsplit_data),
        sapling_shielded_data: None,
    })
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
