use std::sync::Arc;

use tower::util::BoxService;
use zebra_chain::{
    block::Block, parameters::Network, serialization::ZcashDeserializeInto, transaction,
    transparent,
};
use zebra_test::{prelude::*, transcript::Transcript};

use crate::{init, BoxError, Config, Request, Response};

#[spandoc::spandoc]
async fn populated_state() -> BoxService<Request, Response, BoxError> {
    let iter = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=10)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .map(|block| {
            let hash = block.hash();
            (
                Request::CommitFinalizedBlock { block },
                Ok(Response::Committed(hash)),
            )
        });
    let transcript = Transcript::from(iter);

    let config = Config::ephemeral();
    let network = Network::Mainnet;
    let mut state = init(config, network);

    /// SPANDOC: populating state for test
    transcript.check(&mut state).await.unwrap();

    state
}

#[tokio::test]
async fn empty_state_still_responds_to_requests() -> Result<()> {
    zebra_test::init();

    let block =
        zebra_test::vectors::BLOCK_MAINNET_419200_BYTES.zcash_deserialize_into::<Arc<Block>>()?;

    let iter = vec![
        // No checks for CommitBlock or CommitFinalizedBlock because empty state
        // precondition doesn't matter to them
        (Request::Depth(block.hash()), Ok(Response::Depth(None))),
        (Request::Tip, Ok(Response::Tip(None))),
        (Request::BlockLocator, Ok(Response::BlockLocator(vec![]))),
        (
            Request::Transaction(transaction::Hash([0; 32])),
            Ok(Response::Transaction(None)),
        ),
        (
            Request::Block(block.hash().into()),
            Ok(Response::Block(None)),
        ),
        (
            Request::Block(block.coinbase_height().unwrap().into()),
            Ok(Response::Block(None)),
        ),
        // No check for AwaitUTXO because it will wait if the UTXO isn't present
    ]
    .into_iter();
    let transcript = Transcript::from(iter);

    let config = Config::ephemeral();
    let network = Network::Mainnet;
    let state = init(config, network);

    transcript.check(state).await?;

    Ok(())
}

#[tokio::test]
async fn populated_state_responds_correctly() -> Result<()> {
    zebra_test::init();

    let mut state = populated_state().await;

    let last_block_height = 10;

    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=last_block_height)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap());

    for (ind, block) in blocks.enumerate() {
        let mut transcript = vec![];
        let height = block.coinbase_height().unwrap();
        let hash = block.hash();

        transcript.push((
            Request::Block(hash.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        transcript.push((
            Request::Block(height.into()),
            Ok(Response::Block(Some(block.clone()))),
        ));

        transcript.push((
            Request::Depth(block.hash()),
            Ok(Response::Depth(Some(last_block_height - height.0))),
        ));

        if ind == last_block_height as usize {
            transcript.push((Request::Tip, Ok(Response::Tip(Some((height, hash))))));
        }

        // Consensus-critical bug in zcashd: transactions in the genesis block
        // are ignored.
        if height.0 != 0 {
            for transaction in &block.transactions {
                let transaction_hash = transaction.hash();

                transcript.push((
                    Request::Transaction(transaction_hash),
                    Ok(Response::Transaction(Some(transaction.clone()))),
                ));

                for (index, output) in transaction.outputs().iter().enumerate() {
                    let outpoint = transparent::OutPoint {
                        hash: transaction_hash,
                        index: index as _,
                    };

                    transcript.push((
                        Request::AwaitUtxo(outpoint),
                        Ok(Response::Utxo(output.clone())),
                    ));
                }
            }
        }

        let transcript = Transcript::from(transcript);
        transcript.check(&mut state).await?;
    }

    Ok(())
}
