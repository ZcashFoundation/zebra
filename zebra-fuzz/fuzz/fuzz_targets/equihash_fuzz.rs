#![no_main]

//! Equihash PoW solution fuzz target — consensus-critical verification surface.
//!
//! Goal: hammer Zebra's `Solution::check()` and the `equihash_solution_is_valid`
//! consensus wrapper with attacker-controlled wire bytes. This is the PoW
//! verifier every inbound P2P block header is forced through, so a panic =
//! Critical Remote DoS. A round-trip / cross-network divergence here is the
//! early-warning detector for a consensus-split where Zebra and zcashd
//! disagree on whether the same Equihash solution is valid.
//!
//! Attack surfaces in scope:
//!   Path 1: Header::zcash_deserialize → header.solution.check(&header)
//!           — direct consensus-critical PoW verification
//!   Path 2: Solution::zcash_deserialize (standalone) — exercises the variable-
//!           length compact-size + length-mismatch branches in
//!           equihash::Solution::from_bytes
//!   Path 3: Header round-trip + hash() / solution bytes equality —
//!           consensus-split early detector (a serialize/deserialize
//!           asymmetry in the PoW input is exactly the shape an attacker
//!           uses to push a block one validator accepts and another rejects)
//!   Path 4: `zebra_consensus::block::check::equihash_solution_is_valid` —
//!           the call site every full-block verifier uses; same code path
//!           as Path 1 but goes through the consensus crate wrapper so a
//!           wrapper-only panic (e.g. error conversion) is also caught
//!   Path 5: cross-network re-check — same parsed header, same Solution,
//!           but we exercise the header re-serialize path that
//!           `Solution::check` uses internally on every call. Mismatch
//!           between two `check()` invocations on the same header is a
//!           non-determinism canary.
//!   Path 6: Solution::from_bytes standalone — the raw length-dispatch
//!           constructor (1344/36/else). Different surface from Path 2
//!           (which goes via CompactSize + zcash_deserialize_bytes_external_count
//!           length cap). Path 6 hits `from_bytes` with arbitrary lengths
//!           the caller picks (we slide a window over `data`), bypassing
//!           the CompactSize gate, so the mutator can probe the
//!           length-mismatch branch directly without first crafting a
//!           valid CompactSize prefix.
//!   Path 7: equihash::is_valid_solution multi-param — Zebra hardcodes
//!           (n=200, k=9) inside `Solution::check`, so the mutator never
//!           reaches the verifier internals (param/index decode, blake2b
//!           init, tree validator) within seconds of CPU on production
//!           params. We call the upstream `equihash::is_valid_solution`
//!           directly with small `(n=50, k=3)` and `(n=96, k=5)` presets
//!           (the latter is the upstream "all_bits_matter" test vector
//!           param) so Equihash internals execute in microseconds. Any
//!           panic inside `equihash 0.3` itself is caught here — the
//!           crate is no_std + alloc, vec!/Write panic surfaces are real
//!           DoS candidates if reachable from attacker bytes.
//!
//! Each consensus-critical call is wrapped in `std::panic::catch_unwind` so
//! the fuzz iteration does not abort on the first hit and downstream paths
//! still execute on the same input. libfuzzer still sees a process abort
//! when the unwind strategy escapes, so genuine panics surface as crash
//! artifacts under `zebra-fuzz/fuzz/artifacts/equihash_fuzz/`.
//!
//! Build (debug):
//!   cargo +nightly fuzz build equihash_fuzz
//!
//! Smoke run:
//!   cargo +nightly fuzz run equihash_fuzz \
//!       -- -max_total_time=900 -rss_limit_mb=4096
//!
//! Out of scope here:
//! - Multi-block / chain-context PoW checks (separate state-reorg fuzz target)
//! - Solver path (`Solution::solve`) — `internal-miner` feature only, and the
//!   solver is not consensus-critical (verifier is)
//! - Regtest-only Equihash parameters — `Solution::Regtest` (36-byte) is
//!   covered transparently via `Solution::from_bytes` length dispatch

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use std::panic;
use zebra_chain::block::Header;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::work::equihash::Solution;

fuzz_target!(|data: &[u8]| {
    // ─────────────────────────────────────────────────────────────────
    // Path 2 — standalone Solution deserialize (runs first, gate-free).
    //
    // Exercises `Solution::zcash_deserialize` which internally drives
    // the compact-size length prefix decode + `Solution::from_bytes`
    // length dispatch (1344 mainnet/testnet, 36 regtest, else error).
    // A panic here is a deserializer bug independent of any Header
    // context, and surfaces faster than Path 1 because it does not
    // require the input to also be a valid Header.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = Solution::zcash_deserialize(Cursor::new(data));
    }));

    // ─────────────────────────────────────────────────────────────────
    // Path 6 — Solution::from_bytes raw length-dispatch (gate-free).
    //
    // Bypasses the CompactSize length cap that Path 2's
    // `zcash_deserialize` enforces (it rejects lengths > SOLUTION_SIZE
    // before allocating). Path 6 calls `from_bytes` directly with the
    // caller-chosen slice length, exercising:
    //   - len == 1344 (Common variant copy_from_slice)
    //   - len == 36   (Regtest variant copy_from_slice)
    //   - any other length (Err return — must not panic on any len)
    // We probe three lengths: full input, len-1 (off-by-one), and a
    // slice picked by the first byte to densify the mutator's reach
    // toward the boundary lengths.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = Solution::from_bytes(data);
        if !data.is_empty() {
            let _ = Solution::from_bytes(&data[..data.len() - 1]);
            // Length picked from first byte modulated to span the
            // {0, 36, 1344, garbage} regions.
            let pick = match data[0] % 4 {
                0 => 0,
                1 => core::cmp::min(36, data.len()),
                2 => core::cmp::min(1344, data.len()),
                _ => core::cmp::min(data[0] as usize * 17, data.len()),
            };
            let _ = Solution::from_bytes(&data[..pick]);
        }
    }));

    // ─────────────────────────────────────────────────────────────────
    // Path 7 — upstream equihash::is_valid_solution multi-param.
    //
    // Zebra's `Solution::check` hardcodes (n=200, k=9) so reaching the
    // verifier internals (param decode, indices_from_minimal,
    // tree_validator recursion, blake2b init) within seconds of CPU
    // on production params is impractical — the 5min smoke baseline
    // (148K exec/s, corp +0, cov+ft 323/389) confirms.
    //
    // Small Equihash params let the verifier finish in microseconds:
    //   (n=50,  k=3) — minimum sane params, exercises the
    //                  collision_byte_length / hash_length math edge.
    //   (n=96,  k=5) — the upstream `all_bits_matter` test vector
    //                  param; soln length 68 bytes (vs Zcash 1344).
    //
    // Input/nonce/soln carved from `data` — the mutator can iterate the
    // whole tuple in a single iter, vs the current setup where 1344-byte
    // soln + 108-byte input must align before any verifier code runs.
    //
    // Wrapped in catch_unwind because `equihash 0.3` is no_std + alloc
    // and uses `vec!` / `Write::write_all().unwrap()` in
    // `initialise_state` — a malformed input that surfaces an alloc
    // failure or write panic is a real DoS candidate (the verifier is
    // called from `Solution::check` which is consensus-critical).
    // ─────────────────────────────────────────────────────────────────
    if data.len() >= 8 {
        // Carve roughly: first 32 bytes nonce-ish, next 32 bytes input-ish,
        // remainder soln. Lengths intentionally not aligned to either Zcash
        // or upstream test-vector exact lengths so the mutator probes
        // boundary handling (the verifier accepts any input length).
        let split_a = core::cmp::min(32, data.len() / 4);
        let split_b = core::cmp::min(split_a + 32, data.len() / 2);
        let nonce_bytes = &data[..split_a];
        let input_bytes = &data[split_a..split_b];
        let soln_bytes = &data[split_b..];

        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            // (50, 3) — minimum sane.
            let _ = equihash::is_valid_solution(50, 3, input_bytes, nonce_bytes, soln_bytes);
        }));
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            // (96, 5) — upstream test-vector params.
            let _ = equihash::is_valid_solution(96, 5, input_bytes, nonce_bytes, soln_bytes);
        }));
        // (200, 9) — Zcash mainnet params, but called raw (skipping the
        // header re-serialize step Solution::check does internally). This
        // is the third param so the mutator can compare timings/edges
        // across param families. We call it once per iter only when the
        // input is short enough to keep iter time bounded — the Zcash
        // soln length is 1344 so giving the verifier a trimmed slice
        // forces the early indices_from_minimal length-mismatch return,
        // which is itself a coverage edge worth hitting.
        if data.len() <= 256 {
            let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
                let _ = equihash::is_valid_solution(200, 9, input_bytes, nonce_bytes, soln_bytes);
            }));
        }
    }

    // ─────────────────────────────────────────────────────────────────
    // Gate: only proceed past Path 2 if Zebra's Header deserializer
    // accepts the bytes. Header is the wire-level surface — every P2P
    // inbound block routes through this exact call.
    // ─────────────────────────────────────────────────────────────────
    let header = match Header::zcash_deserialize(Cursor::new(data)) {
        Ok(h) => h,
        Err(_) => return,
    };

    // ─────────────────────────────────────────────────────────────────
    // Path 1 — `header.solution.check(&header)` (consensus-critical).
    //
    // This is the PoW gate every inbound block header must pass. It
    // re-serializes the header internally to extract the
    // `Solution::INPUT_LENGTH = 108` byte prefix used as Equihash
    // verification input. The `.expect("serialization into a vec
    // can't fail")` on header.zcash_serialize inside check() is one
    // of the panics we are hunting — any header that parses but fails
    // to re-serialize would trip it.
    //
    // The equihash::is_valid_solution call (n=200, k=9 for mainnet/
    // testnet) is a heavy crypto verification; it returns Err on
    // invalid solutions but must never panic on attacker bytes.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = header.solution.check(&header);
    }));

    // ─────────────────────────────────────────────────────────────────
    // Path 4 — consensus-crate wrapper.
    //
    // Same code path as Path 1 but goes through
    // `zebra_consensus::block::check::equihash_solution_is_valid`,
    // which is what the full-block verifier and checkpoint verifier
    // both call. A wrapper-only panic (e.g. error conversion / type
    // coercion) would surface here but not Path 1.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = zebra_consensus::block::check::equihash_solution_is_valid(&header);
    }));

    // ─────────────────────────────────────────────────────────────────
    // Path 3 — Header round-trip consensus-split detector.
    //
    // A header that survives parse → re-serialize → re-parse must
    // produce identical bytes and identical hash(). Any asymmetry is
    // a consensus-split candidate: an attacker can craft a header
    // where Zebra's deserialize-serialize round-trip flips a bit that
    // zcashd's does not (or vice versa), producing two validators
    // with differing block-hash views.
    //
    // The hash() comparison is the high-signal check: block hash is
    // SHA-256d over the full header serialization, so a single byte
    // of asymmetry anywhere in the header (including inside the
    // Solution variable-length encoding) flips the hash.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let mut buf = Vec::new();
        if header.zcash_serialize(&mut buf).is_err() {
            return;
        }
        let header2 = match Header::zcash_deserialize(Cursor::new(&buf)) {
            Ok(h) => h,
            Err(_) => return,
        };
        let mut buf2 = Vec::new();
        if header2.zcash_serialize(&mut buf2).is_err() {
            return;
        }

        // Byte-level round-trip equality — the canonical asymmetry
        // detector. Mismatch = consensus-split candidate.
        assert_eq!(
            buf, buf2,
            "Header round-trip byte mismatch — consensus-split vector"
        );

        // Hash equality — defence-in-depth. If buf == buf2 but
        // hash() differs, we have a hashing non-determinism
        // (different invocation = different output for the same
        // bytes), which is an even stronger finding.
        assert_eq!(
            header.hash(),
            header2.hash(),
            "Header hash non-determinism on identical round-tripped bytes"
        );

        // Solution-field equality — the PoW slot specifically. If
        // header bytes match but solution differs (would imply
        // PartialEq lies), it is a consensus-split candidate inside
        // the equihash Solution type itself.
        assert!(
            header.solution == header2.solution,
            "Solution PartialEq mismatch on round-tripped header — consensus-split vector"
        );
    }));

    // ─────────────────────────────────────────────────────────────────
    // Path 5 — verification non-determinism canary.
    //
    // `Solution::check()` re-serializes the header on every call and
    // feeds the first 108 bytes into `equihash::is_valid_solution`.
    // The verification result must be deterministic: running check()
    // twice on the same parsed header must return the same Result
    // variant (both Ok, or both Err). A divergence here would mean
    // the Equihash verifier itself is non-deterministic on identical
    // input — an existential consensus problem.
    //
    // We compare only the variant (Ok vs Err), not the error
    // payload, because equihash::Error wraps the upstream crate's
    // error which may not implement PartialEq.
    // ─────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let r1 = header.solution.check(&header).is_ok();
        let r2 = header.solution.check(&header).is_ok();
        assert_eq!(
            r1, r2,
            "Equihash verification non-determinism on identical header"
        );
    }));
});
