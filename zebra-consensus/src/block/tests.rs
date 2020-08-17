//! Tests for block verification

use super::*;

use chrono::Utc;
use color_eyre::eyre::{eyre, Report};
use once_cell::sync::Lazy;

use zebra_chain::block::{self, Block};
use zebra_chain::serialization::{ZcashDeserialize, ZcashDeserializeInto};
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
async fn check_transcripts_test() -> Result<(), Report> {
    check_transcripts().await
}

#[spandoc::spandoc]
async fn check_transcripts() -> Result<(), Report> {
    zebra_test::init();
    let state_service = zebra_state::in_memory::init();
    let block_verifier = super::init(state_service.clone());

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
