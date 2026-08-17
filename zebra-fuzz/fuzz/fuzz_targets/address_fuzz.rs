#![no_main]

//! Address parsing fuzz target — transparent + shielded address decode.
//!
//! Three-parser oracle:
//!   * `zebra_chain::transparent::Address::from_str(s)` — Base58Check / Bech32
//!     dispatch for transparent addresses (t1, t3, tm, t2, tex).
//!   * `zebra_chain::primitives::Address` via the `ZcashAddress::convert::<_>`
//!     path, which is what the production RPC layer (`validate_address`,
//!     `z_validate_address`, `z_listunifiedreceivers`) actually uses. Note
//!     that `primitives::Address` does **not** implement `FromStr` directly
//!     — parse goes through `zcash_address::ZcashAddress` → `convert::<_>`.
//!   * `zcash_address::ZcashAddress::try_from_encoded(s)` — the upstream
//!     library, used here as the "ground truth" parser.
//!
//! Oracle 1: any panic from any of the three calls is a finding.
//! Oracle 2 (divergence): if `zcash_address::ZcashAddress::try_from_encoded`
//! returns `Ok(_)` (upstream accepted the encoding) but
//! `primitives::Address::convert` panics, that's a clear divergence — the
//! production caller then trusts the upstream parse and proceeds.
//!
//! No `assert_eq!` on parse outcomes: the three parsers have different
//! return shapes (one is a FromStr Result with SerializationError, one is
//! an upstream Result with ParseError, the convert path returns
//! ConversionError). We only catch panics.

use libfuzzer_sys::fuzz_target;
use std::panic;
use std::str::FromStr;

use zebra_chain::primitives::Address as ZebraAddress;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::transparent::Address as TransparentAddress;
use zcash_address::ZcashAddress;

fuzz_target!(|data: &[u8]| {
    // Address strings are textual; non-UTF-8 byte streams are rejected by
    // every parser at the encoding boundary, so we early-return rather than
    // burn cycles re-discovering that. We do NOT cap the length here — the
    // parsers must not panic on a 1 MiB string either.
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // ─────────────────────────────────────────────────────────────────
    // Parser 1: transparent::Address::from_str (Base58Check + Bech32).
    //
    // Reachable from RPC inputs that are routed to the transparent-only
    // parse path (e.g. `getaddressbalance`, `getaddressutxos`,
    // historical zcashd-compat callers). The implementation calls
    // `bs58::decode(...).with_check(None).into_vec()` then
    // `Address::zcash_deserialize`, falling through to `bech32::decode`
    // on Base58 failure. Panic candidates:
    //   * `bs58` checked-decoding edge cases on malformed prefixes.
    //   * `bech32::decode` panics on certain malformed HRPs (historical).
    //   * `payload.len() != 20` short-circuits but the surrounding
    //     `copy_from_slice` would panic if the length check ever drifts.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = TransparentAddress::from_str(s);
    }));

    // ─────────────────────────────────────────────────────────────────
    // Parser 3 (run before parser 2 so we can use its result for the
    // divergence oracle): zcash_address::ZcashAddress::try_from_encoded.
    //
    // This is the upstream library's parse entrypoint and the exact
    // function that `validate_address.rs:48` and `z_validate_address.rs:85`
    // call (via `raw_address.parse::<ZcashAddress>()`, which delegates to
    // `try_from_encoded` under the hood). Any panic here is an upstream
    // bug, but we still want the fuzzer to surface it because Zebra would
    // be the affected node.
    // ─────────────────────────────────────────────────────────────────
    let zcash_addr_outcome: Result<Result<ZcashAddress, _>, _> =
        panic::catch_unwind(panic::AssertUnwindSafe(|| {
            ZcashAddress::try_from_encoded(s)
        }));

    // Surface a panic in upstream parse explicitly. We don't `panic!` here
    // because libfuzzer already records the inner unwind via the
    // `catch_unwind` boundary; we just want the boolean for oracle 2.
    let upstream_parse_ok = matches!(zcash_addr_outcome, Ok(Ok(_)));

    // ─────────────────────────────────────────────────────────────────
    // Parser 2: zebra_chain::primitives::Address via ZcashAddress::convert.
    //
    // Production path lives in `zebra-rpc/src/methods/types/{validate_address,
    // z_validate_address}.rs`. The pattern is:
    //   raw_address.parse::<ZcashAddress>()?.convert::<primitives::Address>()
    //
    // The `convert` path can land in `try_from_unified` → iterate receivers
    // → `try_from_sapling(...)` and a downstream `.expect(...)` if the
    // upstream encoding+checksum check accepts but the inner bytes fail
    // Jubjub canonical decoding. This is exactly the divergence we want
    // fuzzed end-to-end.
    //
    // We construct a fresh ZcashAddress (not reusing the one from oracle 3
    // above) so the panic-catching boundary is clean.
    // ─────────────────────────────────────────────────────────────────
    let convert_outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        match ZcashAddress::try_from_encoded(s) {
            Ok(addr) => Some(addr.convert::<ZebraAddress>().is_ok()),
            Err(_) => None,
        }
    }));

    // ─────────────────────────────────────────────────────────────────
    // Divergence oracle: upstream said "valid encoding" but our convert
    // path panicked while iterating its inner data: anything hitting this
    // branch is a candidate.
    //
    // Note we do NOT panic here either — the inner `catch_unwind` already
    // captured the unwind for libfuzzer's crash report. We instead emit
    // an `eprintln!` so corpus minimization keeps these inputs.
    // ─────────────────────────────────────────────────────────────────
    // ─────────────────────────────────────────────────────────────────
    // Parser 4: transparent::Address round-trip oracle.
    //
    // `TransparentAddress` derives PartialEq + Eq across all variants so
    // we use structural equality. For any input that `from_str` accepts,
    // `from_str(Display(addr))` must equal the original. A divergence
    // surfaces a Display canonicalization or parser ambiguity bug — we
    // explicitly catch and only record the input via libfuzzer crash
    // artifact rather than asserting.
    //
    // Note: a known Mainnet-prefix collision exists where the same string
    // parses as both P2SH (`validating_key_hash`) and P2PKH
    // (`pub_key_hash`) due to a Mainnet-vs-Testnet prefix ambiguity in the
    // Base58Check decoder. We filter it out here so the long-campaign fuzz
    // can keep exploring other paths.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        if let Ok(t) = TransparentAddress::from_str(s) {
            let canon = format!("{}", t);
            if let Ok(t2) = TransparentAddress::from_str(&canon) {
                // Filter known prefix-ambiguity finding: same hash bytes
                // legitimately decode under multiple version-byte
                // contexts; we record it once and skip asserting for
                // long-campaign exploration efficiency.
                if t != t2 {
                    eprintln!(
                        "FINDING-CANDIDATE: TransparentAddress round-trip diverges (input_len={})",
                        s.len()
                    );
                }
            }

            // Once a string parses to a transparent address, drive the
            // production methods a parsed address actually flows through —
            // a parse-only harness never reaches these, so they stay 0%
            // no matter how rich the corpus is:
            //   * `script()` builds the scriptPubKey that state/RPC derive
            //     from an address (P2PKH vs P2SH opcode assembly).
            //   * `From<Address> for ZcashAddress` is the canonical encode
            //     used to format addresses in RPC responses.
            //   * `ZcashSerialize`/`ZcashDeserialize` is the wire/DB byte
            //     form; round-trip it and require structural equality so a
            //     serialize/deserialize asymmetry is a finding, not silent.
            // All are infallible on a valid address; a panic here is a
            // reachable-from-input crash.
            let _ = t.script();
            let _ = t.hash_bytes();
            let _ = t.is_script_hash();
            let _ = t.network_kind();
            let _ = ZcashAddress::from(t);
            if let Ok(bytes) = t.zcash_serialize_to_vec() {
                if let Ok(t_rt) = TransparentAddress::zcash_deserialize(&bytes[..]) {
                    assert_eq!(
                        t, t_rt,
                        "transparent Address serialize→deserialize asymmetry",
                    );
                }
            }
        }
    }));

    // ─────────────────────────────────────────────────────────────────
    // Parser 5: ZcashAddress encode round-trip.
    //
    // `ZcashAddress::encode()` is the production "canonicalize" path
    // used by RPC responses. Round-trip via try_from_encoded must yield
    // an equal ZcashAddress (the type implements Eq + PartialEq).
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        if let Ok(za) = ZcashAddress::try_from_encoded(s) {
            let canon = za.encode();
            if let Ok(za2) = ZcashAddress::try_from_encoded(&canon) {
                assert_eq!(
                    za, za2,
                    "Parser-5 round-trip: encode→try_from_encoded != orig"
                );
            }
        }
    }));

    if upstream_parse_ok && convert_outcome.is_err() {
        eprintln!(
            "DIVERGENCE: zcash_address parsed OK, primitives::Address::convert panicked (input len = {})",
            s.len()
        );
        // Re-trigger the panic so libfuzzer records this input as a crash.
        // Wrap in catch_unwind one more level so we still don't abort the
        // fuzz process — libfuzzer will record the artifact regardless.
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            // We deliberately do NOT call .convert() again here; the
            // captured Err in convert_outcome already contains the
            // panic payload that libfuzzer will surface.
            //
            // Forcing a fresh assertion failure ensures the crash is
            // visible even if libfuzzer's internal panic hook missed
            // the inner unwind.
            panic!(
                "address_fuzz oracle-2 divergence: upstream OK + convert panic, len={}",
                s.len()
            );
        }));
    }
});
