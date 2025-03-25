//! Tests for block verification

use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    amount::MAX_MONEY,
    block::{
        error, subsidy,
        subsidy::general::block_subsidy_pre_nsm,
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
        },
        Block, Height,
    },
    error::CoinbaseTransactionError,
    parameters::{subsidy::FundingStreamReceiver, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{arbitrary::transaction_to_fake_v5, LockTime, Transaction},
    work::difficulty::{ParameterDifficulty as _, INVALID_COMPACT_DIFFICULTY},
};
use zebra_script::CachedFfiTransaction;
use zebra_state::check::{coinbase_is_first, subsidy_is_valid, transaction_miner_fees_are_valid};
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{block::check, difficulty_is_valid, transaction};

use super::*;

#[cfg(any(zcash_unstable = "zip234", zcash_unstable = "zip235"))]
use zebra_state::check::miner_fees_are_valid;

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
    let state_service = zebra_state::init_test(&network);

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

        coinbase_is_first(&block).expect("the coinbase in a historical block should be valid");
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

        difficulty_is_valid(&block.header, &network, &Height(height), &block.hash())
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
    let result = difficulty_is_valid(&block.header, &Network::Mainnet, &height, &hash).unwrap_err();
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
    let result = difficulty_is_valid(&block.header, &Network::Mainnet, &height, &hash).unwrap_err();
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
    let result =
        difficulty_is_valid(&block.header, &Network::Mainnet, &height, &bad_hash).unwrap_err();
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
            let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(height, &network)
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

    let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = coinbase_is_first(&block).unwrap_err();
    let expected = CoinbaseTransactionError::Block(error::BlockError::NoTransactions);
    assert_eq!(expected, result);

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = SubsidyError::NoCoinbase;
    assert_eq!(expected, result);

    // Get another funding stream block, and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = coinbase_is_first(&block).unwrap_err();
    let expected = CoinbaseTransactionError::Position;
    assert_eq!(expected, result);

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy).unwrap_err();
    let expected = SubsidyError::NoCoinbase;
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
    let result = coinbase_is_first(&block).unwrap_err();
    let expected = CoinbaseTransactionError::AfterFirst;
    assert_eq!(expected, result);

    let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
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
            let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(height, &network)
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
    let expected_block_subsidy = subsidy::general::block_subsidy_pre_nsm(
        block
            .coinbase_height()
            .expect("block should have coinbase height"),
        &network,
    )
    .expect("valid block subsidy");

    let result = subsidy_is_valid(&block, &network, expected_block_subsidy);
    let expected = Err(SubsidyError::FundingStreamNotFound);
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
            let coinbase_tx = coinbase_is_first(
                &Block::zcash_deserialize(&block[..]).expect("block should deserialize"),
            )?;

            let expected_block_subsidy = block_subsidy_pre_nsm(height, &network)?;

            // See [ZIP-1015](https://zips.z.cash/zip-1015).
            let expected_deferred_amount = subsidy::funding_streams::funding_stream_values(
                height,
                &network,
                expected_block_subsidy,
            )
            .expect("we always expect a funding stream hashmap response even if empty")
            .remove(&FundingStreamReceiver::Deferred)
            .unwrap_or_default();

            assert!(transaction_miner_fees_are_valid(
                &coinbase_tx,
                height,
                // Set the miner fees to a high-enough amount.
                Amount::try_from(MAX_MONEY / 2).unwrap(),
                expected_block_subsidy,
                expected_deferred_amount,
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
    let expected_block_subsidy = block_subsidy_pre_nsm(height, &network)?;
    // See [ZIP-1015](https://zips.z.cash/zip-1015).
    let expected_deferred_amount: Amount<zebra_chain::amount::NonNegative> =
        subsidy::funding_streams::funding_stream_values(height, &network, expected_block_subsidy)
            .expect("we always expect a funding stream hashmap response even if empty")
            .remove(&FundingStreamReceiver::Deferred)
            .unwrap_or_default();

    assert_eq!(
        transaction_miner_fees_are_valid(
            coinbase_is_first(&block)?.as_ref(),
            height,
            // Set the miner fee to an invalid amount.
            Amount::zero(),
            expected_block_subsidy,
            expected_deferred_amount,
            &network
        ),
        Err(SubsidyError::InvalidMinerFees)
    );

    Ok(())
}

#[cfg(zcash_unstable = "zip235")]
#[test]
fn miner_fees_validation_fails_when_zip233_amount_is_zero() -> Result<(), Report> {
    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let sapling_value_balance = Amount::zero();
    let orchard_value_balance = Amount::zero();
    let zip233_amount = Amount::zero();
    let expected_block_subsidy = 100_000_000.try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = Amount::zero();

    assert_eq!(
        miner_fees_are_valid(
            transparent_value_balance,
            sapling_value_balance,
            orchard_value_balance,
            zip233_amount,
            expected_block_subsidy,
            block_miner_fees,
            expected_deferred_amount,
            NetworkUpgrade::Nu7
        ),
        Err(SubsidyError::InvalidZip233Amount)
    );

    Ok(())
}

#[cfg(zcash_unstable = "zip234")]
#[test]
fn miner_fees_validation_succeeds_when_zip233_amount_is_correct() -> Result<(), Report> {
    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let sapling_value_balance = Amount::zero();
    let orchard_value_balance = Amount::zero();
    let zip233_amount = 600.try_into().unwrap();
    let expected_block_subsidy = (100_000_600).try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = Amount::zero();

    assert_eq!(
        miner_fees_are_valid(
            transparent_value_balance,
            sapling_value_balance,
            orchard_value_balance,
            zip233_amount,
            expected_block_subsidy,
            block_miner_fees,
            expected_deferred_amount,
            NetworkUpgrade::Nu7
        ),
        Ok(())
    );

    Ok(())
}

#[cfg(zcash_unstable = "zip235")]
#[test]
fn miner_fees_validation_fails_when_zip233_amount_is_incorrect() -> Result<(), Report> {
    let transparent_value_balance = 100_001_000.try_into().unwrap();
    let sapling_value_balance = Amount::zero();
    let orchard_value_balance = Amount::zero();
    let zip233_amount = 500.try_into().unwrap();
    let expected_block_subsidy = (100_000_500).try_into().unwrap();
    let block_miner_fees = 1000.try_into().unwrap();
    let expected_deferred_amount = Amount::zero();

    assert_eq!(
        miner_fees_are_valid(
            transparent_value_balance,
            sapling_value_balance,
            orchard_value_balance,
            zip233_amount,
            expected_block_subsidy,
            block_miner_fees,
            expected_deferred_amount,
            NetworkUpgrade::Nu7
        ),
        Err(SubsidyError::InvalidZip233Amount)
    );

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

#[test]
fn legacy_sigops_count_for_large_generated_blocks() {
    let _init_guard = zebra_test::init();

    // We can't test sigops using the transaction verifier, because it looks up UTXOs.

    let block = large_single_transaction_block_many_inputs();
    let mut legacy_sigop_count = 0;
    for transaction in block.transactions {
        let cached_ffi_transaction =
            Arc::new(CachedFfiTransaction::new(transaction.clone(), Vec::new()));
        let tx_sigop_count = cached_ffi_transaction.legacy_sigop_count();
        assert_eq!(tx_sigop_count, Ok(0));
        legacy_sigop_count += tx_sigop_count.expect("unexpected invalid sigop count");
    }
    // We know this block has no sigops.
    assert_eq!(legacy_sigop_count, 0);

    let block = large_multi_transaction_block();
    let mut legacy_sigop_count = 0;
    for transaction in block.transactions {
        let cached_ffi_transaction =
            Arc::new(CachedFfiTransaction::new(transaction.clone(), Vec::new()));
        let tx_sigop_count = cached_ffi_transaction.legacy_sigop_count();
        assert_eq!(tx_sigop_count, Ok(1));
        legacy_sigop_count += tx_sigop_count.expect("unexpected invalid sigop count");
    }
    // Test that large blocks can actually fail the sigops check.
    assert!(legacy_sigop_count > MAX_BLOCK_SIGOPS);
}

#[test]
fn legacy_sigops_count_for_historic_blocks() {
    let _init_guard = zebra_test::init();

    // We can't test sigops using the transaction verifier, because it looks up UTXOs.

    for block in zebra_test::vectors::BLOCKS.iter() {
        let mut legacy_sigop_count = 0;

        let block: Block = block
            .zcash_deserialize_into()
            .expect("block test vector is valid");
        for transaction in block.transactions {
            let cached_ffi_transaction =
                Arc::new(CachedFfiTransaction::new(transaction.clone(), Vec::new()));
            legacy_sigop_count += cached_ffi_transaction
                .legacy_sigop_count()
                .expect("unexpected invalid sigop count");
        }
        // Test that historic blocks pass the sigops check.
        assert!(legacy_sigop_count <= MAX_BLOCK_SIGOPS);
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
