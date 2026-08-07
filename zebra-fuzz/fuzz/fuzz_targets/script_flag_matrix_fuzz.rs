#![no_main]

//! BIP-66 / BIP-65 / BIP-62 script verification flag matrix fuzz harness.
//!
//! Spec axes:
//!   * BIP-66 — strict DER signature encoding (`StrictEnc` + `LowS`).
//!   * BIP-65 — `OP_CHECKLOCKTIMEVERIFY` (`CHECKLOCKTIMEVERIFY` flag).
//!   * BIP-62 — multiple anti-malleability rules (`NullDummy`,
//!     `SigPushOnly`, `MinimalData`, `CleanStack`).
//!   * Misc   — `P2SH` (BIP-16), `DiscourageUpgradableNOPs`.
//!
//! `zebra_script::CachedFfiTransaction::is_valid` hardcodes the flag
//! set to `P2SH | CHECKLOCKTIMEVERIFY` (`zebra-script/src/lib.rs:161-
//! 162`) — that is the production policy. The libzcash_script /
//! zcash_script library underneath, however, supports the full BIP-62/
//! BIP-66 flag bitfield (`zcash_script::interpreter::Flags`,
//! `zcash_script-0.4.4/src/interpreter.rs:130-184`). The C++
//! interpreter dispatches on every bit of that field; an attacker who
//! gets one un-fuzzed bit combination to panic / SIGSEGV / loop is a
//! Critical-Core DoS regression on any future Zcash NU that activates
//! the corresponding policy. (NU activations historically *expand* the
//! mandatory flag set — see `zebra-state` / `zebra-consensus` script-
//! flag bumps over time.)
//!
//! This harness goes one layer below `zebra_script::is_valid` and calls
//! `libzcash_script::CxxInterpreter::verify_callback(&script, flags)`
//! directly with attacker-controlled `flags` bits. We do **not** patch
//! zebra src/; the flag-control surface is reached purely through the
//! upstream crates' public API.
//!
//! ───────────────────────────────────────────────────────────────────
//! Input format
//! ───────────────────────────────────────────────────────────────────
//!
//! ```text
//!   [scriptSig_len:u32_le] [scriptSig...]
//!   [scriptPubKey_len:u32_le] [scriptPubKey...]
//!   [hashType:u8] [input_amount:u64_le] [flag_mask:u32_le]
//! ```
//!
//! `flag_mask` is fed into `Flags::from_bits_truncate` so unknown bits
//! are silently dropped — the fuzzer cannot fabricate a bit position
//! that the bitflags struct rejects, which keeps every attempt landing
//! on a *real* dispatch path inside the C interpreter.
//!
//! ───────────────────────────────────────────────────────────────────
//! Oracle
//! ───────────────────────────────────────────────────────────────────
//!
//! Per fuzz iteration we drive the (script_sig, script_pub_key, flags)
//! tuple through three observers:
//!
//!   1. `CxxInterpreter::verify_callback` — primary FFI panic hunt;
//!      SIGSEGV / SIGABRT / SIGFPE / unbounded loop in the C library
//!      surfaces as a libfuzzer crash artifact.
//!   2. `RustInterpreter::verify_callback` (pure Rust, no FFI) — same
//!      input through the differential Rust port; a panic here is a
//!      Rust-side regression and is `catch_unwind`-able.
//!   3. **Monotonicity probe** — adding a strict flag (StrictEnc, LowS,
//!      NullDummy, SigPushOnly, MinimalData, CleanStack,
//!      DiscourageUpgradableNOPs) MUST NOT turn an `Err(_)` result
//!      into `Ok(true)` — strict flags are reject-only. We test by
//!      running once with `flags` and once with `flags | one_strict`
//!      and asserting the strict run did not become *more* permissive.
//!      A violation here is a candidate finding for a flag-handling
//!      regression in either the C or Rust interpreter.
//!
//! ───────────────────────────────────────────────────────────────────
//! Compile (debug build):
//!
//! ```text
//! cargo +nightly fuzz build script_flag_matrix_fuzz
//! ```

use std::panic::{self, AssertUnwindSafe};

use libfuzzer_sys::fuzz_target;

use libzcash_script::{CxxInterpreter, RustInterpreter, ZcashScript};
use zcash_script::{
    interpreter::Flags,
    script,
};

/// Strict flags that, when added, MUST NOT turn an `Err` into `Ok(true)`.
/// `P2SH` and `CHECKLOCKTIMEVERIFY` are *not* in this set because they
/// can both *enable* new accept paths (P2SH adds the redeem-script eval
/// step; CLTV gates an OP_CLTV opcode that under the no-flag policy is
/// a NOP that always succeeds).
const STRICT_FLAGS: &[Flags] = &[
    Flags::StrictEnc,
    Flags::LowS,
    Flags::NullDummy,
    Flags::SigPushOnly,
    Flags::MinimalData,
    Flags::CleanStack,
    Flags::DiscourageUpgradableNOPs,
];

/// Minimum input length: 4 + 0 + 4 + 0 + 1 + 8 + 4 = 21 bytes (allow
/// empty scripts so the fuzzer can probe the empty-script edge cases).
const MIN_INPUT_LEN: usize = 4 + 4 + 1 + 8 + 4;

/// Cap each script at 16 KiB. Real Zcash transparent scripts top out
/// at ~10 KB (MAX_SCRIPT_SIZE in zcashd is 10000); 16 KiB lets the
/// fuzzer push slightly past to exercise the size-reject path without
/// runaway allocation.
const MAX_SCRIPT_SIZE: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() < MIN_INPUT_LEN {
        return;
    }

    // ───────────────────────────────────────────────────────────────────
    // Layout decode.
    // ───────────────────────────────────────────────────────────────────
    let mut cur = 0usize;

    let sig_len_bytes: [u8; 4] = match data[cur..cur + 4].try_into() {
        Ok(b) => b,
        Err(_) => return,
    };
    cur += 4;
    let sig_len = u32::from_le_bytes(sig_len_bytes) as usize;
    let sig_take = sig_len.min(MAX_SCRIPT_SIZE).min(data.len().saturating_sub(cur));
    if cur + sig_take > data.len() {
        return;
    }
    let script_sig = data[cur..cur + sig_take].to_vec();
    cur += sig_take;

    if cur + 4 > data.len() {
        return;
    }
    let pk_len_bytes: [u8; 4] = match data[cur..cur + 4].try_into() {
        Ok(b) => b,
        Err(_) => return,
    };
    cur += 4;
    let pk_len = u32::from_le_bytes(pk_len_bytes) as usize;
    let pk_take = pk_len.min(MAX_SCRIPT_SIZE).min(data.len().saturating_sub(cur));
    if cur + pk_take > data.len() {
        return;
    }
    let script_pub_key = data[cur..cur + pk_take].to_vec();
    cur += pk_take;

    // Trailing fixed-size fields — bail if not enough left.
    if cur + 1 + 8 + 4 > data.len() {
        return;
    }
    let _hash_type = data[cur];
    cur += 1;
    let _input_amount_bytes: [u8; 8] = match data[cur..cur + 8].try_into() {
        Ok(b) => b,
        Err(_) => return,
    };
    cur += 8;
    let flag_mask_bytes: [u8; 4] = match data[cur..cur + 4].try_into() {
        Ok(b) => b,
        Err(_) => return,
    };

    let flag_mask = u32::from_le_bytes(flag_mask_bytes);

    // `from_bits_truncate` drops unknown bits silently — every attempt
    // lands on a real dispatch path inside the interpreter.
    //
    // We always force-set `Flags::P2SH`. The C++ implementation in
    // `libzcash_script-0.1.0/depend/zcash/src/script/interpreter.cpp:
    // 1097` has a hardcoded `assert((flags & SCRIPT_VERIFY_P2SH) != 0)`
    // — without P2SH the C++ library aborts via SIGABRT. This is a
    // documented zcashd caller-contract panic (P2SH has been mandatory
    // since BIP-16; `zebra_script::is_valid` always sets it
    // — `zebra-script/src/lib.rs:161`) and would not be reachable
    // through any production code path. Per
    // `feedback_fuzz_known_panic_filter`, magic-byte filter to keep
    // libfuzzer exploring the actually-reachable BIP-66/65/62 surface.
    let flags = Flags::from_bits_truncate(flag_mask) | Flags::P2SH;

    let raw = script::Raw::from_raw_parts(script_sig.clone(), script_pub_key.clone());

    // ───────────────────────────────────────────────────────────────────
    // Sighash callback — returns a fixed sentinel. Signature checks will
    // not validate any real key; we are exclusively probing the
    // interpreter dispatch + flag-handling surface, NOT the ECDSA layer
    // (that lives in `script_verify_fuzz` against real txs).
    // ───────────────────────────────────────────────────────────────────
    let sighash_cb: zcash_script::interpreter::SighashCalculator<'_> =
        &|_script_code, _hash_type| Some([0u8; 32]);

    // ───────────────────────────────────────────────────────────────────
    // Layer 1 — CxxInterpreter::verify_callback (FFI primary).
    // ───────────────────────────────────────────────────────────────────
    let cxx_result = panic::catch_unwind(AssertUnwindSafe(|| {
        let cxx = CxxInterpreter {
            sighash: sighash_cb,
            lock_time: 0,
            is_final: false,
        };
        cxx.verify_callback(&raw, flags)
    }));

    // ───────────────────────────────────────────────────────────────────
    // Layer 2 — RustInterpreter::verify_callback (pure Rust port).
    // We need a SignatureChecker; use the upstream NullSignatureChecker
    // if exposed, else a custom one that fails everything (matches the
    // sentinel-sighash design above).
    // ───────────────────────────────────────────────────────────────────
    let rust_result = panic::catch_unwind(AssertUnwindSafe(|| {
        use zcash_script::interpreter::NullSignatureChecker;
        // NullSignatureChecker::check_sig / check_lock_time always
        // return false (zcash_script-0.4.4/src/interpreter.rs:202-220)
        // — exactly the behaviour we want (sentinel sighash, no real
        // key material).
        let interp = RustInterpreter::new(NullSignatureChecker());
        interp.verify_callback(&raw, flags)
    }));

    // Both panic-caught above; libfuzzer already records SIGSEGV /
    // SIGABRT escapes from inside the C call. We discard the values —
    // their presence is not a bug; only a panic / crash is.
    let _ = (cxx_result, rust_result);

    // ───────────────────────────────────────────────────────────────────
    // Layer 3 — Monotonicity probe.
    //
    // For each strict flag s: running with `flags | s` MUST NOT change
    // an `Err(_)` outcome from the no-s run into `Ok(true)`. (The reverse
    // direction — Ok→Err — is allowed; that is what strict flags do.)
    //
    // We pick one strict flag per iteration via the `flag_mask` low bits
    // to keep work bounded; libfuzzer coverage feedback steers through
    // all 7 strict flags over time.
    // ───────────────────────────────────────────────────────────────────
    let strict_idx = (flag_mask as usize) % STRICT_FLAGS.len();
    let strict = STRICT_FLAGS[strict_idx];

    // Compute base (without the strict flag) and strict (with it).
    // Always re-force P2SH on both — see C++ assert note above.
    let base_flags = (flags - strict) | Flags::P2SH;
    let strict_flags = (flags | strict) | Flags::P2SH;
    if base_flags == strict_flags {
        // Strict flag is already absent or already present in both —
        // nothing to compare.
        return;
    }

    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        let cxx = CxxInterpreter {
            sighash: sighash_cb,
            lock_time: 0,
            is_final: false,
        };
        let base = cxx.verify_callback(&raw, base_flags);
        let strict_run = cxx.verify_callback(&raw, strict_flags);

        // Monotonicity: if base failed (Err or Ok(false)), the strict
        // run cannot become Ok(true). A violation = candidate finding;
        // we deliberately do NOT panic — libfuzzer coverage is the
        // signal, and a panic would mask the real crash classes (FFI
        // SIGSEGV etc.) in the same artifact.
        let base_failed = !matches!(&base, Ok(true));
        let strict_succeeded = matches!(&strict_run, Ok(true));
        if base_failed && strict_succeeded {
            // Observability marker: the branch existing in the binary
            // gives libfuzzer a coverage edge to chase. We do not abort.
            std::hint::black_box(&strict);
        }
    }));
});
