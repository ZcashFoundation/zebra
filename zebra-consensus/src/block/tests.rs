//! Tests for block verification

use super::*;

use chrono::{Duration, Utc};
use color_eyre::eyre::eyre;
use color_eyre::eyre::Report;
use std::sync::Arc;
use tower::{util::ServiceExt, Service};

use zebra_chain::block::Block;
use zebra_chain::block::BlockHeader;
use zebra_chain::serialization::ZcashDeserialize;
use zebra_chain::transaction::Transaction;

#[tokio::test]
async fn verify_test() -> Result<(), Report> {
    verify().await
}

#[spandoc::spandoc]
async fn verify() -> Result<(), Report> {
    zebra_test::init();

    let block =
        Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;
    let hash: BlockHeaderHash = block.as_ref().into();

    let state_service = Box::new(zebra_state::in_memory::init());
    let mut block_verifier = super::init(state_service);

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Verify the block
    let verify_response = ready_verifier_service
        .call(block.clone())
        .await
        .map_err(|e| eyre!(e))?;

    assert_eq!(verify_response, hash);

    Ok(())
}

#[tokio::test]
async fn verify_fail_future_time_test() -> Result<(), Report> {
    verify_fail_future_time().await
}

#[spandoc::spandoc]
async fn verify_fail_future_time() -> Result<(), Report> {
    zebra_test::init();

    let mut block =
        <Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;

    let state_service = zebra_state::in_memory::init();
    let mut block_verifier = super::init(state_service.clone());

    // Modify the block's time
    // Changing the block header also invalidates the header hashes, but
    // those checks should be performed later in validation, because they
    // are more expensive.
    let three_hours_in_the_future = Utc::now()
        .checked_add_signed(Duration::hours(3))
        .ok_or("overflow when calculating 3 hours in the future")
        .map_err(|e| eyre!(e))?;
    block.header.time = three_hours_in_the_future;

    let arc_block: Arc<Block> = block.into();

    /// SPANDOC: Make sure the verifier service is ready
    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;
    /// SPANDOC: Try to add the block, and expect failure
    // TODO(teor || jlusby): check error kind
    ready_verifier_service
        .call(arc_block.clone())
        .await
        .unwrap_err();

    Ok(())
}

#[tokio::test]
async fn header_solution_test() -> Result<(), Report> {
    header_solution().await
}

#[spandoc::spandoc]
async fn header_solution() -> Result<(), Report> {
    zebra_test::init();

    // Service variables
    let state_service = Box::new(zebra_state::in_memory::init());
    let mut block_verifier = super::init(state_service.clone());

    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;

    // Get a valid block
    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])
        .expect("block test vector should deserialize");

    // This should be ok
    ready_verifier_service
        .call(Arc::new(block.clone()))
        .await
        .map_err(|e| eyre!(e))?;

    // Change nonce to something invalid
    block.header.nonce = [0; 32];

    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;

    // Error: invalid equihash solution for BlockHeader
    ready_verifier_service
        .call(Arc::new(block.clone()))
        .await
        .expect_err("expected the equihash solution to be invalid");

    Ok(())
}

#[tokio::test]
#[spandoc::spandoc]
async fn coinbase() -> Result<(), Report> {
    zebra_test::init();

    // Service variables
    let state_service = Box::new(zebra_state::in_memory::init());
    let mut block_verifier = super::init(state_service.clone());

    // Get a header of a block
    let header = BlockHeader::zcash_deserialize(&zebra_test::vectors::DUMMY_HEADER[..]).unwrap();

    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;

    // Test 1: Empty transaction
    let block = Block {
        header,
        transactions: Vec::new(),
    };

    // Error: no coinbase transaction in block
    ready_verifier_service
        .call(Arc::new(block.clone()))
        .await
        .expect_err("fail with no coinbase transaction in block");

    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;

    // Test 2: Transaction at first position is not coinbase
    let mut transactions = Vec::new();
    let tx = Transaction::zcash_deserialize(&zebra_test::vectors::DUMMY_TX1[..]).unwrap();
    transactions.push(Arc::new(tx));
    let block = Block {
        header,
        transactions,
    };

    // Error: no coinbase transaction in block
    ready_verifier_service
        .call(Arc::new(block))
        .await
        .expect_err("fail with no coinbase transaction in block");

    let ready_verifier_service = block_verifier.ready_and().await.map_err(|e| eyre!(e))?;

    // Test 3: Invalid coinbase position
    let mut block = Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..])?;
    assert_eq!(block.transactions.len(), 1);

    // Extract the coinbase transaction from the block
    let coinbase_transaction = block.transactions.get(0).unwrap().clone();

    // Add another coinbase transaction to block
    block.transactions.push(coinbase_transaction);
    assert_eq!(block.transactions.len(), 2);

    // Error: coinbase input found in additional transaction
    ready_verifier_service
        .call(Arc::new(block))
        .await
        .expect_err("fail with coinbase input found in additional transaction");

    Ok(())
}
