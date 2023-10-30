//! Tests for block verification

use std::sync::Arc;

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use tower::{buffer::Buffer, util::BoxService};

use zebra_chain::{
    amount::{Amount, MAX_MONEY},
    block::{
        self,
        tests::generate::{
            large_multi_transaction_block, large_single_transaction_block_many_inputs,
        },
        Block, Height,
    },
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{arbitrary::transaction_to_fake_v5, LockTime, Transaction},
    work::difficulty::{ExpandedDifficulty, INVALID_COMPACT_DIFFICULTY},
};
use zebra_script::CachedFfiTransaction;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{parameters::SLOW_START_SHIFT, transaction};

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
    let state_service = zebra_state::init_test(network);

    let transaction = transaction::Verifier::new(network, state_service.clone());
    let transaction = Buffer::new(BoxService::new(transaction), 1);
    let block_verifier = Buffer::new(
        SemanticBlockVerifier::new(network, state_service.clone(), transaction),
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

    difficulty_is_valid_for_network(Network::Mainnet)?;
    difficulty_is_valid_for_network(Network::Testnet)?;

    Ok(())
}

fn difficulty_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (&height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        check::difficulty_is_valid(&block.header, network, &Height(height), &block.hash())
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
        check::difficulty_is_valid(&block.header, Network::Mainnet, &height, &hash).unwrap_err();
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
        check::difficulty_is_valid(&block.header, Network::Mainnet, &height, &hash).unwrap_err();
    let expected = BlockError::TargetDifficultyLimit(
        height,
        hash,
        difficulty_threshold,
        Network::Mainnet,
        ExpandedDifficulty::target_difficulty_limit(Network::Mainnet),
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
    let result = check::difficulty_is_valid(&block.header, Network::Mainnet, &height, &bad_hash)
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

    subsidy_is_valid_for_network(Network::Mainnet)?;
    subsidy_is_valid_for_network(Network::Testnet)?;

    Ok(())
}

fn subsidy_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (&height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let canopy_activation_height = NetworkUpgrade::Canopy
            .activation_height(network)
            .expect("Canopy activation height is known");

        // TODO: first halving, second halving, third halving, and very large halvings
        if block::Height(height) >= canopy_activation_height {
            check::subsidy_is_valid(&block, network).expect("subsidies should pass for this block");
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

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::NoTransactions;
    assert_eq!(expected, result);

    // Validate the block using subsidy_is_valid
    let result = check::subsidy_is_valid(&block, network).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(SubsidyError::NoCoinbase));
    assert_eq!(expected, result);

    // Get another funding stream block, and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046401_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block using coinbase_is_first
    let result = check::coinbase_is_first(&block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::CoinbasePosition);
    assert_eq!(expected, result);

    // Validate the block using subsidy_is_valid
    let result = check::subsidy_is_valid(&block, network).unwrap_err();
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

    // Validate the block using subsidy_is_valid, which does not detect this error
    check::subsidy_is_valid(&block, network)
        .expect("subsidy does not check for extra coinbase transactions");

    Ok(())
}

#[test]
fn funding_stream_validation() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    funding_stream_validation_for_network(Network::Mainnet)?;
    funding_stream_validation_for_network(Network::Testnet)?;

    Ok(())
}

fn funding_stream_validation_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    let canopy_activation_height = NetworkUpgrade::Canopy
        .activation_height(network)
        .expect("Canopy activation height is known");

    for (&height, block) in block_iter {
        if Height(height) >= canopy_activation_height {
            let block = Block::zcash_deserialize(&block[..]).expect("block should deserialize");

            // Validate
            let result = check::subsidy_is_valid(&block, network);
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
    let result = check::subsidy_is_valid(&block, network);
    let expected = Err(BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::FundingStreamNotFound,
    )));
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn miner_fees_validation_success() -> Result<(), Report> {
    let _init_guard = zebra_test::init();

    miner_fees_validation_for_network(Network::Mainnet)?;
    miner_fees_validation_for_network(Network::Testnet)?;

    Ok(())
}

fn miner_fees_validation_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (&height, block) in block_iter {
        if Height(height) > SLOW_START_SHIFT {
            let block = Block::zcash_deserialize(&block[..]).expect("block should deserialize");

            // fake the miner fee to a big amount
            let miner_fees = Amount::try_from(MAX_MONEY / 2).unwrap();

            // Validate
            let result = check::miner_fees_are_valid(&block, network, miner_fees);
            assert!(result.is_ok());
        }
    }

    Ok(())
}

#[test]
fn miner_fees_validation_failure() -> Result<(), Report> {
    let _init_guard = zebra_test::init();
    let network = Network::Mainnet;

    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_347499_BYTES[..])
            .expect("block should deserialize");

    // fake the miner fee to a small amount
    let miner_fees = Amount::zero();

    // Validate
    let result = check::miner_fees_are_valid(&block, network, miner_fees);

    let expected = Err(BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::InvalidMinerFees,
    )));
    assert_eq!(expected, result);

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
    merkle_root_is_valid_for_network(Network::Mainnet)?;
    merkle_root_is_valid_for_network(Network::Testnet)?;

    // create and test fake blocks with v5 transactions, all blocks fail validation
    merkle_root_fake_v5_for_network(Network::Mainnet)?;
    merkle_root_fake_v5_for_network(Network::Testnet)?;

    Ok(())
}

fn merkle_root_is_valid_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (_height, block) in block_iter {
        let block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        let transaction_hashes = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();

        check::merkle_root_validity(network, &block, &transaction_hashes)
            .expect("merkle root should be valid for this block");
    }

    Ok(())
}

fn merkle_root_fake_v5_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

    for (height, block) in block_iter {
        let mut block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // skip blocks that are before overwinter as they will not have a valid consensus branch id
        if *height
            < NetworkUpgrade::Overwinter
                .activation_height(network)
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
            .map(|t| transaction_to_fake_v5(t, network, Height(*height)))
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

        check::merkle_root_validity(network, &block, &transaction_hashes)
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

    transaction_expiration_height_for_network(Network::Mainnet)?;
    transaction_expiration_height_for_network(Network::Testnet)?;

    Ok(())
}

fn transaction_expiration_height_for_network(network: Network) -> Result<(), Report> {
    let block_iter = match network {
        Network::Mainnet => zebra_test::vectors::MAINNET_BLOCKS.iter(),
        Network::Testnet => zebra_test::vectors::TESTNET_BLOCKS.iter(),
    };

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
