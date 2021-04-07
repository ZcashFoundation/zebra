- Feature Name: `v5_transaction`
- Start Date: 2021-03-11
- Design PR: [ZcashFoundation/zebra#1886](https://github.com/ZcashFoundation/zebra/pull/1886)
- Zebra Issue: [ZcashFoundation/zebra#1863](https://github.com/ZcashFoundation/zebra/issues/1863)

# Summary
[summary]: #summary

Network Upgrade number 5 (`NU5`) introduces a new transaction type (transaction version 5). This document is a proposed design for implementing such a transaction version.

# Motivation
[motivation]: #motivation

The Zebra software wants to be a protocol compatible Zcash implementation. One of the tasks to do this includes the support of the new version 5 transactions that will be implemented in Network Upgrade 5 (NU5).

# Definitions
[definitions]: #definitions

- `NU5` - the 5th Zcash network upgrade, counting from the `Overwinter` upgrade as upgrade zero.
- `Orchard` - a new shielded pool introduced in `NU5`.
- `Sapling` - a new shielded pool introduced in the 1st network upgrade. (`Sapling` is also the name of that network upgrade, but this RFC is focused on the `Sapling` shielded pool.)
- `orchard data` - Data types needed to support orchard transactions.
- `sapling data` - Data types needed to support sapling transactions.
- `orchard transaction version` - Transactions that support orchard data. Currently only V5.
- `sapling transaction version` - Transactions that support sapling data. Currently V4 and V5 but the data is implemented differently in them.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

V5 transactions are described by the protocol in the second table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus).

All of the changes proposed in this document are only to the `zebra-chain` crate.

To highlight changes most of the document comments from the code snippets in the [reference section](#reference-level-explanation) were removed.

## Sapling Changes Overview
[sapling-changes-overview]: #sapling-changes-overview

V4 and V5 transactions both support sapling, but the underlying data structures are different. So need to make the sapling data types generic over the V4 and V5 structures.

In V4, anchors are per-spend, but in V5, they are per-transaction.

For consistency, also we move some fields into the `ShieldedData` type, and rename some fields and types.

## Orchard Additions Overview
[orchard-additions-overview]: #orchard-additions-overview

V5 transactions are the only ones that will support orchard transactions with `Orchard` data types.

Orchard uses `Halo2Proof`s with corresponding signature type changes. Each Orchard `Action` contains a spend and an output. Placeholder values are substituted for unused spends and outputs.

## Other Transaction V5 Changes
[other-transaction-v5-changes]: #other-transaction-v5-changes

The order of some of the fields changed from V4 to V5. For example the `lock_time` and `expiry_height` were moved above the transparent inputs and outputs.

Zebra enums and structs put fields in serialized order. Composite fields are ordered based on **last** data deserialized for each field.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Sapling Changes
[sapling-changes]: #sapling-changes

We know by protocol (2nd table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)) that V5 transactions will support sapling data however we also know by protocol that spends ([Spend Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#spendencodingandconsensus), See †) and outputs ([Output Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#outputencodingandconsensus), See †) fields change from V4 to V5.

`ShieldedData` is currently defined and implemented in `zebra-chain/src/transaction/shielded_data.rs`. As this is Sapling specific we propose to move this file to `zebra-chain/src/sapling/shielded_data.rs`.

### Changes to V4 Transactions
[changes-to-v4-transactions]: #changes-to-v4-transactions

Here we have the proposed changes for V4 transactions:
* make `sapling_shielded_data` use the `PerSpendAnchor` anchor variant
* rename `shielded_data` to `sapling_shielded_data`
* move `value_balance` into the `sapling::ShieldedData` type
* order fields based on the **last** data deserialized for each field

```rust
enum Transaction::V4 {
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    lock_time: LockTime,
    expiry_height: block::Height,
    joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    sapling_shielded_data: Option<sapling::ShieldedData<PerSpendAnchor>>,
}
```

### Anchor Variants
[anchor-variants]: #anchor-variants

We add an `AnchorVariant` generic type trait, because V4 transactions have a per-`Spend` anchor, but V5 transactions have a shared anchor. This trait can be added to `sapling/shielded_data.rs`:

```rust
struct PerSpendAnchor {}
struct SharedAnchor {}

/// This field is not present in this transaction version.
struct FieldNotPresent;

impl AnchorVariant for PerSpendAnchor {
    type Shared = FieldNotPresent;
    type PerSpend = tree::Root;
}

impl AnchorVariant for SharedAnchor {
    type Shared = tree::Root;
    type PerSpend = FieldNotPresent;
}

trait AnchorVariant {
    type Shared;
    type PerSpend;
}
```

### Changes to Sapling ShieldedData
[changes-to-sapling-shieldeddata]: #changes-to-sapling-shieldeddata

We use `AnchorVariant` in `ShieldedData` to model the anchor differences between V4 and V5:

```rust
struct sapling::ShieldedData<AnchorV: AnchorVariant> {
    value_balance: Amount,
    shared_anchor: AnchorV::Shared,
    first: Either<Spend<AnchorV>, Output>,
    rest_spends: Vec<Spend<AnchorV>>,
    rest_outputs: Vec<Output>,
    binding_sig: Signature<Binding>,
}
```

### Adding V5 Sapling Spend
[adding-v5-sapling-spend]: #adding-v5-sapling-spend

Sapling spend code is located at `zebra-chain/src/sapling/spend.rs`. We use `AnchorVariant` to model the anchor differences between V4 and V5:

```rust
struct Spend<AnchorV: AnchorVariant> {
    cv: commitment::ValueCommitment,
    per_spend_anchor: AnchorV::PerSpend,
    nullifier: note::Nullifier,
    rk: redjubjub::VerificationKeyBytes<SpendAuth>,
    zkproof: Groth16Proof,
    spend_auth_sig: redjubjub::Signature<SpendAuth>,
}
```

### No Changes to Sapling Output
[no-changes-to-sapling-output]: #no-changes-to-sapling-output

In Zcash the Sapling output representations are the same for V4 and V5 transactions, so no variants are needed. The output code is located at `zebra-chain/src/sapling/output.rs`:

```rust
struct Output {
    cv: commitment::ValueCommitment,
    cm_u: jubjub::Fq,
    ephemeral_key: keys::EphemeralPublicKey,
    enc_ciphertext: note::EncryptedNote,
    out_ciphertext: note::WrappedNoteKey,
    zkproof: Groth16Proof,
}
```

## Orchard Additions
[orchard-additions]: #orchard-additions

### Adding V5 Transactions
[adding-v5-transactions]: #adding-v5-transactions

Now lets see how the V5 transaction is specified in the protocol, this is the second table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus) and how are we going to represent it based in the above changes for Sapling fields and the new Orchard fields.

We propose the following representation for transaction V5 in Zebra:

```rust
enum Transaction::V5 {
    lock_time: LockTime,
    expiry_height: block::Height,
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    sapling_shielded_data: Option<sapling::ShieldedData<SharedAnchor>>,
    orchard_shielded_data: Option<orchard::ShieldedData>,
}
```

To model the V5 anchor type, `sapling_shielded_data` uses the `SharedAnchor` variant located at `zebra-chain/src/transaction/sapling/shielded_data.rs`.

### Adding Orchard ShieldedData
[adding-orchard-shieldeddata]: #adding-orchard-shieldeddata

The new V5 structure will create a new `orchard::ShieldedData` type. This new type will be defined in a new `zebra-chain/src/orchard/shielded_data.rs` file:

```rust
struct orchard::ShieldedData {
    flags: Flags,
    value_balance: Amount,
    shared_anchor: tree::Root,
    proof: Halo2Proof,
    /// An authorized action description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no actions.
    first: AuthorizedAction,
    rest: Vec<AuthorizedAction>,
    binding_sig: redpallas::Signature<redpallas::Binding>,
}
```

The fields are ordered based on the **last** data deserialized for each field.

### Adding Orchard AuthorizedAction
[adding-orchard-authorizedaction]: #adding-orchard-authorizedaction

In `V5` transactions, there is one `SpendAuth` signature for every `Action`. To ensure that this structural rule is followed, we create an `AuthorizedAction` type in `orchard/shielded_data.rs`:

```rust
/// An authorized action description.
///
/// Every authorized Orchard `Action` must have a corresponding `SpendAuth` signature.
struct orchard::AuthorizedAction {
    action: Action,
    spend_auth_sig: redpallas::Signature<redpallas::SpendAuth>,
}
```

Where `Action` is defined as [Action definition](https://github.com/ZcashFoundation/zebra/blob/68c12d045b63ed49dd1963dd2dc22eb991f3998c/zebra-chain/src/orchard/action.rs#L18-L41).

### Adding Orchard Flags
[adding-orchard-flags]: #adding-orchard-flags

Finally, in the V5 transaction we have a new `orchard::Flags` type. This is a bitfield type defined as:

```rust
bitflags! {
    /// Per-Transaction flags for Orchard.
    ///
    /// The spend and output flags are passed to the `Halo2Proof` verifier, which verifies
    /// the relevant note spending and creation consensus rules.
    struct orchard::Flags: u8 {
        /// Enable spending non-zero valued Orchard notes.
        ///
        /// "the `enableSpendsOrchard` flag, if present, MUST be 0 for coinbase transactions"
        const ENABLE_SPENDS = 0b00000001;
        /// Enable creating new non-zero valued Orchard notes.
        const ENABLE_OUTPUTS = 0b00000010;
        // Reserved, zeros (bits 2 .. 7)
    }
}
```

This type is also defined in `orchard/shielded_data.rs`.

## Test Plan
[test-plan]: #test-plan

- All renamed, modified and new types should serialize and deserialize.
- The full V4 and V5 transactions should serialize and deserialize.
- Prop test strategies for V4 and V5 will be updated and created.
- Before NU5 activation on testnet, test on the following test vectors:
  - Hand-crafted Orchard-only, Orchard/Sapling, Orchard/Transparent, and Orchard/Sapling/Transparent transactions based on the spec
  - "Fake" Sapling-only and Sapling/Transparent transactions based on the existing test vectors, converted from V4 to V5 format
    - We can write a test utility function to automatically do these conversions
  - An empty transaction, with no Orchard, Sapling, or Transparent data
  - Any available `zcashd` test vectors
- After NU5 activation on testnet:
  - Add test vectors using the testnet activation block and 2 more post-activation blocks
- After NU5 activation on mainnet:
  - Add test vectors using the mainnet activation block and 2 more post-activation blocks

# Security

To avoid parsing memory exhaustion attacks, we will make the following changes across all `Transaction`, `ShieldedData`, `Spend` and `Output` variants, V1 through to V5:
- Check cardinality consensus rules at parse time, before deserializing any `Vec`s
  - In general, Zcash requires that each transaction has at least one Transparent/Sprout/Sapling/Orchard transfer, this rule is not currently encoded in our data structures (it is only checked during semantic verification)
- Stop parsing as soon as the first error is detected

These changes should be made in a later pull request, see [#1917](https://github.com/ZcashFoundation/zebra/issues/1917) for details.
