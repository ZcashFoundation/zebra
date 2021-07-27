use std::{convert::TryFrom, env, sync::Arc};

use futures::stream::FuturesUnordered;
use tower::{util::BoxService, Service, ServiceExt};

use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction,
    transparent::{self, CoinbaseSpendRestriction::OnlyShieldedOutputs},
};
use zebra_test::{prelude::*, transcript::Transcript};

use crate::{
    constants::MIN_TRANSPARENT_COINBASE_MATURITY, init, service::arbitrary, BoxError, Config,
    Request, Response,
};

const LAST_BLOCK_HEIGHT: u32 = 10;

async fn populated_state(
    blocks: impl IntoIterator<Item = Arc<Block>>,
) -> BoxService<Request, Response, BoxError> {
    let requests = blocks
        .into_iter()
        .map(|block| Request::CommitFinalizedBlock(block.into()));

    let config = Config::ephemeral();
    let network = Network::Mainnet;
    let mut state = init(config, network);

    let mut responses = FuturesUnordered::new();

    for request in requests {
        let rsp = state.ready_and().await.unwrap().call(request);
        responses.push(rsp);
    }

    use futures::StreamExt;
    while let Some(rsp) = responses.next().await {
        rsp.expect("blocks should commit just fine");
    }

    state
}

async fn test_populated_state_responds_correctly(
    mut state: BoxService<Request, Response, BoxError>,
) -> Result<()> {
    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap());

    for (ind, block) in blocks.into_iter().enumerate() {
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
            Ok(Response::Depth(Some(LAST_BLOCK_HEIGHT - height.0))),
        ));

        if ind == LAST_BLOCK_HEIGHT as usize {
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

                let from_coinbase = transaction.is_coinbase();
                for (index, output) in transaction.outputs().iter().cloned().enumerate() {
                    let outpoint = transparent::OutPoint {
                        hash: transaction_hash,
                        index: index as _,
                    };

                    let utxo = transparent::Utxo {
                        output,
                        height,
                        from_coinbase,
                    };

                    // Use the minimum possible spend height
                    let spend_height = block::Height(
                        u32::try_from(ind).unwrap() + MIN_TRANSPARENT_COINBASE_MATURITY,
                    );

                    transcript.push((
                        Request::AwaitSpendableUtxo {
                            outpoint,
                            spend_restriction: OnlyShieldedOutputs { spend_height },
                        },
                        Ok(Response::SpendableUtxo(utxo)),
                    ));
                }
            }
        }

        let transcript = Transcript::from(transcript);
        transcript.check(&mut state).await?;
    }

    Ok(())
}

#[tokio::main]
async fn populate_and_check(blocks: Vec<Arc<Block>>) -> Result<()> {
    let state = populated_state(blocks).await;
    test_populated_state_responds_correctly(state).await?;
    Ok(())
}

fn out_of_order_committing_strategy() -> BoxedStrategy<Vec<Arc<Block>>> {
    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect::<Vec<_>>();

    Just(blocks).prop_shuffle().boxed()
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

#[test]
fn state_behaves_when_blocks_are_committed_in_order() -> Result<()> {
    zebra_test::init();

    let blocks = zebra_test::vectors::MAINNET_BLOCKS
        .range(0..=LAST_BLOCK_HEIGHT)
        .map(|(_, block_bytes)| block_bytes.zcash_deserialize_into::<Arc<Block>>().unwrap())
        .collect();

    populate_and_check(blocks)?;

    Ok(())
}

const DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES: u32 = 2;
const BLOCKS_AFTER_NU5: u32 = 101;

proptest! {
    #![proptest_config(
        proptest::test_runner::Config::with_cases(env::var("PROPTEST_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_PARTIAL_CHAIN_PROPTEST_CASES))
    )]

    /// Test out of order commits of continuous block test vectors from genesis onward.
    #[test]
    fn state_behaves_when_blocks_are_committed_out_of_order(blocks in out_of_order_committing_strategy()) {
        zebra_test::init();

        populate_and_check(blocks).unwrap();
    }

    /// Test blocks that are less than the NU5 activation height.
    #[test]
    fn some_block_less_than_network_upgrade(
        (network, nu_activation_height, chain) in arbitrary::partial_nu5_chain_strategy(4, true, BLOCKS_AFTER_NU5/2, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter().rev(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }

    /// Test the maximum amount of blocks to check before chain is declared to be legacy.
    #[test]
    fn no_transaction_with_network_upgrade(
        (network, nu_activation_height, chain) in arbitrary::partial_nu5_chain_strategy(4, true, BLOCKS_AFTER_NU5, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter().rev(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("giving up after checking too many blocks".into())
        );
    }

    /// Test the `Block.check_transaction_network_upgrade()` error inside the legacy check.
    #[test]
    fn at_least_one_transaction_with_inconsistent_network_upgrade(
        (network, nu_activation_height, chain) in arbitrary::partial_nu5_chain_strategy(5, false, BLOCKS_AFTER_NU5, NetworkUpgrade::Canopy)
    ) {
        // this test requires that an invalid block is encountered
        // before a valid block (and before the check gives up),
        // but setting `transaction_has_valid_network_upgrade` to false
        // sometimes generates blocks with all valid (or missing) network upgrades

        // we must check at least one block, and the first checked block must be invalid
        let first_checked_block = chain
            .iter()
            .rev()
            .take_while(|block| block.coinbase_height().unwrap() >= nu_activation_height)
            .take(100)
            .next();
        prop_assume!(first_checked_block.is_some());
        prop_assume!(
            first_checked_block
                .unwrap()
                .check_transaction_network_upgrade_consistency(network)
                .is_err()
        );

        let response = crate::service::legacy_chain_check(
            nu_activation_height,
            chain.clone().into_iter().rev(),
            network
        ).map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("inconsistent network upgrade found in transaction".into()),
            "first: {:?}, last: {:?}",
            chain.first().map(|block| block.coinbase_height()),
            chain.last().map(|block| block.coinbase_height()),
        );
    }

    /// Test there is at least one transaction with a valid `network_upgrade` in the legacy check.
    #[test]
    fn at_least_one_transaction_with_valid_network_upgrade(
        (network, nu_activation_height, chain) in arbitrary::partial_nu5_chain_strategy(5, true, BLOCKS_AFTER_NU5/2, NetworkUpgrade::Canopy)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter().rev(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }
}
