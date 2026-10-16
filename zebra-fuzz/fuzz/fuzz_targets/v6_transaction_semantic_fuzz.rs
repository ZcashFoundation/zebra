#![no_main]

//! `v6_transaction_semantic_fuzz` — the business-logic / consensus-structure
//! surface of the NU6.3 ("Ironwood") v6 transaction, complementing the parse-
//! only `v6_transaction_fuzz`.
//!
//! `v6_transaction_fuzz` fuzzes the wire (decode ↔ encode round-trip). This
//! target decodes a transaction and then *drives the parsed value through the
//! Ironwood business methods*: the `ironwood_*` accessors, the shielded-flag and
//! value-balance logic, and the transaction-structure consensus checks that gain
//! an Ironwood branch under NU6.3. None of these may panic on any
//! successfully-parsed transaction — a panic would let a peer abort a node with
//! a crafted-but-well-formed v6 transaction.
//!
//! Scope and safety. Every method called here is a pure `&Transaction` /
//! `&ShieldedData` function or a synchronous `fn(&Transaction) -> Result`
//! consensus check — no async, no proof verification, no FFI. It computes **no
//! transaction id / auth digest**: the txid builder's librustzcash-compat
//! `.expect()` is never on this path (and, per the v6 deserializer's own
//! `to_librustzcash` pre-validation, is unreachable on any parsed tx anyway).
//! The proof-verification and note-commitment-tree / history-tree surfaces need
//! block-level or async harnesses and are intentionally out of scope here.
//!
//! Oracle: (1) no panic across all driven methods; (2) two structural
//! invariants that must hold by construction — the presence predicate agrees
//! with the accessor, and the consensus flag-check agrees with the transaction
//! method it wraps.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use zebra_chain::parameters::NetworkUpgrade;
use zebra_chain::serialization::ZcashDeserialize;
use zebra_chain::transaction::Transaction;
use zebra_consensus::transaction::check;

fuzz_target!(|data: &[u8]| {
    // Only a successful decode carries invariants to assert; parse failure is the
    // expected outcome for most fuzzer inputs. Decoding must never panic.
    let tx = match Transaction::zcash_deserialize(Cursor::new(data)) {
        Ok(tx) => tx,
        Err(_) => return,
    };

    // The network upgrade the checks branch on. A v6 tx carries Nu6_3/Nu7; for
    // other versions fall back to Nu6_3 so the Ironwood branches are exercised.
    let network_upgrade = tx.network_upgrade().unwrap_or(NetworkUpgrade::Nu6_3);

    // ── Ironwood + v6 accessors (must not panic; iterators fully consumed) ──
    let has_ironwood = tx.has_ironwood_shielded_data();
    let ironwood_present = tx.ironwood_flags().is_some();
    let _ = tx.ironwood_actions().count();
    let _ = tx.ironwood_nullifiers().count();
    let _ = tx.ironwood_note_commitments().count();
    let method_enough_flags = tx.has_enough_ironwood_flags();
    let _ = tx.ironwood_value_balance();
    let _ = tx.orchard_value_balance();
    let _ = tx.orchard_flags();
    let _ = tx.has_shielded_inputs();
    let _ = tx.has_shielded_outputs();
    let _ = tx.version_group_id();

    // ── Structural / value-balance consensus checks (sync, txid-free) ──
    // Each returns Result; malformed inputs are *expected* to return Err. We
    // assert only that none panics — libFuzzer treats a panic/abort as a crash.
    let _ = check::has_enough_ironwood_flags(&tx);
    let _ = check::orchard_cross_address_disabled(&tx);
    let _ = check::orchard_value_balance_non_negative(&tx, network_upgrade);
    let _ = check::coinbase_orchard_component_empty(&tx, network_upgrade);
    let _ = check::coinbase_tx_no_prevout_joinsplit_spend(&tx);
    let _ = check::spend_conflicts(&tx);

    // ── Structural invariants that must hold by construction ──
    // The presence predicate must agree with the accessor.
    assert_eq!(
        has_ironwood, ironwood_present,
        "has_ironwood_shielded_data disagrees with ironwood_flags().is_some()"
    );
    // The consensus flag-check is a thin wrapper over the transaction method:
    // it returns Ok iff the tx has enough Ironwood flags. A divergence is a bug.
    assert_eq!(
        check::has_enough_ironwood_flags(&tx).is_ok(),
        method_enough_flags,
        "consensus has_enough_ironwood_flags disagrees with Transaction::has_enough_ironwood_flags"
    );
});
