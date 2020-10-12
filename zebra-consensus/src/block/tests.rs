//! Tests for block verification

use crate::parameters::SLOW_START_INTERVAL;

use super::*;

use std::sync::Arc;

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;
use tower::buffer::Buffer;

use zebra_chain::block::{self, Block};
use zebra_chain::{
    parameters::{Network, NetworkUpgrade},
    serialization::{ZcashDeserialize, ZcashDeserializeInto},
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

#[tokio::test]
// TODO: enable this test after implementing contextual verification
#[ignore]
async fn check_transcripts_test() -> Result<(), Report> {
    check_transcripts().await
}

#[spandoc::spandoc]
async fn check_transcripts() -> Result<(), Report> {
    zebra_test::init();

    let network = Network::Mainnet;
    let state_service = zebra_state::init(zebra_state::Config::ephemeral(), network);

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
fn time_check_past_block() {
    // This block is also verified as part of the BlockVerifier service
    // tests.
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let now = Utc::now();

    // This check is non-deterministic, but BLOCK_MAINNET_415000 is
    // a long time in the past. So it's unlikely that the test machine
    // will have a clock that's far enough in the past for the test to
    // fail.
    check::time_is_valid_at(
        &block.header,
        now,
        &block
            .coinbase_height()
            .expect("block test vector height should be valid"),
        &block.hash(),
    )
    .expect("the header time from a mainnet block should be valid");
}

#[test]
fn subsidy_is_valid_test() -> Result<(), Report> {
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
        if block::Height(height) > SLOW_START_INTERVAL
            && block::Height(height) < NetworkUpgrade::Canopy.activation_height(network).unwrap()
        {
            check::subsidy_is_valid(network, &block)
                .expect("subsidies should pass for this block");
        }
    }
    Ok(())
}

#[test]
fn nocoinbase_validation_failure() -> Result<(), Report> {
    use crate::error::*;

    let network = Network::Mainnet;

    // Get a header form a block in the mainnet that is inside the founders reward period.
    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
            .expect("block should deserialize");
    let mut block = Arc::try_unwrap(block).expect("block should unwrap");

    // Remove coinbase transaction
    block.transactions.remove(0);

    // Validate the block
    let result = check::subsidy_is_valid(network, &block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(SubsidyError::NoCoinbase));
    assert_eq!(expected, result);

    Ok(())
}

#[test]
fn founders_reward_validation_failure() -> Result<(), Report> {
    use crate::error::*;
    use zebra_chain::transaction::Transaction;

    let network = Network::Mainnet;

    // Get a header from a block in the mainnet that is inside the founders reward period.
    let header =
        block::Header::zcash_deserialize(&zebra_test::vectors::HEADER_MAINNET_415000_BYTES[..])
            .unwrap();

    // From the same block get the coinbase transaction
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
    let mut transactions: Vec<Arc<zebra_chain::transaction::Transaction>> = Vec::new();
    transactions.push(Arc::new(tx));
    let block = Block {
        header,
        transactions,
    };

    // Validate it
    let result = check::subsidy_is_valid(network, &block).unwrap_err();
    let expected = BlockError::Transaction(TransactionError::Subsidy(
        SubsidyError::FoundersRewardNotFound,
    ));
    assert_eq!(expected, result);

    Ok(())
}
