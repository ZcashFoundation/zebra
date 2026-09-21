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
    parameters::{subsidy::block_subsidy, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{arbitrary::transaction_to_fake_v5, LockTime, Transaction},
    work::difficulty::{ParameterDifficulty as _, INVALID_COMPACT_DIFFICULTY},
};
use zebra_script::Sigops;
use zebra_test::transcript::{ExpectedTranscriptError, Transcript};

use crate::{block::check::subsidy_is_valid, transaction};

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

/// Returns a Regtest network with NU7 activating at `nu7_height`.
///
/// NU7 is unscheduled on Mainnet and the default Testnet, so the ZIP 218 action limits are
/// unreachable there.
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

/// Returns `block`, with its transaction list replaced by `count` copies of `transaction`.
///
/// The block's merkle root no longer matches its transactions, which is irrelevant to the ZIP 218
/// action limits: they only count the shielded components of the block's transactions.
fn with_repeated_transaction(
    mut block: Block,
    transaction: Arc<Transaction>,
    count: usize,
) -> Block {
    block.transactions = std::iter::repeat_n(transaction, count).collect();
    block
}

/// ZIP 218: a block whose Orchard actions exceed `OrchardBlockActionLimit` is rejected from NU7,
/// and the same block is accepted below the activation height.
#[test]
fn zip_218_orchard_action_limit() {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
        .expect("block test vector is valid");

    // A transaction whose only shielded data is Orchard actions, so the Orchard limit is the
    // only one the repeated block can exceed.
    let transaction =
        zebra_chain::transaction::arbitrary::v5_transactions(Network::Mainnet.block_iter())
            .find(|transaction| {
                transaction.orchard_actions().count() > 0
                    && transaction.joinsplit_count() == 0
                    && transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_none()
            })
            .map(Arc::new)
            .expect("a V5 transaction whose only shielded data is Orchard actions");

    let actions_per_transaction = transaction.orchard_actions().count();

    // Just over the limit, and just under it.
    let over = ORCHARD_BLOCK_ACTION_LIMIT / actions_per_transaction + 1;
    let under = ORCHARD_BLOCK_ACTION_LIMIT / actions_per_transaction;

    let over_block = with_repeated_transaction(block.clone(), transaction.clone(), over);
    let under_block = with_repeated_transaction(block, transaction, under);

    let hash = over_block.hash();

    assert!(
        matches!(
            check::shielded_action_limits_are_valid(&over_block, &network, Height(1_000), hash),
            Err(BlockError::TooManyShieldedActions { .. })
        ),
        "a block over the Orchard action limit must be rejected from NU7",
    );

    assert!(
        check::shielded_action_limits_are_valid(&under_block, &network, Height(1_000), hash)
            .is_ok(),
        "a block under the Orchard action limit must be accepted at NU7",
    );

    // The limit does not apply before NU7 activates.
    assert!(
        check::shielded_action_limits_are_valid(&over_block, &network, Height(999), hash).is_ok(),
        "the ZIP 218 action limits must not apply before NU7",
    );
}

/// ZIP 218: the global shielded budget applies even when no single pool is over its own limit.
#[test]
fn zip_218_global_shielded_budget() {
    let _init_guard = zebra_test::init();

    let network = nu7_network(1_000);
    let block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1046400_BYTES[..])
        .expect("block test vector is valid");

    // A transaction whose only shielded data is Orchard actions.
    let orchard_tx =
        zebra_chain::transaction::arbitrary::v5_transactions(Network::Mainnet.block_iter())
            .find(|transaction| {
                transaction.orchard_actions().count() > 0
                    && transaction.joinsplit_count() == 0
                    && transaction.sapling_spends_count() == 0
                    && transaction.sapling_outputs().next().is_none()
            })
            .map(Arc::new)
            .expect("a V5 transaction whose only shielded data is Orchard actions");

    // A transaction whose only shielded data is Sapling spends and outputs.
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

    // Fill a little over half of each per-pool limit, so neither is exceeded on its own but their
    // sum is over the global budget of 330.
    let orchard_count = (ORCHARD_BLOCK_ACTION_LIMIT * 3 / 5).div_ceil(orchard_actions);
    let sapling_count = (SAPLING_BLOCK_IO_LIMIT * 3 / 5).div_ceil(sapling_io);

    let orchard_total = orchard_count * orchard_actions;
    let sapling_total = sapling_count * sapling_io;

    assert!(orchard_total <= ORCHARD_BLOCK_ACTION_LIMIT);
    assert!(sapling_total <= SAPLING_BLOCK_IO_LIMIT);
    assert!(
        orchard_total + sapling_total > GLOBAL_SHIELDED_BUDGET,
        "the test block must exceed the global budget while staying under both pool limits",
    );

    let mut over_block = block.clone();
    over_block.transactions = std::iter::repeat_n(orchard_tx.clone(), orchard_count)
        .chain(std::iter::repeat_n(sapling_tx.clone(), sapling_count))
        .collect();
    let hash = over_block.hash();

    assert!(
        matches!(
            check::shielded_action_limits_are_valid(&over_block, &network, Height(1_000), hash),
            Err(BlockError::TooManyShieldedActions { .. })
        ),
        "a block over the global shielded budget must be rejected from NU7, \
         even though neither pool is over its own limit",
    );

    // The same block is valid before NU7 activates.
    assert!(
        check::shielded_action_limits_are_valid(&over_block, &network, Height(999), hash).is_ok(),
        "the global shielded budget must not apply before NU7",
    );

    // Dropping the Sapling transactions brings the block back under the budget.
    let mut under_block = block;
    under_block.transactions = std::iter::repeat_n(orchard_tx, orchard_count).collect();

    assert!(
        check::shielded_action_limits_are_valid(&under_block, &network, Height(1_000), hash)
            .is_ok(),
        "a block within the global shielded budget must be accepted at NU7",
    );
}
