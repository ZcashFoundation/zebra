//! Instruction-count guard for shielded proof and signature verification.

#![allow(missing_docs)]

use std::{hint::black_box, sync::Arc};

use gungraun::prelude::*;
use orchard::bundle::{
    Authorized as OrchardAuthorized, BatchValidator as OrchardBatch, Bundle as OrchardBundle,
};
use rand::{rngs::StdRng, SeedableRng};
use sapling_crypto::{
    bundle::Authorized as SaplingAuthorized,
    circuit::{OutputVerifyingKey, SpendVerifyingKey},
    BatchValidator as SaplingBatch, Bundle as SaplingBundle,
};
use zcash_proofs::prover::LocalTxProver;
use zcash_protocol::value::ZatBalance;
use zebra_chain::{
    block::{self, Block},
    parameters::{Network, NetworkUpgrade},
    serialization::ZcashDeserializeInto,
    transaction::{HashType, SigHash},
    transparent,
};
use zebra_consensus::halo2::VERIFYING_KEY_PRE_NU6_2;

type OrchardInput = (OrchardBundle<OrchardAuthorized, ZatBalance>, SigHash);
type SaplingInput = (
    SaplingBundle<SaplingAuthorized, ZatBalance>,
    SigHash,
    SpendVerifyingKey,
    OutputVerifyingKey,
);

/// Runs Rayon work on the measured thread, since Callgrind counts only the main thread.
fn rayon_on_this_thread() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .use_current_thread()
        .build_global()
        .expect("nothing else builds the global pool in a benchmark process");
}

fn orchard_input() -> OrchardInput {
    rayon_on_this_thread();
    let input = find_orchard_input();
    // Build the verifying key and its proof layouts outside the measured function.
    assert!(validate_two_orchard_bundles(&input));
    input
}

fn find_orchard_input() -> OrchardInput {
    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let nu = NetworkUpgrade::current(&Network::Mainnet, block::Height(*height));
        if nu != NetworkUpgrade::Nu5 {
            continue;
        }
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("test vector block is valid");
        for tx in &block.transactions {
            if !tx.has_orchard_shielded_data() || !tx.inputs().is_empty() {
                continue;
            }
            let previous = Arc::new(Vec::<transparent::Output>::new());
            if let Ok(sighasher) = tx.sighasher(nu, previous) {
                if let Some(bundle) = sighasher.orchard_bundle() {
                    return (bundle, sighasher.sighash(HashType::ALL, None));
                }
            }
        }
    }
    panic!("mainnet test blocks contain an Orchard transaction without transparent inputs");
}

fn sapling_input() -> SaplingInput {
    rayon_on_this_thread();
    let (spend_vk, output_vk) = LocalTxProver::bundled().verifying_keys();
    for (height, bytes) in zebra_test::vectors::MAINNET_BLOCKS.iter() {
        let block: Block = bytes
            .zcash_deserialize_into()
            .expect("test vector block is valid");
        let nu = NetworkUpgrade::current(&Network::Mainnet, block::Height(*height));
        for tx in &block.transactions {
            if !tx.has_sapling_shielded_data() || !tx.inputs().is_empty() {
                continue;
            }
            let previous = Arc::new(Vec::<transparent::Output>::new());
            if let Ok(sighasher) = tx.sighasher(nu, previous) {
                if let Some(bundle) = sighasher.sapling_bundle() {
                    return (
                        bundle,
                        sighasher.sighash(HashType::ALL, None),
                        spend_vk,
                        output_vk,
                    );
                }
            }
        }
    }
    panic!("mainnet test blocks contain a Sapling transaction without transparent inputs");
}

fn validate_two_orchard_bundles((bundle, sighash): &OrchardInput) -> bool {
    let mut batch = OrchardBatch::new(&VERIFYING_KEY_PRE_NU6_2);
    for _ in 0..2 {
        batch
            .add_bundle(bundle, sighash.0)
            .expect("NU5 Orchard bundle matches the verifying key");
    }
    batch.validate(StdRng::from_seed([7; 32]))
}

// The Orchard batch validator checks the bundles' proofs and their RedPallas signatures.
#[library_benchmark]
#[bench::two_bundles(orchard_input())]
fn orchard_proofs_and_signatures(input: OrchardInput) {
    assert!(black_box(validate_two_orchard_bundles(&input)));
}

#[library_benchmark]
#[bench::real_sapling_transaction(sapling_input())]
fn sapling_proof_and_signature((bundle, sighash, spend_vk, output_vk): SaplingInput) {
    let mut batch = SaplingBatch::default();
    assert!(batch.check_bundle(bundle, sighash.into()));
    assert!(black_box(batch.validate(
        &spend_vk,
        &output_vk,
        StdRng::from_seed([7; 32])
    )));
}

library_benchmark_group!(
    name = mining_consensus,
    benchmarks = [orchard_proofs_and_signatures, sapling_proof_and_signature]
);
main!(library_benchmark_groups = mining_consensus);
