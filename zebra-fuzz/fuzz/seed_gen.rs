//! Structural-diversity seed generator for the Ironwood v6-transaction fuzz
//! targets (`v6_transaction_fuzz`, `v6_transaction_semantic_fuzz`).
//!
//! Zebra's `Arbitrary for Transaction` only produces v4/v5 even for NU6.3, so a
//! coverage-guided fuzzer starting from v4/v5 seeds almost never stumbles into a
//! well-formed v6 wire structure. This tool constructs cryptographically-invalid
//! but *wire-valid* v6 transactions with the `fake_orchard_bundle` /
//! `fake_v6_transaction` test helpers, spanning the bundle-presence / flag /
//! value-balance / action-count space, so the seed corpus actually reaches the
//! v6 / Ironwood decode branches. Each candidate is self-checked to round-trip
//! through `zcash_deserialize` before it is written.
//!
//! Usage: `generate_v6_seeds <output_dir>`

use std::fs;
use std::io::Cursor;

use orchard::bundle::{Authorized, BundleVersion, Flags};
use orchard::Bundle;
use zcash_protocol::value::ZatBalance;

use zebra_chain::parameters::NetworkUpgrade;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::transaction::arbitrary::{fake_orchard_bundle, fake_v6_transaction};
use zebra_chain::transaction::Transaction;

/// The v6 Orchard-shaped bundle type carried in both the Orchard and Ironwood slots.
type V6Bundle = Bundle<Authorized, ZatBalance>;

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

/// Build a v6 Orchard-shaped bundle from the generator's bounded inputs.
fn build_bundle(
    bundle_version: BundleVersion,
    flag_byte: u8,
    value_balance: i64,
    action_count: usize,
    seed: u64,
) -> V6Bundle {
    let flags = Flags::from_byte(flag_byte, bundle_version)
        .expect("generator flags are valid for their bundle version");
    let value_balance =
        ZatBalance::from_i64(value_balance).expect("generator value balance is in range");

    fake_orchard_bundle(
        flags,
        value_balance,
        action_count,
        seed,
        bundle_version,
    )
}

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "v6_seeds".to_string());
    fs::create_dir_all(&out_dir).expect("create output dir");

    let nu = NetworkUpgrade::Nu6_3;

    // These bytes cover each valid combination for its pool: Orchard reserves
    // the cross-address bit, while Ironwood permits it.
    let orchard_flag_bytes: [u8; 4] = [0b000, 0b001, 0b010, 0b011];
    let ironwood_flag_bytes: [u8; 5] = [0b001, 0b010, 0b011, 0b101, 0b111];
    let value_balances: [i64; 5] = [0, 1, -1, 1_000_000, -1_000_000];
    let action_counts: [usize; 3] = [1, 2, 5];

    // Distinct per-bundle seed so bundles have disjoint nullifier sets.
    let mut seed: u64 = 0;
    let mut next_seed = || {
        seed = seed.wrapping_add(1);
        seed
    };

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
    for &fl in &orchard_flag_bytes {
        for &vb in &value_balances {
            for &ac in &action_counts {
                let bundle = build_bundle(BundleVersion::orchard_v3(), fl, vb, ac, next_seed());
                emit(
                    &fake_v6_transaction(nu, Some(bundle), None),
                    "orchard",
                    &out_dir,
                    &mut written,
                    &mut skipped,
                );
            }
        }
    }

    // Ironwood bundle only (exercises the Ironwood flag set + cross-address).
    for &fl in &ironwood_flag_bytes {
        for &vb in &value_balances {
            for &ac in &action_counts {
                let bundle = build_bundle(BundleVersion::ironwood_v3(), fl, vb, ac, next_seed());
                emit(
                    &fake_v6_transaction(nu, None, Some(bundle)),
                    "ironwood",
                    &out_dir,
                    &mut written,
                    &mut skipped,
                );
            }
        }
    }

    // Both bundles present (Orchard-v6 + Ironwood), a representative slice.
    for &ofl in &orchard_flag_bytes {
        for &ifl in &ironwood_flag_bytes {
            let orchard = build_bundle(BundleVersion::orchard_v3(), ofl, 1, 2, next_seed());
            let ironwood = build_bundle(BundleVersion::ironwood_v3(), ifl, -1, 2, next_seed());
            emit(
                &fake_v6_transaction(nu, Some(orchard), Some(ironwood)),
                "both",
                &out_dir,
                &mut written,
                &mut skipped,
            );
        }
    }

    println!("wrote {written} v6/Ironwood seeds to {out_dir} ({skipped} skipped)");
}
