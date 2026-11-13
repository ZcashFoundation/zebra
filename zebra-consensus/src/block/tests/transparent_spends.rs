//! Early rejection of duplicate transparent spends within and across block transactions.

use std::{sync::Arc, time::Duration};

use tower::{service_fn, ServiceExt};
use zebra_chain::{
    block::{Block, Height},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserialize,
    transaction::{Hash, LockTime, Transaction},
    transparent::{Input, OutPoint, Output, Script},
};

use crate::{
    block::{
        check::{coinbase_is_first, no_duplicate_transparent_spends},
        Request, SemanticBlockVerifier, VerifyBlockError,
    },
    error::{BlockError, TransactionError},
    BoxError,
};

const HEIGHT: Height = Height(10);
const OUTPOINT: OutPoint = OutPoint {
    hash: Hash([1; 32]),
    index: 0,
};

fn transaction(inputs: &[OutPoint], version: u32, tag: u32, network: &Network) -> Transaction {
    let inputs = inputs
        .iter()
        .map(|outpoint| Input::PrevOut {
            outpoint: *outpoint,
            unlock_script: Script::new(&[]),
            sequence: tag,
        })
        .collect();
    let outputs = vec![Output {
        value: 1.try_into().unwrap(),
        lock_script: Script::new(&[0x51]),
    }];
    if version == 4 {
        Transaction::test_v4(inputs, outputs, LockTime::unlocked(), Height(11))
    } else {
        Transaction::test_v5(
            NetworkUpgrade::current(network, HEIGHT),
            inputs,
            outputs,
            LockTime::unlocked(),
            Height(11),
        )
    }
}

fn block(network: &Network, transactions: Vec<Transaction>) -> Block {
    let mut block =
        Block::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..]).unwrap();
    let coinbase = Transaction::test_v5(
        NetworkUpgrade::current(network, HEIGHT),
        vec![Input::Coinbase {
            height: HEIGHT,
            data: vec![0],
            sequence: u32::MAX,
        }],
        vec![Output {
            value: zebra_chain::parameters::subsidy::block_subsidy(HEIGHT, network).unwrap(),
            lock_script: Script::new(&[0x51]),
        }],
        LockTime::unlocked(),
        HEIGHT,
    );
    block.transactions = std::iter::once(coinbase)
        .chain(transactions)
        .map(Arc::new)
        .collect();
    Arc::make_mut(&mut block.header).merkle_root =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    block
}

async fn verify(network: &Network, request: Request) -> VerifyBlockError {
    let state = service_fn(|request| async move {
        assert!(matches!(request, zebra_state::Request::KnownBlock(_)));
        Ok::<_, BoxError>(zebra_state::Response::KnownBlock(None))
    });
    let transactions = service_fn(|_| async {
        unreachable!("transparent spend conflicts must be rejected before transaction work")
    });
    tokio::time::timeout(
        Duration::from_secs(10),
        SemanticBlockVerifier::new(network, state, transactions).oneshot(request),
    )
    .await
    .expect("preflight checks must not wait for state or transaction verification")
    .expect_err("the block is invalid")
}

#[tokio::test]
async fn transparent_spends_rejected_before_transaction_work() {
    let _init_guard = zebra_test::init();
    let network = super::nu7_network(1_000);
    for version in [4, 5] {
        for transactions in [
            vec![
                transaction(&[OUTPOINT], version, 0, &network),
                transaction(&[OUTPOINT], version, 1, &network),
            ],
            vec![transaction(&[OUTPOINT, OUTPOINT], version, 0, &network)],
        ] {
            let block = Arc::new(block(&network, transactions));
            for request in [
                Request::Commit(block.clone()),
                Request::CheckProposal(block.clone()),
            ] {
                let error = verify(&network, request).await;
                assert!(matches!(
                    error,
                    VerifyBlockError::Block {
                        source: BlockError::Transaction(
                            TransactionError::DuplicateTransparentSpend(OUTPOINT)
                        ),
                    }
                ));
                // Keep the contextual duplicate-spend rejection's existing peer penalty.
                assert_eq!(error.misbehavior_score(), 0);
            }
        }
    }
}

#[tokio::test]
async fn merkle_binding_precedes_transparent_spend_rejection() {
    let _init_guard = zebra_test::init();
    let network = super::nu7_network(1_000);
    let mut block = block(
        &network,
        vec![
            transaction(&[OUTPOINT], 5, 0, &network),
            transaction(&[OUTPOINT], 5, 1, &network),
        ],
    );
    Arc::make_mut(&mut block.header).merkle_root = zebra_chain::block::merkle::Root([0; 32]);
    assert!(matches!(
        verify(&network, Request::CheckProposal(Arc::new(block))).await,
        VerifyBlockError::Block {
            source: BlockError::BadMerkleRoot { .. },
        }
    ));
}

#[test]
fn transparent_spend_preflight_scope() {
    let _init_guard = zebra_test::init();
    let network = super::nu7_network(1_000);
    for spends in [
        vec![],
        vec![vec![]],
        vec![
            vec![OUTPOINT],
            vec![OutPoint {
                index: 1,
                ..OUTPOINT
            }],
            vec![OutPoint {
                hash: Hash([2; 32]),
                ..OUTPOINT
            }],
        ],
    ] {
        let transactions = spends
            .iter()
            .map(|inputs| transaction(inputs, 5, 0, &network))
            .collect();
        let block = block(&network, transactions);
        coinbase_is_first(&block).unwrap();
        no_duplicate_transparent_spends(&block).unwrap();
    }
}

#[tokio::test]
async fn coinbase_structure_precedes_transparent_spend_rejection() {
    let _init_guard = zebra_test::init();
    let network = super::nu7_network(1_000);
    let mut block = block(
        &network,
        vec![
            transaction(&[OUTPOINT], 5, 0, &network),
            transaction(&[OUTPOINT], 5, 1, &network),
        ],
    );
    // Keep the later coinbase distinct for the preceding Merkle duplicate-transaction check.
    let mut outputs = block.transactions[0].outputs();
    outputs[0].lock_script = Script::new(&[0x52]);
    let later_coinbase = block.transactions[0]
        .as_ref()
        .clone()
        .with_transparent_outputs(outputs);
    block.transactions.push(Arc::new(later_coinbase));
    Arc::make_mut(&mut block.header).merkle_root =
        block.transactions.iter().map(|tx| tx.hash()).collect();
    assert!(matches!(
        verify(&network, Request::CheckProposal(Arc::new(block))).await,
        VerifyBlockError::Block {
            source: BlockError::Transaction(TransactionError::CoinbaseAfterFirst),
        }
    ));
}
