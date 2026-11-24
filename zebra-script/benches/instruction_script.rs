//! Instruction-count guard for a signed transparent P2PKH spend.

#![allow(missing_docs)]

use std::{hint::black_box, sync::Arc};

use gungraun::prelude::*;
use zebra_chain::{parameters::NetworkUpgrade, serialization::ZcashDeserializeInto, transparent};
use zebra_script::CachedFfiTransaction;

// The signed Blossom transaction and its spent output are also exercised in
// zebra-script/src/tests.rs::verify_valid_script_v4.
const SIGNED_TX: &str = "0400008085202f8901fcaf44919d4a17f6181a02a7ebe0420be6f7dad1ef86755b81d5a9567456653c010000006a473044022035224ed7276e61affd53315eca059c92876bc2df61d84277cafd7af61d4dbf4002203ed72ea497a9f6b38eb29df08e830d99e32377edb8a574b8a289024f0241d7c40121031f54b095eae066d96b2557c1f99e40e967978a5fd117465dbec0986ca74201a6feffffff020050d6dc0100000017a9141b8a9bda4b62cd0d0582b55455d0778c86f8628f870d03c812030000001976a914e4ff5512ffafe9287992a1cd177ca6e408e0300388ac62070d0095070d000000000000000000000000";
const SPENT_SCRIPT: &str = "76a914f47cac1e6fec195c055994e8064ffccce0044dd788ac";

fn signed_spend() -> CachedFfiTransaction {
    let tx = hex::decode(SIGNED_TX).expect("test fixture is hex");
    let tx = tx
        .zcash_deserialize_into()
        .expect("test fixture is a valid transaction");
    let previous = transparent::Output {
        value: (212 * 100_000_000u64)
            .try_into()
            .expect("amount is in range"),
        lock_script: transparent::Script::new(&hex::decode(SPENT_SCRIPT).expect("script is hex")),
    };
    CachedFfiTransaction::new(
        Arc::new(tx),
        Arc::new(vec![previous]),
        NetworkUpgrade::Blossom,
    )
    .expect("Blossom has a consensus branch ID")
}

#[library_benchmark]
#[bench::signed_p2pkh(signed_spend())]
fn verify_signed_transparent_script(tx: CachedFfiTransaction) {
    black_box(tx.is_valid(0)).expect("test vector signature is valid");
}

library_benchmark_group!(
    name = mining_script,
    benchmarks = [verify_signed_transparent_script]
);
main!(library_benchmark_groups = mining_script);
