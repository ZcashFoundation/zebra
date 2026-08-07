#![no_main]

//! `ironwood_value_balance_codec_fuzz` — the `ValueBalance` on-disk state record
//! codec, which grew a 48-byte form under NU6.3 to carry the Ironwood pool.
//!
//! This is a *state* codec, not the transaction wire: `ValueBalance::from_bytes`
//! parses the per-pool value-balance record Zebra reads back from its database
//! (`ValueBalance::to_bytes` writes it). NU6.3 appends the Ironwood pool after
//! `deferred`, so the record is now 48 bytes; `from_bytes` still accepts the
//! legacy 32-byte (pre-`deferred`) and 40-byte (pre-Ironwood) forms for
//! backward compatibility. `from_bytes` carries `#[allow(clippy::unwrap_in_result)]`,
//! so this target deliberately probes those in-`Result` unwraps for any input
//! that passes the length gate but trips an inner conversion.
//!
//! Oracle: decode must not panic; and the canonical 48-byte re-encoding is
//! idempotent (`from_bytes(to_bytes(from_bytes(x))) == to_bytes(from_bytes(x))`).

use libfuzzer_sys::fuzz_target;
use zebra_chain::amount::NonNegative;
use zebra_chain::value_balance::ValueBalance;

fuzz_target!(|data: &[u8]| {
    // Only 32/40/48-byte inputs get past the length gate; all others must return
    // Err (not panic). A successful parse is the interesting case.
    let vb = match ValueBalance::<NonNegative>::from_bytes(data) {
        Ok(vb) => vb,
        Err(_) => return,
    };

    // to_bytes always emits the 48-byte canonical form; re-parsing it must
    // succeed and re-encode to the identical bytes (idempotent canonical form).
    let canonical = vb.to_bytes();
    let reparsed = ValueBalance::<NonNegative>::from_bytes(&canonical)
        .expect("a freshly-encoded 48-byte canonical record must re-parse");
    assert_eq!(
        canonical,
        reparsed.to_bytes(),
        "ValueBalance state-codec round-trip is not idempotent"
    );
});
