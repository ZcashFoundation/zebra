use color_eyre::eyre::{eyre, Result};

use zebra_chain::parameters::Network::*;
use zebra_test::prelude::*;

use crate::common::test_type::TestType::*;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn has_spending_transaction_ids() -> Result<()> {
    use std::sync::Arc;
    use tower::Service;
    use zebra_chain::{chain_tip::ChainTip, transparent::Input};
    use zebra_state::{ReadRequest, ReadResponse, SemanticallyVerifiedBlock, Spend};

    use crate::common::cached_state::future_blocks;

    let _init_guard = zebra_test::init();
    let test_type = UpdateZebraCachedStateWithRpc;
    let test_name = "has_spending_transaction_ids_test";
    let network = Mainnet;

    let Some(zebrad_state_path) = test_type.zebrad_state_path(test_name) else {
        // Skip test if there's no cached state.
        return Ok(());
    };

    tracing::info!("loading blocks for non-finalized state");

    let non_finalized_blocks = future_blocks(&network, test_type, test_name, 100).await?;

    let (mut state, mut read_state, latest_chain_tip, _chain_tip_change) =
        crate::common::cached_state::start_state_service_with_cache_dir(
            &Mainnet,
            zebrad_state_path,
        )
        .await?;

    tracing::info!("committing blocks to non-finalized state");

    for block in non_finalized_blocks {
        use zebra_state::{CommitSemanticallyVerifiedBlockRequest, MappedRequest};

        let expected_hash = block.hash();
        let block = SemanticallyVerifiedBlock::with_hash(Arc::new(block), expected_hash);
        let block_hash = CommitSemanticallyVerifiedBlockRequest(block)
            .mapped_oneshot(&mut state)
            .await
            .map_err(|err| eyre!(err))?;

        assert_eq!(
            expected_hash, block_hash,
            "state should respond with expected block hash"
        );
    }

    let mut tip_hash = latest_chain_tip
        .best_tip_hash()
        .expect("cached state must not be empty");

    tracing::info!("checking indexes of spending transaction ids");

    // Read the last 500 blocks - should be greater than the MAX_BLOCK_REORG_HEIGHT so that
    // both the finalized and non-finalized state are checked.
    let num_blocks_to_check = 500;
    let mut is_failure = false;
    for i in 0..num_blocks_to_check {
        let ReadResponse::Block(block) = read_state
            .ready()
            .await
            .map_err(|err| eyre!(err))?
            .call(ReadRequest::Block(tip_hash.into()))
            .await
            .map_err(|err| eyre!(err))?
        else {
            panic!("unexpected response to Block request");
        };

        let block = block.expect("should have block with latest_chain_tip hash");

        let spends_with_spending_tx_hashes = block.transactions.iter().cloned().flat_map(|tx| {
            let tx_hash = tx.hash();
            tx.inputs()
                .iter()
                .filter_map(Input::outpoint)
                .map(Spend::from)
                .chain(tx.sprout_nullifiers().cloned().map(Spend::from))
                .chain(tx.sapling_nullifiers().cloned().map(Spend::from))
                .chain(tx.orchard_nullifiers().cloned().map(Spend::from))
                .map(|spend| (spend, tx_hash))
                .collect::<Vec<_>>()
        });

        for (spend, expected_transaction_hash) in spends_with_spending_tx_hashes {
            let ReadResponse::TransactionId(transaction_hash) = read_state
                .ready()
                .await
                .map_err(|err| eyre!(err))?
                .call(ReadRequest::SpendingTransactionId(spend))
                .await
                .map_err(|err| eyre!(err))?
            else {
                panic!("unexpected response to Block request");
            };

            let Some(transaction_hash) = transaction_hash else {
                tracing::warn!(
                    ?spend,
                    depth = i,
                    height = ?block.coinbase_height(),
                    "querying spending tx id for spend failed"
                );
                is_failure = true;
                continue;
            };

            assert_eq!(
                transaction_hash, expected_transaction_hash,
                "spending transaction hash should match expected transaction hash"
            );
        }

        if i % 25 == 0 {
            tracing::info!(
                height = ?block.coinbase_height(),
                "has all spending tx ids at and above block"
            );
        }

        tip_hash = block.header.previous_block_hash;
    }

    assert!(
        !is_failure,
        "at least one spend was missing a spending transaction id"
    );

    Ok(())
}
