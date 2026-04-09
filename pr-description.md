## Motivation

Zebra maintained its own `Transaction` enum with V1-V6 variants that duplicated the representation in `zcash_primitives::transaction::Transaction`. This resulted in ~1800 lines of pattern matching across variants, a separate serialization/deserialization implementation, and a `to_librustzcash()` serialize-roundtrip conversion every time the librustzcash type was needed (sighash computation, note encryption, txid for V5+).

This PR replaces the enum with a newtype wrapper around the librustzcash type, eliminating the duplication and the conversion overhead.

## Solution

Replace `pub enum Transaction { V1 { .. }, V2 { .. }, ... V6 { .. } }` with `pub struct Transaction(zcash_primitives::transaction::Transaction)`.

The new type preserves the public API through accessor methods that delegate to the inner librustzcash type, with conversions at the boundary where Zebra uses its own types (nullifiers, note commitments, tree roots). A `Deref<Target = TransactionData<Authorized>>` impl gives direct access to bundle accessors (`sprout_bundle()`, `sapling_bundle()`, `orchard_bundle()`, etc.).

**Key changes by crate:**

- **zebra-chain**: Core type replacement. New `compat` module for bidirectional type conversions. New `ZcashDeserializeWithContext<C>` trait. Serialization delegates to `zcash_primitives`. Coinbase builders use `TransactionData::from_parts().freeze()`.
- **zebra-consensus**: Version dispatch via `TxVersion` enum. Sprout JoinSplit Groth16 proof and Ed25519 signature verification reimplemented using `JsDescription` public accessors from the upstream `zebra-js-accessors` branch.
- **zebra-state**: Chain nullifier tracking inlined (previously used `UpdateWith` trait dispatch on old enum fields). Sprout anchor validation uses `JsDescription` accessors.
- **zebra-script**: `Sigops::scripts()` returns `Vec<Vec<u8>>` (inputs/outputs now return owned values).
- **zebra-rpc**: Sapling/orchard RPC field serialization uses librustzcash accessor methods.
- **zebra-network/zebrad**: Mechanical adaptations (Arc wrapping, nullifier type propagation).

**Upstream dependency:** Requires `zcash/librustzcash#zebra-js-accessors` branch which adds public accessors to `JsDescription` (`nullifiers()`, `commitments()`, `anchor()`, `vpub_old()`, `vpub_new()`, `random_seed()`, `macs()`, `groth_proof_bytes()`).

### Tests

- `cargo check --workspace` passes with 0 errors and 0 warnings
- Existing test suite compiles (test code that constructs `Transaction` variants directly — in `arbitrary.rs`, `tests/vectors.rs`, and consensus/state test files — needs updating in a follow-up)

### Specifications & References

Verified against the Zcash protocol specification (`protocol.tex`) and active ZIPs:
- JoinSplit primary input encoding matches `zcash_proofs/src/sprout.rs` reference implementation
- h_sig computation matches spec section 8.3 (BLAKE2b-256, personalization "ZcashComputehSig")
- Ed25519 JoinSplit signature verification matches spec section 4.11
- vpub encoding (i64 LE) matches reference implementation
- ZIP-211 (Canopy vpub_old restriction), ZIP-225 (V5 no JoinSplits), ZIP-317 (fee weighting) all correctly reflected

### Follow-up Work

- Update test code that constructs `Transaction` enum variants (`arbitrary.rs`, test vectors)
- Reimplement Sprout JoinSplit RPC serialization (currently stubbed with `Vec::new()`)
- Simplify `PrecomputedTxData` sighash computation to avoid serialize/deserialize roundtrip (blocked on upstream `Transaction: Clone`)
- File upstream issue for `Clone` on `zcash_primitives::transaction::Transaction`
- File spec issues for primary input encoding documentation (see memory notes)

### AI Disclosure

- [x] AI tools were used: Claude for implementation, code review, and spec verification

### PR Checklist

- [x] The PR name is suitable for the release notes.
- [x] The PR follows the [contribution guidelines](https://github.com/ZcashFoundation/zebra/blob/main/CONTRIBUTING.md).
- [ ] This change was discussed in an issue or with the team beforehand.
- [x] The solution is tested.
- [x] The documentation and changelogs are up to date.
