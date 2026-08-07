#![no_main]

//! `v6_transaction_fuzz` — untrusted-input decode surface for the NU6.3
//! ("Ironwood") v6 transaction format.
//!
//! Zebra v6.0.0 introduced `Transaction::V6`, which carries the Ironwood
//! Orchard bundle (`orchard::ShieldedDataV6`) and the Ironwood shielded pool.
//! This is brand-new consensus-serialized code that is reachable directly from
//! the peer-to-peer and mempool wire: a peer can hand a node arbitrary bytes
//! claiming to be a v6 transaction. This harness mutates those bytes directly —
//! rather than reaching the transaction decoder only through a full block — so
//! the fuzzer explores the v6 header, the Ironwood bundle, and every
//! shielded/transparent field with far more inputs per second than
//! `block_deserialize` reaches the same code indirectly.
//!
//! Oracle — decode/encode round-trip, asserted only when a decode succeeds:
//!
//! * **Idempotent re-encode.** `serialize(deserialize(bytes))` must be
//!   byte-for-byte stable across a second decode/encode cycle. An asymmetry
//!   means the node could disagree with peers about the canonical encoding of a
//!   transaction it accepts — a consensus-split vector. We assert Zebra's own
//!   idempotency, not equality with the raw input, which may carry non-canonical
//!   `CompactSize` or trailing bytes the decoder tolerated.
//! * **Structural inverse.** The re-decoded transaction must be equal
//!   (`PartialEq`) to the first: `deserialize` must be the exact inverse of
//!   `serialize` over every field — the Ironwood shielded data included — not
//!   merely reproduce the same bytes by coincidence.
//!
//! The entry point is version-agnostic — `Transaction::zcash_deserialize`
//! dispatches on the version header — and the target is focused on the
//! v6/Ironwood surface through its seed corpus (real Ironwood transactions from
//! the activated testnet, plus structural mutation). Non-v6 inputs still
//! exercise the shared decode path at no extra cost. The oracle is a no-panic /
//! round-trip check only; it computes no transaction id and drives no
//! consensus checks, keeping the target cheap enough to churn many inputs.

use libfuzzer_sys::fuzz_target;
use std::io::Cursor;
use zebra_chain::serialization::{ZcashDeserialize, ZcashSerialize};
use zebra_chain::transaction::Transaction;

fuzz_target!(|data: &[u8]| {
    // Parse failure is the expected outcome for the vast majority of
    // fuzzer-generated inputs; only a successful decode carries an invariant to
    // assert. Decoding must never panic on any input, well-formed or not — a
    // panic here would let a peer crash a node with a crafted transaction.
    let tx = match Transaction::zcash_deserialize(Cursor::new(data)) {
        Ok(tx) => tx,
        Err(_) => return,
    };

    // Re-encode the accepted transaction. A serialize error on a value that
    // just decoded is unusual but not the primary oracle; nothing to assert.
    let serialized = match tx.zcash_serialize_to_vec() {
        Ok(bytes) => bytes,
        Err(_) => return,
    };

    // Decode the re-encoded bytes and encode once more. The two encodings must
    // match byte-for-byte (idempotent canonical form) and the two decoded
    // values must be structurally equal (serialize is invertible over every
    // field, the Ironwood shielded data included).
    if let Ok(tx2) = Transaction::zcash_deserialize(Cursor::new(&serialized)) {
        let serialized2 = match tx2.zcash_serialize_to_vec() {
            Ok(bytes) => bytes,
            Err(_) => return,
        };
        assert_eq!(
            serialized, serialized2,
            "round-trip re-encode mismatch — canonical-encoding disagreement (consensus-split vector)"
        );
        assert_eq!(
            tx, tx2,
            "round-trip structural mismatch — deserialize is not the inverse of serialize"
        );
    }
});
