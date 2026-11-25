//! Parent outputs reach transaction verification without becoming new block outputs.

use super::*;

#[tokio::test]
async fn parent_prefetch_reaches_block_transactions_and_contextual_validation() {
    let _init_guard = zebra_test::init();
    let mut block: block::Block = zebra_test::vectors::BLOCK_MAINNET_1_BYTES
        .zcash_deserialize_into()
        .unwrap();
    let outpoint = transparent::OutPoint {
        hash: zebra_chain::transaction::Hash([7; 32]),
        index: 0,
    };
    let utxo = transparent::Utxo::new(
        transparent::Output::new(
            Amount::try_from(1).unwrap(),
            transparent::Script::new(&[0x51]),
        ),
        Height(0),
        false,
    );
    block.transactions.push(Arc::new(Transaction::test_v4(
        vec![transparent::Input::PrevOut {
            outpoint,
            unlock_script: transparent::Script::new(&[]),
            sequence: u32::MAX,
        }],
        vec![utxo.output.clone()],
        LockTime::unlocked(),
        Height(1),
    )));
    let hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash()).collect();
    Arc::make_mut(&mut block.header).merkle_root = hashes.iter().copied().collect();
    let expected_outputs = transparent::new_ordered_outputs(&block, &hashes);
    let expected = utxo.clone();
    let transactions = tower::service_fn(move |request: tx::BlockRequest| {
        assert_eq!(request.parent_utxos[&outpoint], expected);
        std::future::ready(Ok::<_, BoxError>(tx::BlockResponse {
            tx_id: request.transaction.unmined_id(),
            miner_fee: None,
            sigops: 0,
        }))
    });
    let state = tower::service_fn(move |request| {
        let result = match request {
            zs::Request::KnownBlock(_) => Ok(zs::Response::KnownBlock(None)),
            zs::Request::AnyChainUtxos { outpoints } => {
                assert_eq!(outpoints, vec![outpoint]);
                Ok(zs::Response::AnyChainUtxos(Arc::new(HashMap::from([(
                    outpoint,
                    utxo.clone(),
                )]))))
            }
            zs::Request::CheckBlockProposalValidity(prepared) => {
                assert_eq!(prepared.new_outputs.len(), expected_outputs.len());
                for (outpoint, output) in &expected_outputs {
                    assert_eq!(prepared.new_outputs[outpoint].as_ref(), output);
                }
                Err::<_, BoxError>("contextual validation reached".into())
            }
            _ => panic!("unexpected request: {request:?}"),
        };
        std::future::ready(result)
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        SemanticBlockVerifier::new(&Network::Mainnet, state, transactions)
            .oneshot(Request::CheckProposal(Arc::new(block))),
    )
    .await
    .expect("prefetch must not stall");
    let Err(VerifyBlockError::ValidateProposal(error)) = result else {
        panic!("expected contextual validation, got {result:?}");
    };
    assert_eq!(error.to_string(), "contextual validation reached");
}
