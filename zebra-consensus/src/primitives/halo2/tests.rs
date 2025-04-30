//! Tests for verifying simple Halo2 proofs with the async verifier

use std::future::ready;

use futures::stream::{FuturesUnordered, StreamExt};
use tower::ServiceExt;

use halo2::pasta::{group::ff::PrimeField, pallas};
use orchard::{
    builder::{Builder, BundleType},
    bundle::Flags,
    circuit::ProvingKey,
    keys::{FullViewingKey, Scope, SpendingKey},
    note::AssetBase,
    value::NoteValue,
    Anchor, Bundle,
};
use rand::rngs::OsRng;

use zebra_chain::{
    orchard::{ShieldedData, ShieldedDataFlavor},
    serialization::{ZcashDeserialize, ZcashDeserializeInto, ZcashSerialize},
};

use crate::primitives::halo2::*;

// FIXME: Where is this function called from?
#[allow(dead_code, clippy::print_stdout)]
fn generate_test_vectors<Flavor: ShieldedDataFlavor>()
where
    Flavor::BurnType: for<'a> From<&'a [(AssetBase, NoteValue)]>,
    ShieldedData<Flavor>: ZcashSerialize,
{
    let proving_key = ProvingKey::build::<Flavor>();

    let rng = OsRng;

    let sk = SpendingKey::from_bytes([7; 32]).unwrap();
    let recipient = FullViewingKey::from(&sk).address_at(0u32, Scope::External);

    // FIXME: Add ENABLE_ZSA for OrchardZSA?
    let flags =
        zebra_chain::orchard::Flags::ENABLE_SPENDS | zebra_chain::orchard::Flags::ENABLE_OUTPUTS;

    let anchor_bytes = [0; 32];
    let note_value = 10;

    let shielded_data: Vec<ShieldedData<Flavor>> = (1..=4)
        .map(|num_recipients| {
            let mut builder = Builder::new(
                BundleType::Transactional {
                    flags: Flags::from_byte(flags.bits()).unwrap(),
                    bundle_required: true,
                },
                Anchor::from_bytes(anchor_bytes).unwrap(),
            );

            for _ in 0..num_recipients {
                builder
                    .add_output(
                        None,
                        recipient,
                        NoteValue::from_raw(note_value),
                        // FIXME: Use another AssetBase for OrchardZSA?
                        AssetBase::native(),
                        None,
                    )
                    .unwrap();
            }

            let bundle: Bundle<_, i64, Flavor> = builder.build(rng).unwrap().0;

            let bundle = bundle
                .create_proof(&proving_key, rng)
                .unwrap()
                .apply_signatures(rng, [0; 32], &[])
                .unwrap();

            ShieldedData::<Flavor> {
                flags,
                value_balance: note_value.try_into().unwrap(),
                shared_anchor: anchor_bytes.try_into().unwrap(),
                proof: zebra_chain::primitives::Halo2Proof(
                    bundle.authorization().proof().as_ref().into(),
                ),
                actions: bundle
                    .actions()
                    .iter()
                    .map(|a| {
                        let action = zebra_chain::orchard::Action::<Flavor> {
                            cv: a.cv_net().to_bytes().try_into().unwrap(),
                            nullifier: a.nullifier().to_bytes().try_into().unwrap(),
                            rk: <[u8; 32]>::from(a.rk()).into(),
                            cm_x: pallas::Base::from_repr(a.cmx().into()).unwrap(),
                            ephemeral_key: a.encrypted_note().epk_bytes.try_into().unwrap(),
                            enc_ciphertext: a
                                .encrypted_note()
                                .enc_ciphertext
                                .as_ref()
                                .try_into()
                                .unwrap(),
                            out_ciphertext: a.encrypted_note().out_ciphertext.into(),
                        };
                        zebra_chain::orchard::shielded_data::AuthorizedAction {
                            action,
                            spend_auth_sig: <[u8; 64]>::from(a.authorization()).into(),
                        }
                    })
                    .collect::<Vec<_>>()
                    .try_into()
                    .unwrap(),
                binding_sig: <[u8; 64]>::from(bundle.authorization().binding_signature()).into(),
                #[cfg(feature = "tx-v6")]
                burn: bundle.burn().as_slice().into(),
            }
        })
        .collect();

    for sd in shielded_data {
        println!(
            "{}",
            hex::encode(sd.clone().zcash_serialize_to_vec().unwrap())
        );
    }
}

async fn verify_orchard_halo2_proofs<V, Flavor: OrchardVerifier>(
    verifier: &mut V,
    shielded_data: Vec<ShieldedData<Flavor>>,
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<Item>>::Error: From<tower::BoxError> + std::fmt::Debug,
{
    let mut async_checks = FuturesUnordered::new();

    for sd in shielded_data {
        tracing::trace!(?sd);

        let rsp = verifier.ready().await?.call(Item::from(&sd));

        async_checks.push(rsp);
    }

    while let Some(result) = async_checks.next().await {
        tracing::trace!(?result);
        result?;
    }

    Ok(())
}

async fn verify_generated_halo2_proofs<V: OrchardVerifier>(shielded_data_test_vectors: &[&[u8]])
where
    Option<ShieldedData<V>>: ZcashDeserialize,
{
    let _init_guard = zebra_test::init();

    // These test vectors are generated by `generate_test_vectors()` function.
    let shielded_data = shielded_data_test_vectors
        .iter()
        .map(|bytes| {
            let maybe_shielded_data: Option<ShieldedData<V>> = bytes
                .zcash_deserialize_into()
                .expect("a valid orchard::ShieldedData instance");
            maybe_shielded_data.unwrap()
        })
        .collect();

    // Use separate verifier so shared batch tasks aren't killed when the test ends (#2390)
    let mut verifier = Fallback::new(
        Batch::new(
            Verifier::new(OrchardVanilla::get_verifying_key()),
            crate::primitives::MAX_BATCH_SIZE,
            None,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(
                    item.verify_single(V::get_verifying_key())
                        .map_err(Halo2Error::from),
                )
            }) as fn(_) -> _,
        ),
    );

    // This should fail if any of the proofs fail to validate.
    assert!(verify_orchard_halo2_proofs(&mut verifier, shielded_data)
        .await
        .is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_generated_halo2_proofs_vanilla() {
    verify_generated_halo2_proofs::<OrchardVanilla>(
        &zebra_test::vectors::ORCHARD_SHIELDED_DATA_VANILLA,
    )
    .await
}

#[cfg(feature = "tx-v6")]
#[tokio::test(flavor = "multi_thread")]
async fn verify_generated_halo2_proofs_zsa() {
    verify_generated_halo2_proofs::<OrchardZSA>(&zebra_test::vectors::ORCHARD_SHIELDED_DATA_ZSA)
        .await
}

async fn verify_invalid_orchard_halo2_proofs<V, Flavor: OrchardVerifier>(
    verifier: &mut V,
    shielded_data: Vec<ShieldedData<Flavor>>,
) -> Result<(), V::Error>
where
    V: tower::Service<Item, Response = ()>,
    <V as tower::Service<Item>>::Error: From<tower::BoxError> + std::fmt::Debug,
{
    let mut async_checks = FuturesUnordered::new();

    for sd in shielded_data {
        let mut sd = sd.clone();

        sd.flags.remove(zebra_chain::orchard::Flags::ENABLE_SPENDS);
        sd.flags.remove(zebra_chain::orchard::Flags::ENABLE_OUTPUTS);
        // FIXME: What about zebra_chain::orchard::Flags::ENABLE_ZSA?

        tracing::trace!(?sd);

        let rsp = verifier.ready().await?.call(Item::from(&sd));

        async_checks.push(rsp);
    }

    while let Some(result) = async_checks.next().await {
        tracing::trace!(?result);
        result?;
    }

    Ok(())
}

async fn correctly_err_on_invalid_halo2_proofs<V: OrchardVerifier>(
    shielded_data_test_vectors: &[&[u8]],
) where
    Option<ShieldedData<V>>: ZcashDeserialize,
{
    let _init_guard = zebra_test::init();

    // These test vectors are generated by `generate_test_vectors()` function.
    let shielded_data = shielded_data_test_vectors
        .iter()
        .map(|bytes| {
            let maybe_shielded_data: Option<ShieldedData<V>> = bytes
                .zcash_deserialize_into()
                .expect("a valid orchard::ShieldedData instance");
            maybe_shielded_data.unwrap()
        })
        .collect();

    // Use separate verifier so shared batch tasks aren't killed when the test ends (#2390)
    let mut verifier = Fallback::new(
        Batch::new(
            Verifier::new(OrchardVanilla::get_verifying_key()),
            crate::primitives::MAX_BATCH_SIZE,
            None,
            crate::primitives::MAX_BATCH_LATENCY,
        ),
        tower::service_fn(
            (|item: Item| {
                ready(
                    item.verify_single(V::get_verifying_key())
                        .map_err(Halo2Error::from),
                )
            }) as fn(_) -> _,
        ),
    );

    // This should fail if any of the proofs fail to validate.
    assert!(
        verify_invalid_orchard_halo2_proofs(&mut verifier, shielded_data)
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn correctly_err_on_invalid_halo2_proofs_vanilla() {
    correctly_err_on_invalid_halo2_proofs::<OrchardVanilla>(
        &zebra_test::vectors::ORCHARD_SHIELDED_DATA_VANILLA,
    )
    .await
}

#[cfg(feature = "tx-v6")]
#[tokio::test(flavor = "multi_thread")]
async fn correctly_err_on_invalid_halo2_proofs_zsa() {
    correctly_err_on_invalid_halo2_proofs::<OrchardZSA>(
        &zebra_test::vectors::ORCHARD_SHIELDED_DATA_ZSA,
    )
    .await
}
