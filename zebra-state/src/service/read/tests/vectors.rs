//! Fixed test vectors for the ReadStateService.

use std::sync::Arc;

use zebra_chain::{
    block::Block, parameters::Network::*, serialization::ZcashDeserializeInto, transaction,
};

use zebra_test::{
    prelude::Result,
    transcript::{ExpectedTranscriptError, Transcript},
};

use crate::{init_test_services, populated_state, ReadRequest, ReadResponse};

/// Test that ReadStateService responds correctly when empty.
#[tokio::test]
async fn empty_read_state_still_responds_to_requests() -> Result<()> {
    let _init_guard = zebra_test::init();

    let transcript = Transcript::from(empty_state_test_cases());

    let network = Mainnet;
    let (_state, read_state, _latest_chain_tip, _chain_tip_change) = init_test_services(network);

    transcript.check(read_state).await?;

    Ok(())
}

/// Test that ReadStateService responds correctly when the state contains blocks.
#[tokio::test(flavor = "multi_thread")]
async fn populated_read_state_responds_correctly() -> Result<()> {
    let _init_guard = zebra_test::init();

    // Create a continuous chain of mainnet blocks from genesis
    let blocks: Vec<Arc<Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
        .iter()
        .map(|(_height, block_bytes)| block_bytes.zcash_deserialize_into().unwrap())
        .collect();

    let (_state, read_state, _latest_chain_tip, _chain_tip_change) =
        populated_state(blocks.clone(), Mainnet).await;

    let empty_cases = Transcript::from(empty_state_test_cases());
    empty_cases.check(read_state.clone()).await?;

    for block in blocks {
        let block_cases = vec![
            (
                ReadRequest::Block(block.hash().into()),
                Ok(ReadResponse::Block(Some(block.clone()))),
            ),
            (
                ReadRequest::Block(block.coinbase_height().unwrap().into()),
                Ok(ReadResponse::Block(Some(block.clone()))),
            ),
        ];

        let block_cases = Transcript::from(block_cases);
        block_cases.check(read_state.clone()).await?;

        // Spec: transactions in the genesis block are ignored.
        if block.coinbase_height().unwrap().0 == 0 {
            continue;
        }

        for transaction in &block.transactions {
            let transaction_cases = vec![(
                ReadRequest::Transaction(transaction.hash()),
                Ok(ReadResponse::Transaction(Some((
                    transaction.clone(),
                    block.coinbase_height().unwrap(),
                )))),
            )];

            let transaction_cases = Transcript::from(transaction_cases);
            transaction_cases.check(read_state.clone()).await?;
        }
    }

    Ok(())
}

/// Returns test cases for the empty state and missing blocks.
fn empty_state_test_cases() -> Vec<(ReadRequest, Result<ReadResponse, ExpectedTranscriptError>)> {
    let block: Arc<Block> = zebra_test::vectors::BLOCK_MAINNET_419200_BYTES
        .zcash_deserialize_into()
        .unwrap();

    vec![
        (
            ReadRequest::Transaction(transaction::Hash([0; 32])),
            Ok(ReadResponse::Transaction(None)),
        ),
        (
            ReadRequest::Block(block.hash().into()),
            Ok(ReadResponse::Block(None)),
        ),
        (
            ReadRequest::Block(block.coinbase_height().unwrap().into()),
            Ok(ReadResponse::Block(None)),
        ),
    ]
}
