use std::{env, sync::Arc};

use futures::stream::FuturesUnordered;
use tower::{util::BoxService, Service, ServiceExt};
use zebra_chain::{
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction, transparent,
};
use zebra_test::{prelude::*, transcript::Transcript};

use crate::{init, BoxError, Config, Request, Response, Utxo};

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
                    let utxo = Utxo {
                        output,
                        height,
                        from_coinbase,
                    };

                    transcript.push((Request::AwaitUtxo(outpoint), Ok(Response::Utxo(utxo))));
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

#[test]
fn state_behaves_when_blocks_are_committed_out_of_order() -> Result<()> {
    zebra_test::init();

    proptest!(|(blocks in out_of_order_committing_strategy())| {
        populate_and_check(blocks).unwrap();
    });

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

    #[test]
    fn at_least_one_transaction_with_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, true, BLOCKS_AFTER_NU5)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(response, Ok(()));
    }

    #[test]
    fn no_transactions_with_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(4, true, BLOCKS_AFTER_NU5)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("giving up after checking too many blocks".into())
        );
    }

    #[test]
    fn at_least_one_transactions_with_inconsistent_network_upgrade(
        (network, nu_activation_height, chain) in partial_nu5_chain_strategy(5, false, BLOCKS_AFTER_NU5)
    ) {
        let response = crate::service::legacy_chain_check(nu_activation_height, chain.into_iter(), network)
            .map_err(|error| error.to_string());

        prop_assert_eq!(
            response,
            Err("inconsistent network upgrade found in transaction".into())
        );
    }
}

// Utility functions

fn partial_nu5_chain_strategy(
    transaction_version_override: u32,
    transaction_has_valid_network_upgrade: bool,
    blocks_after_nu_activation: u32,
) -> impl Strategy<
    Value = (
        Network,
        Height,
        zebra_chain::fmt::SummaryDebug<Vec<Arc<Block>>>,
    ),
> {
    (any::<Network>(), NetworkUpgrade::branch_id_strategy()).prop_flat_map(
        move |(network, random_nu)| {
            // We are going to test the legacy chain with Canopy instead as we don't have a
            // Nu5 activation height yet.
            // TODO: update this to Nu5 after we have a height #1841
            let mut nu = NetworkUpgrade::Canopy;
            let nu_activation = NetworkUpgrade::Canopy.activation_height(network).unwrap();
            let height = Height(nu_activation.0 + blocks_after_nu_activation);

            // The `network_upgrade_override` will not be enough as when it is `None`,
            // current network upgrade will be used (`NetworkUpgrade::Canopy`) which will be valid.
            if !transaction_has_valid_network_upgrade {
                nu = random_nu;
            }

            zebra_chain::block::LedgerState::height_strategy(
                height,
                Some(nu),
                Some(transaction_version_override),
                transaction_has_valid_network_upgrade,
            )
            .prop_flat_map(move |init| {
                Block::partial_chain_strategy(init, blocks_after_nu_activation as usize)
            })
            .prop_map(move |partial_chain| (network, nu_activation, partial_chain))
        },
    )
}
