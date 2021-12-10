//! Tests for transaction verification

use std::convert::TryInto;

use futures::stream::{FuturesUnordered, StreamExt};
use hex::FromHex;
use tower::ServiceExt;

use zebra_chain::{
    block::Block, serialization::ZcashDeserializeInto, sprout::EncryptedNote,
    transaction::Transaction,
};

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
                .ready()
                .await?
                .call(groth16::ItemWrapper::from(&spend).into());

            async_checks.push(spend_rsp);
        }

        for output in outputs {
            tracing::trace!(?output);

            let output_rsp = output_verifier
                .ready()
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
    // Use separate verifiers so shared batch tasks aren't killed when the test ends (#2390)
    let mut spend_verifier = Fallback::new(
        Batch::new(
            Verifier::new(&GROTH16_PARAMETERS.sapling.spend.vk),
            crate::primitives::MAX_BATCH_SIZE,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(item.verify_single(&GROTH16_PARAMETERS.sapling.spend_prepared_verifying_key))
            }) as fn(_) -> _,
        ),
    );
    let mut output_verifier = Fallback::new(
        Batch::new(
            Verifier::new(&GROTH16_PARAMETERS.sapling.output.vk),
            crate::primitives::MAX_BATCH_SIZE,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(item.verify_single(&GROTH16_PARAMETERS.sapling.output_prepared_verifying_key))
            }) as fn(_) -> _,
        ),
    );

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
                .ready()
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
async fn correctly_err_on_invalid_output_proof() {
    // Use separate verifiers so shared batch tasks aren't killed when the test ends (#2390).
    // Also, since we expect these to fail, we don't want to slow down the communal verifiers.
    let mut output_verifier = Fallback::new(
        Batch::new(
            Verifier::new(&GROTH16_PARAMETERS.sapling.output.vk),
            crate::primitives::MAX_BATCH_SIZE,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(item.verify_single(&GROTH16_PARAMETERS.sapling.output_prepared_verifying_key))
            }) as fn(_) -> _,
        ),
    );

    let block = zebra_test::vectors::BLOCK_MAINNET_903001_BYTES
        .clone()
        .zcash_deserialize_into::<Block>()
        .expect("a valid block");

    verify_invalid_groth16_output_description(&mut output_verifier, block.transactions)
        .await
        .expect_err("unexpected success checking invalid groth16 inputs");
}

async fn verify_groth16_joinsplits<V>(
    verifier: &mut V,
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
        let joinsplits = tx.sprout_groth16_joinsplits();

        for joinsplit in joinsplits {
            tracing::trace!(?joinsplit);

            let pub_key = tx
                .sprout_joinsplit_pub_key()
                .expect("pub key must exist since there are joinsplits");
            let joinsplit_rsp = verifier
                .ready()
                .await?
                .call(groth16::ItemWrapper::from(&(joinsplit, &pub_key)).into());

            async_checks.push(joinsplit_rsp);
        }

        while let Some(result) = async_checks.next().await {
            tracing::trace!(?result);
            result?;
        }
    }

    Ok(())
}

#[tokio::test]
async fn verify_sprout_groth16() {
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(&GROTH16_PARAMETERS.sprout.joinsplit_prepared_verifying_key)
                    .map_err(tower_fallback::BoxedError::from),
            )
        }) as fn(_) -> _,
    );

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
    verify_groth16_joinsplits(&mut verifier, transactions)
        .await
        .expect("verification should pass");
}

async fn verify_groth16_joinsplit_vector<V>(
    verifier: &mut V,
    joinsplit: &JoinSplit<Groth16Proof>,
    pub_key: &ed25519::VerificationKeyBytes,
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

    tracing::trace!(?joinsplit);

    let joinsplit_rsp = verifier
        .ready()
        .await?
        .call(groth16::ItemWrapper::from(&(joinsplit, pub_key)).into());

    async_checks.push(joinsplit_rsp);

    while let Some(result) = async_checks.next().await {
        tracing::trace!(?result);
        result?;
    }

    Ok(())
}

#[tokio::test]
async fn verify_sprout_groth16_vector() {
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(&GROTH16_PARAMETERS.sprout.joinsplit_prepared_verifying_key)
                    .map_err(tower_fallback::BoxedError::from),
            )
        }) as fn(_) -> _,
    );

    // Test vector extracted manually by printing a JoinSplit generated by
    // the test_joinsplit test of the zcashd repository.
    // https://github.com/zcash/zcash/blob/7aaefab2d7d671951f153e47cdd83ae55d78144f/src/gtest/test_joinsplit.cpp#L48
    let joinsplit = JoinSplit::<Groth16Proof> {
        vpub_old: 0x0Ai32.try_into().unwrap(),
        vpub_new: 0.try_into().unwrap(),
        anchor: <[u8; 32]>::from_hex(
            "D7C612C817793191A1E68652121876D6B3BDE40F4FA52BC314145CE6E5CDD259",
        )
        .unwrap()
        .into(),
        nullifiers: [
            <[u8; 32]>::from_hex(
                "F9AD4EED10C97FF8FDE3C63512242D3937C0E2836389A95B972C50FB942F775B",
            )
            .unwrap()
            .into(),
            <[u8; 32]>::from_hex(
                "DF6EB39839A549F0DF24CDEBBB23CA7107E84D2E6BD0294A8B1BFBD0FAE7800C",
            )
            .unwrap()
            .into(),
        ],
        commitments: [
            <[u8; 32]>::from_hex(
                "0D595308A445D07EB62C7C13CB9F2630DFD39E6A060E98A9788C92BDDBAEA538",
            )
            .unwrap()
            .into(),
            <[u8; 32]>::from_hex(
                "EE7D9622C410878A218ED8A8A6A10B11DDBDA83CB2A627508354BFA490E0F33E",
            )
            .unwrap()
            .into(),
        ],
        // The ephemeral key is not validated in the proof, use a dummy value.
        ephemeral_key: [0u8; 32].into(),
        random_seed: <[u8; 32]>::from_hex(
            "6A14E910A94EF500043A42417D8D2B4124AB35DC1E14DDF830EBCF972E850807",
        )
        .unwrap().into(),
        vmacs: [
            <[u8; 32]>::from_hex(
                "630D39F963960E9092E518CEF4C84853C13EF9FC759CBECDD2ED61D1070C82E6",
            )
            .unwrap()
            .into(),
            <[u8; 32]>::from_hex(
                "1C8DCEC25F816D0177AC29958D0B8594EC669AED4A32D9FBEEC3C57B4503F19A",
            )
            .unwrap()
            .into(),
        ],
        zkproof: <[u8; 192]>::from_hex(
            "802BD3D746BA4831E10027C92E0E610618F619E3CE7EE087622BFF86F19B5BC3292DACFD27506C8BFF4C808035EB9C7685010235D47F1D77C5DCC212323E69726F04A46E0BBDCE17C64EEEA36F443E25F21DF2C39FE8A996BAE899AB8F8CCF52054DC6A5553D0F86283E056AED8E6EABE11D85EDF7948005AD9B982759F20E5DE54A59A1B80CD31AD4CC96419492886C91C4D7C521C327B47F4F5688067BE2B19EB8BC0B7BD357BF931CCF8BCC62A7E48A81CD287F00854767B41748F05EDD5B",
        )
        .unwrap()
        .into(),
        // The ciphertexts are not validated in the proof, use a dummy value.
        enc_ciphertexts: [EncryptedNote([0u8; 601]),EncryptedNote([0u8; 601])],
    };

    let pub_key =
        <[u8; 32]>::from_hex("63A144ABC0524C9EADE1DB9DE17AEC4A39626A0FDB597B9EC6DDA327EE9FE845")
            .unwrap()
            .into();

    verify_groth16_joinsplit_vector(&mut verifier, &joinsplit, &pub_key)
        .await
        .expect("verification should pass");
}

async fn verify_invalid_groth16_joinsplit_description<V>(
    verifier: &mut V,
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
        let joinsplits = tx.sprout_groth16_joinsplits();

        for joinsplit in joinsplits {
            // Use an arbitrary public key which is not the correct one,
            // which will make the verification fail.
            let modified_pub_key = [0x42; 32].into();
            let joinsplit_rsp = verifier
                .ready()
                .await?
                .call(groth16::ItemWrapper::from(&(joinsplit, &modified_pub_key)).into());

            async_checks.push(joinsplit_rsp);
        }

        while let Some(result) = async_checks.next().await {
            result?;
        }
    }

    Ok(())
}

#[tokio::test]
async fn correctly_err_on_invalid_joinsplit_proof() {
    // Use separate verifiers so shared batch tasks aren't killed when the test ends (#2390).
    // Also, since we expect these to fail, we don't want to slow down the communal verifiers.
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(&GROTH16_PARAMETERS.sprout.joinsplit_prepared_verifying_key)
                    .map_err(tower_fallback::BoxedError::from),
            )
        }) as fn(_) -> _,
    );

    let block = zebra_test::vectors::BLOCK_MAINNET_419201_BYTES
        .clone()
        .zcash_deserialize_into::<Block>()
        .expect("a valid block");

    verify_invalid_groth16_joinsplit_description(&mut verifier, block.transactions)
        .await
        .expect_err("unexpected success checking invalid groth16 inputs");
}
