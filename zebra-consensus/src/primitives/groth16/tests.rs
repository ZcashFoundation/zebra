//! Tests for transaction verification

#![allow(clippy::unwrap_in_result)]

use futures::{
    future::ready,
    stream::{FuturesUnordered, StreamExt},
};
use hex::FromHex;
use tower::ServiceExt;

use zcash_primitives::transaction::components::sprout::JsDescription;

use zebra_chain::{
    block::Block,
    serialization::ZcashDeserializeInto,
    transaction::{Transaction, TxVersion},
};

use crate::primitives::groth16::*;

/// Builds a [`JsDescription`] from its wire-format fields.
///
/// `JsDescription`'s fields are private, so the only way to construct one is to serialize the
/// fields in wire order and parse them back. This keeps the tests on the same parsing path that
/// production uses, rather than on a second hand-written encoder.
///
/// The `ephemeral_key` and `enc_ciphertexts` fields are not covered by the proof, so callers can
/// pass dummy values for them.
#[allow(clippy::too_many_arguments)]
fn joinsplit_from_parts(
    vpub_old: i64,
    vpub_new: i64,
    anchor: [u8; 32],
    nullifiers: [[u8; 32]; 2],
    commitments: [[u8; 32]; 2],
    ephemeral_key: [u8; 32],
    random_seed: [u8; 32],
    macs: [[u8; 32]; 2],
    zkproof: [u8; 192],
) -> JsDescription {
    let mut bytes = Vec::new();

    bytes.extend_from_slice(&vpub_old.to_le_bytes());
    bytes.extend_from_slice(&vpub_new.to_le_bytes());
    bytes.extend_from_slice(&anchor);
    bytes.extend_from_slice(&nullifiers[0]);
    bytes.extend_from_slice(&nullifiers[1]);
    bytes.extend_from_slice(&commitments[0]);
    bytes.extend_from_slice(&commitments[1]);
    bytes.extend_from_slice(&ephemeral_key);
    bytes.extend_from_slice(&random_seed);
    bytes.extend_from_slice(&macs[0]);
    bytes.extend_from_slice(&macs[1]);
    bytes.extend_from_slice(&zkproof);
    // The two encrypted note ciphertexts are not validated by the proof.
    bytes.extend_from_slice(&[0u8; 601]);
    bytes.extend_from_slice(&[0u8; 601]);

    JsDescription::read(&bytes[..], true).expect("hand-built JoinSplit must parse")
}

async fn verify_groth16_joinsplits<V>(
    verifier: &mut V,
    transactions: Vec<std::sync::Arc<Transaction>>,
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<Item>>::Error: std::fmt::Debug
        + std::convert::From<
            std::boxed::Box<dyn std::error::Error + std::marker::Send + std::marker::Sync>,
        >,
{
    let _init_guard = zebra_test::init();

    let mut async_checks = FuturesUnordered::new();

    for tx in transactions {
        // Only V4 transactions carry Groth16 JoinSplit proofs; V2 and V3 use PHGR13, and the
        // verifier only reaches `joinsplit_to_item` from `verify_v4_transaction`.
        if tx.tx_version() != TxVersion::V4 {
            continue;
        }

        // Iterate the bundle exactly the way `verify_v4_transaction` does, so these tests
        // exercise the same code path that verifies blocks.
        if let Some(sprout_bundle) = tx.sprout_bundle() {
            for joinsplit in &sprout_bundle.joinsplits {
                let joinsplit_rsp = verifier.ready().await?.call(
                    joinsplit_to_item(joinsplit, &sprout_bundle.joinsplit_pubkey)
                        .map_err(tower_fallback::BoxedError::from)?,
                );

                async_checks.push(joinsplit_rsp);
            }
        }

        while let Some(result) = async_checks.next().await {
            tracing::trace!(?result);
            result?;
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_sprout_groth16() {
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(SPROUT.prepared_verifying_key())
                    .map_err(BoxedError::from),
            )
        }) as fn(_) -> _,
    );

    let transactions = zebra_test::vectors::MAINNET_BLOCKS
        .clone()
        .into_values()
        .flat_map(|bytes| {
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
    joinsplit: &JsDescription,
    pub_key: &[u8; 32],
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<Item>>::Error: std::fmt::Debug
        + std::convert::From<
            std::boxed::Box<dyn std::error::Error + std::marker::Send + std::marker::Sync>,
        >,
{
    let _init_guard = zebra_test::init();

    let mut async_checks = FuturesUnordered::new();

    let joinsplit_rsp = verifier
        .ready()
        .await?
        .call(joinsplit_to_item(joinsplit, pub_key).map_err(tower_fallback::BoxedError::from)?);

    async_checks.push(joinsplit_rsp);

    while let Some(result) = async_checks.next().await {
        tracing::trace!(?result);
        result?;
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_sprout_groth16_vector() {
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(SPROUT.prepared_verifying_key())
                    .map_err(BoxedError::from),
            )
        }) as fn(_) -> _,
    );

    // Test vector extracted manually by printing a JoinSplit generated by
    // the test_joinsplit test of the zcashd repository.
    // https://github.com/zcash/zcash/blob/7aaefab2d7d671951f153e47cdd83ae55d78144f/src/gtest/test_joinsplit.cpp#L48
    let joinsplit = joinsplit_from_parts(
        0x0A,
        0,
        <[u8; 32]>::from_hex("D7C612C817793191A1E68652121876D6B3BDE40F4FA52BC314145CE6E5CDD259")
            .unwrap(),
        [
            <[u8; 32]>::from_hex(
                "F9AD4EED10C97FF8FDE3C63512242D3937C0E2836389A95B972C50FB942F775B",
            )
            .unwrap(),
            <[u8; 32]>::from_hex(
                "DF6EB39839A549F0DF24CDEBBB23CA7107E84D2E6BD0294A8B1BFBD0FAE7800C",
            )
            .unwrap(),
        ],
        [
            <[u8; 32]>::from_hex(
                "0D595308A445D07EB62C7C13CB9F2630DFD39E6A060E98A9788C92BDDBAEA538",
            )
            .unwrap(),
            <[u8; 32]>::from_hex(
                "EE7D9622C410878A218ED8A8A6A10B11DDBDA83CB2A627508354BFA490E0F33E",
            )
            .unwrap(),
        ],
        // The ephemeral key is not validated in the proof, use a dummy value.
        [0u8; 32],
        <[u8; 32]>::from_hex("6A14E910A94EF500043A42417D8D2B4124AB35DC1E14DDF830EBCF972E850807")
            .unwrap(),
        [
            <[u8; 32]>::from_hex(
                "630D39F963960E9092E518CEF4C84853C13EF9FC759CBECDD2ED61D1070C82E6",
            )
            .unwrap(),
            <[u8; 32]>::from_hex(
                "1C8DCEC25F816D0177AC29958D0B8594EC669AED4A32D9FBEEC3C57B4503F19A",
            )
            .unwrap(),
        ],
        <[u8; 192]>::from_hex(
            "802BD3D746BA4831E10027C92E0E610618F619E3CE7EE087622BFF86F19B5BC3292DACFD27506C8BFF4C808035EB9C7685010235D47F1D77C5DCC212323E69726F04A46E0BBDCE17C64EEEA36F443E25F21DF2C39FE8A996BAE899AB8F8CCF52054DC6A5553D0F86283E056AED8E6EABE11D85EDF7948005AD9B982759F20E5DE54A59A1B80CD31AD4CC96419492886C91C4D7C521C327B47F4F5688067BE2B19EB8BC0B7BD357BF931CCF8BCC62A7E48A81CD287F00854767B41748F05EDD5B",
        )
        .unwrap(),
    );

    let pub_key =
        <[u8; 32]>::from_hex("63A144ABC0524C9EADE1DB9DE17AEC4A39626A0FDB597B9EC6DDA327EE9FE845")
            .unwrap();

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
    <V as tower::Service<Item>>::Error: std::convert::From<
        std::boxed::Box<dyn std::error::Error + std::marker::Send + std::marker::Sync>,
    >,
{
    let _init_guard = zebra_test::init();

    let mut async_checks = FuturesUnordered::new();

    for tx in transactions {
        if tx.tx_version() != TxVersion::V4 {
            continue;
        }

        if let Some(sprout_bundle) = tx.sprout_bundle() {
            for joinsplit in &sprout_bundle.joinsplits {
                // Use an arbitrary public key which is not the correct one,
                // which will make the verification fail.
                let modified_pub_key = [0x42; 32];

                let joinsplit_rsp = verifier.ready().await?.call(
                    joinsplit_to_item(joinsplit, &modified_pub_key)
                        .map_err(tower_fallback::BoxedError::from)?,
                );

                async_checks.push(joinsplit_rsp);
            }
        }

        while let Some(result) = async_checks.next().await {
            result?;
        }
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn correctly_err_on_invalid_joinsplit_proof() {
    // Use separate verifiers so shared batch tasks aren't killed when the test ends (#2390).
    // Also, since we expect these to fail, we don't want to slow down the communal verifiers.
    let mut verifier = tower::service_fn(
        (|item: Item| {
            ready(
                item.verify_single(SPROUT.prepared_verifying_key())
                    .map_err(BoxedError::from),
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
