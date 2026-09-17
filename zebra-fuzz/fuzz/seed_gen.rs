//! Structural-diversity seed generator for the Ironwood v6-transaction fuzz
//! targets (`v6_transaction_fuzz`, `v6_transaction_semantic_fuzz`).
//!
//! Zebra's `Arbitrary for Transaction` only produces v4/v5 even for NU6.3, so a
//! coverage-guided fuzzer starting from v4/v5 seeds almost never stumbles into a
//! well-formed v6 wire structure. This tool constructs cryptographically-invalid
//! but *wire-valid* v6 transactions with the `fake_v6_*` test helpers, spanning
//! the bundle-presence / flag / value-balance / action-count space, so the seed
//! corpus actually reaches the v6 / Ironwood decode branches. Each candidate is
//! self-checked to round-trip through `zcash_deserialize` before it is written.
//!
//! Usage: `generate_v6_seeds <output_dir>`

use std::fs;
use std::io::Cursor;

use zebra_chain::amount::{Amount, NegativeAllowed};
use zebra_chain::ironwood;
use zebra_chain::orchard::shielded_data::Flags;
use zebra_chain::orchard::ShieldedDataV6;
use zebra_chain::parameters::NetworkUpgrade;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::transaction::arbitrary::{fake_v6_orchard_shielded_data, fake_v6_transaction};
use zebra_chain::transaction::Transaction;

/// Emit `tx` as a seed file iff it round-trips through the wire deserializer, so
/// every seed is a valid decode that reaches the deep v6 code (not a reject).
fn emit(tx: &Transaction, tag: &str, out_dir: &str, written: &mut usize, skipped: &mut usize) {
    match tx.zcash_serialize_to_vec() {
        Ok(bytes) if Transaction::zcash_deserialize(Cursor::new(&bytes)).is_ok() => {
            let path = format!("{}/v6_{}_{:05}.bin", out_dir, tag, *written);
            fs::write(&path, &bytes).expect("write seed");
            *written += 1;
        }
        _ => *skipped += 1,
    }
}

fn orchard_bundle(fl: Flags, vb: i64, ac: usize) -> Option<ShieldedDataV6> {
    Amount::<NegativeAllowed>::try_from(vb)
        .ok()
        .map(|amount| ShieldedDataV6::new(fake_v6_orchard_shielded_data(fl, amount, ac)))
}

fn ironwood_bundle(fl: Flags, vb: i64, ac: usize) -> Option<ironwood::ShieldedData> {
    Amount::<NegativeAllowed>::try_from(vb).ok().map(|amount| {
        ironwood::ShieldedData::new(ShieldedDataV6::new(fake_v6_orchard_shielded_data(
            fl, amount, ac,
        )))
    })
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "v6_seeds".to_string());
    fs::create_dir_all(&out_dir).expect("create output dir");

    let nu = NetworkUpgrade::Nu6_3;

    // Orchard pool forbids bit 2 (cross-address); Ironwood pool permits it.
    let orchard_flags = [
        Flags::empty(),
        Flags::ENABLE_SPENDS,
        Flags::ENABLE_OUTPUTS,
        Flags::ENABLE_SPENDS | Flags::ENABLE_OUTPUTS,
    ];
    let ironwood_flags = [
        Flags::ENABLE_SPENDS,
        Flags::ENABLE_OUTPUTS,
        Flags::ENABLE_SPENDS | Flags::ENABLE_OUTPUTS,
        Flags::ENABLE_SPENDS | Flags::ENABLE_CROSS_ADDRESS,
        Flags::ENABLE_SPENDS | Flags::ENABLE_OUTPUTS | Flags::ENABLE_CROSS_ADDRESS,
    ];
    let value_balances: [i64; 5] = [0, 1, -1, 1_000_000, -1_000_000];
    let action_counts: [usize; 3] = [1, 2, 5];

    let mut written = 0usize;
    let mut skipped = 0usize;

    // Transparent-only v6 (no shielded bundles).
    emit(
        &fake_v6_transaction(nu, None, None),
        "bare",
        &out_dir,
        &mut written,
        &mut skipped,
    );

    // Orchard-v6 bundle only.
    for &fl in &orchard_flags {
        for &vb in &value_balances {
            for &ac in &action_counts {
                emit(
                    &fake_v6_transaction(nu, orchard_bundle(fl, vb, ac), None),
                    "orchard",
                    &out_dir,
                    &mut written,
                    &mut skipped,
                );
            }
        }
    }

    // Ironwood bundle only (exercises FlagsV6 + cross-address).
    for &fl in &ironwood_flags {
        for &vb in &value_balances {
            for &ac in &action_counts {
                emit(
                    &fake_v6_transaction(nu, None, ironwood_bundle(fl, vb, ac)),
                    "ironwood",
                    &out_dir,
                    &mut written,
                    &mut skipped,
                );
            }
        }
    }

    // Both bundles present (Orchard-v6 + Ironwood), a representative slice.
    for &ofl in &orchard_flags {
        for &ifl in &ironwood_flags {
            emit(
                &fake_v6_transaction(nu, orchard_bundle(ofl, 1, 2), ironwood_bundle(ifl, -1, 2)),
                "both",
                &out_dir,
                &mut written,
                &mut skipped,
            );
        }
    }

    println!("wrote {written} v6/Ironwood seeds to {out_dir} ({skipped} skipped)");
}
