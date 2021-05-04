//! Tests for block verification

use crate::parameters::SLOW_START_INTERVAL;

use super::*;

use std::sync::Arc;

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use tower::buffer::Buffer;

use zebra_chain::{
    block::{self, Block, Height},
    parameters::Network,
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
    transaction::{arbitrary::transaction_to_fake_v5, Transaction},
    work::difficulty::{ExpandedDifficulty, INVALID_COMPACT_DIFFICULTY},
};
use zebra_test::transcript::{TransError, Transcript};

static VALID_BLOCK_TRANSCRIPT: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let block: Arc<_> =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap()
                .into();
        let hash = Ok(block.as_ref().into());
        vec![(block, hash)]
    });

static INVALID_TIME_BLOCK_TRANSCRIPT: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let mut block: Block =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap();

        // Modify the block's time
        // Changing the block header also invalidates the header hashes, but
        // those checks should be performed later in validation, because they
        // are more expensive.
        let three_hours_in_the_future = Utc::now()
            .checked_add_signed(chrono::Duration::hours(3))
            .ok_or_else(|| eyre!("overflow when calculating 3 hours in the future"))
            .unwrap();
        block.header.time = three_hours_in_the_future;

        vec![(Arc::new(block), Err(TransError::Any))]
    });

static INVALID_HEADER_SOLUTION_TRANSCRIPT: Lazy<
    Vec<(Arc<Block>, Result<block::Hash, TransError>)>,
> = Lazy::new(|| {
    let mut block: Block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]).unwrap();

    // Change nonce to something invalid
    block.header.nonce = [0; 32];

    vec![(Arc::new(block), Err(TransError::Any))]
});

static INVALID_COINBASE_TRANSCRIPT: Lazy<Vec<(Arc<Block>, Result<block::Hash, TransError>)>> =
    Lazy::new(|| {
        let header =
            block::Header::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap();

        // Test 1: Empty transaction
        let block1 = Block {
            header,
            transactions: Vec::new(),
        };

        // Test 2: Transaction at first position is not coinbase
        let mut transactions = Vec::new();
        let tx = zebra_test::vectors::DUMMY_TX1
            .zcash_deserialize_into()
            .unwrap();
        transactions.push(tx);
        let block2 = Block {
            header,
            transactions,
        };

        // Test 3: Invalid coinbase position
        let mut block3 =
            Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])
                .unwrap();
        assert_eq!(block3.transactions.len(), 1);

        // Extract the coinbase transaction from the block
        let coinbase_transaction = block3.transactions.get(0).unwrap().clone();

        // Add another coinbase transaction to block
        block3.transactions.push(coinbase_transaction);
        assert_eq!(block3.transactions.len(), 2);

        vec![
            (Arc::new(block1), Err(TransError::Any)),
            (Arc::new(block2), Err(TransError::Any)),
            (Arc::new(block3), Err(TransError::Any)),
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
    zebra_test::init();

    let network = Network::Mainnet;
    let state_service = Buffer::new(
        zebra_state::init(zebra_state::Config::ephemeral(), network),
        1,
    );

    let block_verifier = Buffer::new(BlockVerifier::new(network, state_service.clone()), 1);

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
    zebra_test::init();

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
    zebra_test::init();

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
    zebra_test::init();
    use crate::error::*;

    // Get a block in the mainnet, and mangle its difficulty field
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");
    let height = block.coinbase_height().unwrap();
    let hash = block.hash();

    // Set the difficulty field to an invalid value
    block.header.difficulty_threshold = INVALID_COMPACT_DIFFICULTY;

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
    zebra_test::init();

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
    zebra_test::init();

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

        // TODO: first halving, second halving, third halving, and very large halvings
        if block::Height(height) > SLOW_START_INTERVAL {
            check::subsidy_is_valid(&block, network).expect("subsidies should pass for this block");
        }
    }

    Ok(())
}

#[test]
fn coinbase_validation_failure() -> Result<(), Report> {
    zebra_test::init();
    use crate::error::*;

    let network = Network::Mainnet;

    // Get a block in the mainnet that is inside the founders reward period,
    // and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
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

    // Get another founders reward block, and delete the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..])
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

    // Get another founders reward block, and duplicate the coinbase transaction
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    // Remove coinbase transaction
    block.transactions.push(
        block
            .transactions
            .get(0)
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
fn founders_reward_validation_failure() -> Result<(), Report> {
    zebra_test::init();
    use crate::error::*;
    use zebra_chain::transaction::Transaction;

    let network = Network::Mainnet;

    // Get a block in the mainnet that is inside the founders reward period.
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");

    // Build the new transaction with modified coinbase outputs
    let tx = block
        .transactions
        .get(0)
        .map(|transaction| Transaction::V3 {
            inputs: transaction.inputs().to_vec(),
            outputs: vec![transaction.outputs()[0].clone()],
            lock_time: transaction.lock_time(),
            expiry_height: transaction.expiry_height().unwrap(),
            joinsplit_data: None,
        })
        .unwrap();

    // Build new block
    let transactions: Vec<Arc<zebra_chain::transaction::Transaction>> = vec![Arc::new(tx)];
    let block = Block {
        header: block.header,
        transactions,
    };

    // Validate it
    let result = check::subsidy_is_valid(&block, network).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::FoundersRewardNotFound,
    ));
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn time_is_valid_for_historical_blocks() -> Result<(), Report> {
    zebra_test::init();

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
    zebra_test::init();

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

    for (_height, block) in block_iter {
        let mut block = block
            .zcash_deserialize_into::<Block>()
            .expect("block is structurally valid");

        // convert all transactions from the block to V5
        let transactions: Vec<Arc<Transaction>> = block
            .transactions
            .iter()
            .map(AsRef::as_ref)
            .map(transaction_to_fake_v5)
            .map(Into::into)
            .collect();

        block.transactions = transactions;

        let transaction_hashes = block
            .transactions
            .iter()
            .map(|tx| tx.hash())
            .collect::<Vec<_>>();

        check::merkle_root_validity(network, &block, &transaction_hashes).expect_err(
            "network upgrade by block height do not match transaction ConsensusBranchId",
        );
    }

    Ok(())
}
