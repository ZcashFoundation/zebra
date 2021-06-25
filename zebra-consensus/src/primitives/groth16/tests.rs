//! Tests for transaction verification

use futures::stream::{FuturesUnordered, StreamExt};
use tower::ServiceExt;

use zebra_chain::{block::Block, serialization::ZcashDeserializeInto, transaction::Transaction};

use crate::primitives::groth16::{self, *};

async fn verify_groth16_spends_and_outputs<V>(
    spend_verifier: &mut V,
    output_verifier: &mut V,
    transactions: Vec<std::sync::Arc<Transaction>>,
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<bellman::groth16::batch::Item<bls12_381::Bls12>>>::Error:
        std::fmt::Debug
            + std::convert::From<
                std::boxed::Box<dyn std::error::Error + std::marker::Send + std::marker::Sync>,
            >,
{
    zebra_test::init();

    let mut async_checks = FuturesUnordered::new();

    for tx in transactions {
        let spends = tx.sapling_spends_per_anchor();
        let outputs = tx.sapling_outputs();

        for spend in spends {
            tracing::trace!(?spend);

            let spend_rsp = spend_verifier
                .ready_and()
                .await?
                .call(groth16::ItemWrapper::from(&spend).into());

            async_checks.push(spend_rsp);
        }

        for output in outputs {
            tracing::trace!(?output);

            let output_rsp = output_verifier
                .ready_and()
                .await?
                .call(groth16::ItemWrapper::from(output).into());

            async_checks.push(output_rsp);
        }

        while let Some(result) = async_checks.next().await {
            tracing::trace!(?result);
            result?;
        }
    }

    Ok(())
}

#[tokio::test]
async fn verify_sapling_groth16() {
    // Since we expect these to pass, we can use the communal verifiers.
    let mut spend_verifier = groth16::SPEND_VERIFIER.clone();
    let mut output_verifier = groth16::OUTPUT_VERIFIER.clone();

    let transactions = zebra_test::vectors::MAINNET_BLOCKS
        .clone()
        .iter()
        .flat_map(|(_, bytes)| {
            let block = bytes
                .zcash_deserialize_into::<Block>()
                .expect("a valid block");
            block.transactions
        })
        .collect();

    // This should fail if any of the proofs fail to validate.
    verify_groth16_spends_and_outputs(&mut spend_verifier, &mut output_verifier, transactions)
        .await
        .unwrap()
}

async fn verify_invalid_groth16_output_description<V>(
    output_verifier: &mut V,
    transactions: Vec<std::sync::Arc<Transaction>>,
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<bellman::groth16::batch::Item<bls12_381::Bls12>>>::Error:
        std::convert::From<
            std::boxed::Box<dyn std::error::Error + std::marker::Send + std::marker::Sync>,
        >,
{
    zebra_test::init();

    let mut async_checks = FuturesUnordered::new();

    for tx in transactions {
        let outputs = tx.sapling_outputs();

        for output in outputs {
            // This changes the primary inputs to the proof
            // verification, causing it to fail for this proof.
            let mut modified_output = output.clone();
            modified_output.cm_u = jubjub::Fq::zero();

            tracing::trace!(?modified_output);

            let output_rsp = output_verifier
                .ready_and()
                .await?
                .call(groth16::ItemWrapper::from(&modified_output).into());

            async_checks.push(output_rsp);
        }

        while let Some(result) = async_checks.next().await {
            result?;
        }
    }

    Ok(())
}

#[tokio::test]
#[should_panic]
async fn correctly_err_on_invalid_output_proof() {
    // Since we expect these to fail, we don't want to poison the communal
    // verifiers.
    let mut output_verifier = Fallback::new(
        Batch::new(
            Verifier::new(&PARAMS.sapling.output.vk),
            crate::primitives::MAX_BATCH_SIZE,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(item.verify_single(&prepare_verifying_key(&PARAMS.sapling.output.vk)))
            }) as fn(_) -> _,
        ),
    );

    let block = zebra_test::vectors::BLOCK_MAINNET_903001_BYTES
        .clone()
        .zcash_deserialize_into::<Block>()
        .expect("a valid block");

    verify_invalid_groth16_output_description(&mut output_verifier, block.transactions)
        .await
        .unwrap()
}
