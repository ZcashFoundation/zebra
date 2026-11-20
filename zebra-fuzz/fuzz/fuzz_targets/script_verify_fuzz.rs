#![no_main]

//! Script Verify Fuzz Target (transparent script consensus path)
//!
//! Targets `zebra_script::CachedFfiTransaction::is_valid`, the script
//! verification entrypoint, as a high-value fuzz surface because:
//!
//! - It runs the Rust `zcash_script` interpreter on scripts gossiped
//!   from the network. A panic, OOM, or unbounded loop in the
//!   interpreter, in Zebra's sighash callback, or in Zebra's sigop
//!   counter is a remote-DoS primitive for a node validating
//!   transparent input scripts.
//! - It drives the interpreter with deserialized transactions, so
//!   signature checks reach Zebra's sighash computation.
//!
//! `zebra-script` no longer calls zcashd's C++ interpreter. That
//! interpreter is still fuzzed directly, and compared with Zebra's
//! sigop counter, in `script_flag_matrix_fuzz`.
//!
//! We wrap each invariant in `catch_unwind` so that a panic on one
//! invariant (e.g. an `as` cast that traps in debug, or a
//! `.expect(...)` in the sighash callback) does not lose the rest of
//! the iteration's coverage on the same input.
//!
//! ───────────────────────────────────────────────────────────────────
//! Mock state design (kept minimal on purpose)
//! ───────────────────────────────────────────────────────────────────
//!
//! Script verification needs the previous-output that each input is
//! spending — under normal validation Zebra reads this from the
//! UTXO state. The fuzzer obviously has no UTXO database, so we
//! mock the previous outputs in the simplest way that still drives
//! the interpreter:
//!
//! - One `transparent::Output` per `tx.inputs()` slot.
//! - Each mock output has `value = 1 zatoshi` (any non-zero NonNegative
//!   `Amount` works — value is consensus-relevant for fee checks
//!   higher up the stack but **not** for script evaluation, which
//!   only consumes the lock_script bytes).
//! - Each mock output's `lock_script` is fed from fuzz-controlled
//!   bytes (the `script_bytes` slice below). This is the actual
//!   attack input: malformed Bitcoin Script
//!   bytecode in the prev-output `script_pub_key`.
//!
//! We deliberately do **not** build "valid-looking" mock prevouts.
//! Complex mock state masks real bugs (mock pollutes the input
//! distribution); minimal mock keeps the fuzz signal sharp on the
//! interpreter's response to garbage bytes.
//!
//! ───────────────────────────────────────────────────────────────────
//! Input split layout
//! ───────────────────────────────────────────────────────────────────
//!
//! Fuzz bytes are partitioned into four regions, recovered from the
//! tail of `data` so the more "structured" fields (selectors) are
//! stable across small mutations of the head:
//!
//! ```text
//!   [ tx_bytes ............................. | script_bytes ... | nu_sel | idx_sel ]
//!   ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
//!   variable-length head (Transaction::zcash_deserialize)
//!                                              ^^^^^^^^^^^^^^^^^^^^
//!                                              variable-length middle
//!                                              (lock_script bytes)
//!                                                                   ^^^^^^   ^^^^^^^
//!                                                                   1 byte   1 byte
//! ```
//!
//! `idx_sel` chooses which input's `is_valid` we exercise (mod
//! `tx.inputs().len()`); `nu_sel` chooses one of a small set of
//! reasonable `NetworkUpgrade` variants so the sighasher constructor
//! has a chance of succeeding for V4 (Blossom..Canopy) and V5+
//! (Nu5..). We do not dimension-explode further — libfuzzer's
//! coverage feedback will steer the corpus through the variants
//! that reach new code automatically.
//!
//! Compile (debug build):
//!
//! ```text
//! cargo +nightly fuzz build script_verify_fuzz
//! ```
//!
//! ───────────────────────────────────────────────────────────────────
//! Panic / crash classes of interest for this target
//! ───────────────────────────────────────────────────────────────────
//!
//! 1. **Interpreter panic** — malformed `OP_PUSHDATA*` length
//!    prefixes or stack underflow on `OP_CHECKSIG` could index out of
//!    bounds in the interpreter.
//! 2. **Integer overflow in OP_* dispatch** — an attacker-controlled
//!    arithmetic op on script numbers could overflow, which panics in
//!    a debug build and wraps silently in a release build.
//! 3. **Sigop counter panic** — `zebra_script::legacy_sigop_count`
//!    walks raw script bytes; a slip in its push-length handling
//!    could slice past the end of the script.
//! 4. **Sighasher Rust-panic** — `CachedFfiTransaction::new` calls
//!    `transaction.sighasher(nu, all_previous_outputs)`, which
//!    routes through `librustzcash` for V5+ and through
//!    `zebra-chain`'s own implementation for V4. Either path can
//!    `.expect(...)` on a malformed bundle layout that survived
//!    deserialization (cross-ref `librustzcash_tx_read_fuzz` —
//!    we are the **post-construction** side of the same surface).

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use std::panic;
use std::sync::Arc;

use zebra_chain::parameters::NetworkUpgrade;
use zebra_chain::serialization::ZcashDeserialize;
use zebra_chain::transaction::Transaction;
use zebra_chain::transparent;
use zebra_script::CachedFfiTransaction;

/// Reasonable NetworkUpgrade choices for sighasher construction.
///
/// `CachedFfiTransaction::new` calls `transaction.sighasher(nu, ...)`
/// which only succeeds when the (version, NU) pair is consistent.
/// We list the most-traffic ones; the selector index is `nu_sel %
/// NU_CHOICES.len()`. Picking an inconsistent NU just produces an
/// `Error::TxInvalid` at construction time — that is a graceful
/// failure path, not a fuzz signal, so we silently skip it.
const NU_CHOICES: &[NetworkUpgrade] = &[
    NetworkUpgrade::Sapling,
    NetworkUpgrade::Blossom,
    NetworkUpgrade::Heartwood,
    NetworkUpgrade::Canopy,
    NetworkUpgrade::Nu5,
    NetworkUpgrade::Nu6,
];

/// Minimum fuzz input length: 1-byte tx + 1 lock_script byte +
/// 2 selector bytes = 4 bytes. Any shorter input cannot drive
/// `is_valid` and we early-return.
const MIN_INPUT_LEN: usize = 4;

fuzz_target!(|data: &[u8]| {
    // ───────────────────────────────────────────────────────────────────
    // Layer 0: Input split.
    //
    // We need at least MIN_INPUT_LEN bytes to recover the two trailing
    // selectors and leave room for tx + script. Below that, abort.
    // ───────────────────────────────────────────────────────────────────
    if data.len() < MIN_INPUT_LEN {
        return;
    }

    // Pull the two trailing selector bytes from the tail (stable under
    // libfuzzer mutations of the head).
    let idx_sel = data[data.len() - 1];
    let nu_sel = data[data.len() - 2];
    let head = &data[..data.len() - 2];

    // Split the head into tx_bytes and script_bytes. We give the tx
    // the first 75% (heuristic — most tx serializations dominate in
    // size) and reserve the last 25% for script bytecode. If the head
    // is too short for any sensible split, give the script at least
    // 1 byte.
    let split_at = (head.len() * 3 / 4)
        .max(1)
        .min(head.len().saturating_sub(1));
    let tx_bytes = &head[..split_at];
    let script_bytes = &head[split_at..];

    // ───────────────────────────────────────────────────────────────────
    // Layer 1: Transaction::zcash_deserialize gate.
    //
    // We only proceed for structurally-valid Zebra transactions —
    // deserializer panics here are already covered by
    // `transaction_deep_fuzz` and `transaction_deserialize`. This
    // target's job starts **after** the wire decode succeeds.
    // ───────────────────────────────────────────────────────────────────
    let tx = match Transaction::zcash_deserialize(Cursor::new(tx_bytes)) {
        Ok(tx) => Arc::new(tx),
        Err(_) => return,
    };

    // Coinbase txs are explicitly rejected by `CachedFfiTransaction::is_valid`
    // (returns `Error::TxCoinbase`). Skipping them keeps the fuzz signal
    // focused on the path that actually reaches the interpreter.
    if tx.is_coinbase() {
        return;
    }

    let inputs_len = tx.inputs().len();
    if inputs_len == 0 {
        // No transparent inputs ⇒ no script to evaluate.
        // Sapling/Orchard-only V5 txs land here; their fuzz coverage
        // belongs to other targets.
        return;
    }

    // ───────────────────────────────────────────────────────────────────
    // Layer 2: Build minimal mock previous-outputs.
    //
    // One `transparent::Output` per input slot, with the lock_script
    // bytes fed straight from `script_bytes`. This is the actual
    // adversarial payload that the interpreter runs as `script_pub_key`.
    // Value is a sentinel 1 zatoshi — irrelevant to script evaluation.
    // ───────────────────────────────────────────────────────────────────
    let lock_script = transparent::Script::new(script_bytes);
    let mock_value: zebra_chain::amount::Amount<zebra_chain::amount::NonNegative> =
        match 1u64.try_into() {
            Ok(v) => v,
            // Should never fail (1 is in [0, MAX_MONEY]) but defend
            // against future Amount API changes that could re-shape
            // the conversion.
            Err(_) => return,
        };

    let prev_outputs: Vec<transparent::Output> = (0..inputs_len)
        .map(|_| transparent::Output {
            value: mock_value,
            lock_script: lock_script.clone(),
        })
        .collect();
    let prev_outputs = Arc::new(prev_outputs);

    // ───────────────────────────────────────────────────────────────────
    // Layer 3: NetworkUpgrade selection.
    //
    // Index into NU_CHOICES via the selector byte. We additionally try
    // the tx's own declared `network_upgrade()` if present (V5+) — this
    // is the most likely candidate to satisfy the sighasher's (version,
    // NU) consistency check, so we always probe it first.
    // ───────────────────────────────────────────────────────────────────
    let nu_from_tx = tx.network_upgrade();
    let nu_from_sel = NU_CHOICES[(nu_sel as usize) % NU_CHOICES.len()];

    // Try the tx's own declared NU first when present (most likely to
    // satisfy the sighasher's (version, NU) consistency check), then
    // also try the selector-chosen NU when it differs — libfuzzer
    // coverage feedback steers exploration through whichever variants
    // reach new code.
    let mut nu_candidates: Vec<NetworkUpgrade> = Vec::with_capacity(2);
    if let Some(nu) = nu_from_tx {
        nu_candidates.push(nu);
    }
    if !nu_candidates.contains(&nu_from_sel) {
        nu_candidates.push(nu_from_sel);
    }

    for nu_try in nu_candidates {
        drive_is_valid(&tx, prev_outputs.clone(), nu_try, idx_sel, inputs_len);
    }
});

/// Drive a single (tx, prev_outputs, NU, input_index) tuple through
/// `CachedFfiTransaction::new` + `is_valid`.
///
/// Both calls are wrapped in `panic::catch_unwind` so a Rust-side
/// panic on one tuple does not abort the iteration — libfuzzer can
/// still observe coverage on the remaining tuples for the same input.
fn drive_is_valid(
    tx: &Arc<Transaction>,
    prev_outputs: Arc<Vec<transparent::Output>>,
    nu: NetworkUpgrade,
    idx_sel: u8,
    inputs_len: usize,
) {
    // ───────────────────────────────────────────────────────────────────
    // Invariant S-1: CachedFfiTransaction::new must not Rust-panic.
    //
    // Constructor failure (returns Err) is the expected graceful path
    // for inconsistent (version, NU) pairs or malformed bundles — that
    // is **not** a fuzz signal. A panic here would indicate a missed
    // error-handling case in `transaction.sighasher`.
    // ───────────────────────────────────────────────────────────────────
    let cached = match panic::catch_unwind(panic::AssertUnwindSafe(|| {
        CachedFfiTransaction::new(tx.clone(), prev_outputs, nu)
    })) {
        Ok(Ok(c)) => c,
        // Constructor returned Err — graceful failure, skip.
        Ok(Err(_)) => return,
        // Constructor panicked — libfuzzer already records the crash.
        Err(_) => return,
    };

    // Pick the input index modulo the number of inputs. We also test
    // an out-of-bounds index to exercise the `Error::TxIndex` path,
    // because the bounds check is the last gate before the input is
    // indexed and a future refactor could regress it.
    let in_bounds = (idx_sel as usize) % inputs_len;
    let oob = inputs_len; // exactly one past the end

    // ───────────────────────────────────────────────────────────────────
    // Invariant S-2: is_valid(in_bounds) must not Rust-panic.
    //
    // This is the **primary** surface. Inside `is_valid`:
    //   - We read the lock_script from the mock prev_output.
    //   - We construct `zcash_script::interpreter::Flags::P2SH |
    //     CHECKLOCKTIMEVERIFY`.
    //   - We unwrap the input's `unlock_script` (rejects Coinbase ⇒
    //     returns Error::TxCoinbase, so coinbase txs are pre-filtered
    //     above).
    //   - We evaluate the script with `script::Raw::eval(flags, &checker)`,
    //     the Rust `zcash_script` interpreter.
    //
    // A panic, OOM, or unbounded loop in the interpreter or the sighash
    // callback is the highest-severity crash class for this surface.
    // ───────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _ = cached.is_valid(in_bounds);
    }));

    // ───────────────────────────────────────────────────────────────────
    // Invariant S-3: is_valid(out_of_bounds) returns Err, never panics.
    //
    // Defensive: confirms the explicit index check at lib.rs:134
    // (`self.all_previous_outputs.get(input_index).ok_or(Error::TxIndex)?`)
    // is the only gate. A panic here would mean the OOB check is being
    // bypassed somewhere — unlikely today, but cheap to assert.
    // ───────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let res = cached.is_valid(oob);
        // We only assert the shape (must be Err) when the call returned
        // — a Rust panic is caught above and is itself the finding.
        // `assert!`, not `debug_assert!`: `cargo fuzz build -O` builds in
        // release and this workspace declares no `[profile.release]`, so a
        // `debug_assert!` is compiled out and this target would ship with no
        // oracle at all.
        assert!(
            res.is_err(),
            "S-3 violated: out-of-bounds is_valid returned Ok"
        );
    }));

    // ───────────────────────────────────────────────────────────────────
    // Invariant S-4: Sigops::sigops() must not Rust-panic.
    //
    // `Sigops::sigops` walks the raw bytes of every input and output
    // script with `zebra_script::legacy_sigop_count`. The scripts are
    // attacker-controlled, so a slip in push-length handling would
    // panic here. `script_flag_matrix_fuzz` compares the counter with
    // zcashd's C++ `GetSigOpCount`.
    // ───────────────────────────────────────────────────────────────────
    let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        use zebra_script::Sigops;
        let _ = cached.sigops();
    }));
}
