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

V4 and V5 transactions both support sapling, but the underlying data structures are different. So we need to make the sapling data types generic over the V4 and V5 structures.

In V4, anchors are per-spend, but in V5, they are per-transaction. In V5, the shared anchor is only present if there is at least one spend.

For consistency, we also move some fields into the `ShieldedData` type, and rename some fields and types.

## Orchard Additions Overview

[orchard-additions-overview]: #orchard-additions-overview

V5 transactions are the only ones that will support orchard transactions with `Orchard` data types.

Orchard uses `Halo2Proof`s with corresponding signature type changes. Each Orchard `Action` contains a spend and an output. Placeholder values are substituted for unused spends and outputs.

## Other Transaction V5 Changes

[other-transaction-v5-changes]: #other-transaction-v5-changes

V5 transactions split `Spend`s, `Output`s, and `AuthorizedAction`s into multiple arrays,
with a single `CompactSize` count before the first array. We add new
`zcash_deserialize_external_count` and `zcash_serialize_external_count` utility functions,
which make it easier to serialize and deserialize these arrays correctly.

The order of some of the fields changed from V4 to V5. For example the `lock_time` and
`expiry_height` were moved above the transparent inputs and outputs.

The serialized field order and field splits are in [the V5 transaction section in the NU5 spec](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus).
(Currently, the V5 spec is on a separate page after the V1-V4 specs.)

Zebra's structs sometimes use a different order from the spec.
We combine fields that occur together, to make it impossible to represent structurally
invalid Zcash data.

In general:

- Zebra enums and structs put fields in serialized order.
- Composite structs and emnum variants are ordered based on **last** data
  deserialized for the composite.

# Reference-level explanation

[reference-level-explanation]: #reference-level-explanation

## Sapling Changes

[sapling-changes]: #sapling-changes

We know by protocol (2nd table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)) that V5 transactions will support sapling data however we also know by protocol that spends ([Spend Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#spendencodingandconsensus), See †) and outputs ([Output Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#outputencodingandconsensus), See †) fields change from V4 to V5.

`ShieldedData` is currently defined and implemented in `zebra-chain/src/transaction/shielded_data.rs`. As this is Sapling specific we propose to move this file to `zebra-chain/src/sapling/shielded_data.rs`.

### Changes to V4 Transactions

[changes-to-v4-transactions]: #changes-to-v4-transactions

Here we have the proposed changes for V4 transactions:

- make `sapling_shielded_data` use the `PerSpendAnchor` anchor variant
- rename `shielded_data` to `sapling_shielded_data`
- move `value_balance` into the `sapling::ShieldedData` type
- order fields based on the **last** data deserialized for each field

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

The following types have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `transparent::Input`
- `transparent::Output`
- `LockTime`
- `block::Height`
- `Option<JoinSplitData<Groth16Proof>>`

Note: `Option<sapling::ShieldedData<PerSpendAnchor>>` does not have serialize or deserialize implementations,
because the binding signature is after the joinsplits. Its serialization and deserialization is handled as
part of `Transaction::V4`.

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
    type PerSpend = sapling::tree::Root;
}

impl AnchorVariant for SharedAnchor {
    type Shared = sapling::tree::Root;
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

- in V4, there is a per-spend anchor
- in V5, there is a shared anchor, which is only present when there are spends

If there are no spends and no outputs:

- in v4, the value_balance is fixed to zero
- in v5, the value balance field is not present
- in both versions, the binding_sig field is not present

```rust
/// ShieldedData ensures that value_balance and binding_sig are only present when
/// there is at least one spend or output.
struct sapling::ShieldedData<AnchorV: AnchorVariant> {
    value_balance: Amount,
    transfers: sapling::TransferData<AnchorV>,
    binding_sig: redjubjub::Signature<Binding>,
}

/// TransferData ensures that:
/// * there is at least one spend or output, and
/// * the shared anchor is only present when there are spends
enum sapling::TransferData<AnchorV: AnchorVariant> {
    /// In Transaction::V5, if there are any spends,
    /// there must also be a shared spend anchor.
    SpendsAndMaybeOutputs {
        shared_anchor: AnchorV::Shared,
        spends: AtLeastOne<Spend<AnchorV>>,
        maybe_outputs: Vec<Output>,
    }

    /// If there are no spends, there must not be a shared
    /// anchor.
    JustOutputs {
        outputs: AtLeastOne<Output>,
    }
}
```

The `AtLeastOne` type is a vector wrapper which always contains at least one
element. For more details, see [its documentation](https://github.com/ZcashFoundation/zebra/blob/673b95dea5f0b057c11f2f450943b012fec75c00/zebra-chain/src/serialization/constraint.rs).

<!-- TODO: update link to main branch when PR #2021 merges -->

Some of these fields are in a different order to the serialized data, see
[the V4 and V5 transaction specs](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)
for details.

The following types have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `Amount`
- `sapling::tree::Root`
- `redjubjub::Signature<Binding>`

### Adding V5 Sapling Spend

[adding-v5-sapling-spend]: #adding-v5-sapling-spend

Sapling spend code is located at `zebra-chain/src/sapling/spend.rs`.
We use `AnchorVariant` to model the anchor differences between V4 and V5.
And we create a struct for serializing V5 transaction spends:

```rust
struct Spend<AnchorV: AnchorVariant> {
    cv: commitment::ValueCommitment,
    per_spend_anchor: AnchorV::PerSpend,
    nullifier: note::Nullifier,
    rk: redjubjub::VerificationKeyBytes<SpendAuth>,
    // This field is stored in a separate array in v5 transactions, see:
    // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
    // parse using `zcash_deserialize_external_count` and `zcash_serialize_external_count`
    zkproof: Groth16Proof,
    // This fields is stored in another separate array in v5 transactions
    spend_auth_sig: redjubjub::Signature<SpendAuth>,
}

/// The serialization prefix fields of a `Spend` in Transaction V5.
///
/// In `V5` transactions, spends are split into multiple arrays, so the prefix,
/// proof, and signature must be serialised and deserialized separately.
///
/// Serialized as `SpendDescriptionV5` in [protocol specification §7.3].
struct SpendPrefixInTransactionV5 {
    cv: commitment::ValueCommitment,
    nullifier: note::Nullifier,
    rk: redjubjub::VerificationKeyBytes<SpendAuth>,
}
```

The following types have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `Spend<PerSpendAnchor>` (moved from the pre-RFC `Spend`)
- `SpendPrefixInTransactionV5` (new)
- `Groth16Proof`
- `redjubjub::Signature<redjubjub::SpendAuth>` (new - for v5 spend auth sig arrays)

Note: `Spend<SharedAnchor>` does not have serialize and deserialize implementations.
It must be split using `into_v5_parts` before serialization, and
recombined using `from_v5_parts` after deserialization.

These convenience methods convert between `Spend<SharedAnchor>` and its v5 parts:
`SpendPrefixInTransactionV5`, the spend proof, and the spend auth signature.

### Changes to Sapling Output

[changes-to-sapling-output]: #changes-to-sapling-output

In Zcash the Sapling output fields are the same for V4 and V5 transactions,
so the `Output` struct is unchanged. However, V4 and V5 transactions serialize
outputs differently, so we create additional structs for serializing outputs in
each transaction version.

The output code is located at `zebra-chain/src/sapling/output.rs`:

```rust
struct Output {
    cv: commitment::ValueCommitment,
    cm_u: jubjub::Fq,
    ephemeral_key: keys::EphemeralPublicKey,
    enc_ciphertext: note::EncryptedNote,
    out_ciphertext: note::WrappedNoteKey,
    // This field is stored in a separate array in v5 transactions, see:
    // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
    // parse using `zcash_deserialize_external_count` and `zcash_serialize_external_count`
    zkproof: Groth16Proof,
}

/// Wrapper for `Output` serialization in a `V4` transaction.
struct OutputInTransactionV4(pub Output);

/// The serialization prefix fields of an `Output` in Transaction V5.
///
/// In `V5` transactions, spends are split into multiple arrays, so the prefix
/// and proof must be serialised and deserialized separately.
///
/// Serialized as `OutputDescriptionV5` in [protocol specification §7.3].
struct OutputPrefixInTransactionV5 {
    cv: commitment::ValueCommitment,
    cm_u: jubjub::Fq,
    ephemeral_key: keys::EphemeralPublicKey,
    enc_ciphertext: note::EncryptedNote,
    out_ciphertext: note::WrappedNoteKey,
}
```

The following fields have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `OutputInTransactionV4` (moved from `Output`)
- `OutputPrefixInTransactionV5` (new)
- `Groth16Proof`

Note: The serialize and deserialize implementations on `Output` are moved to
`OutputInTransactionV4`. In v4 transactions, outputs must be wrapped using
`into_v4` before serialization, and unwrapped using
`from_v4` after deserialization. In transaction v5, outputs
must be split using `into_v5_parts` before serialization, and
recombined using `from_v5_parts` after deserialization.

These convenience methods convert `Output` to:

- its v4 serialization wrapper `OutputInTransactionV4`, and
- its v5 parts: `OutputPrefixInTransactionV5` and the output proof.

## Adding V5 Transactions

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

The following fields have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `LockTime`
- `block::Height`
- `transparent::Input`
- `transparent::Output`
- `Option<sapling::ShieldedData<SharedAnchor>>` (new)
- `Option<orchard::ShieldedData>` (new)

## Orchard Additions

[orchard-additions]: #orchard-additions

### Adding Orchard ShieldedData

[adding-orchard-shieldeddata]: #adding-orchard-shieldeddata

The new V5 structure will create a new `orchard::ShieldedData` type. This new type will be defined in a new `zebra-chain/src/orchard/shielded_data.rs` file:

```rust
struct orchard::ShieldedData {
    flags: Flags,
    value_balance: Amount,
    shared_anchor: orchard::tree::Root,
    proof: Halo2Proof,
    actions: AtLeastOne<AuthorizedAction>,
    binding_sig: redpallas::Signature<Binding>,
}
```

The fields are ordered based on the **last** data deserialized for each field.

The following types have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `orchard::Flags` (new)
- `Amount`
- `Halo2Proof` (new)
- `redpallas::Signature<Binding>` (new)

### Adding Orchard AuthorizedAction

[adding-orchard-authorizedaction]: #adding-orchard-authorizedaction

In `V5` transactions, there is one `SpendAuth` signature for every `Action`. To ensure that this structural rule is followed, we create an `AuthorizedAction` type in `orchard/shielded_data.rs`:

```rust
/// An authorized action description.
///
/// Every authorized Orchard `Action` must have a corresponding `SpendAuth` signature.
struct orchard::AuthorizedAction {
    action: Action,
    // This field is stored in a separate array in v5 transactions, see:
    // https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus
    // parse using `zcash_deserialize_external_count` and `zcash_serialize_external_count`
    spend_auth_sig: redpallas::Signature<SpendAuth>,
}
```

Where `Action` is defined as [Action definition](https://github.com/ZcashFoundation/zebra/blob/68c12d045b63ed49dd1963dd2dc22eb991f3998c/zebra-chain/src/orchard/action.rs#L18-L41).

The following types have `ZcashSerialize` and `ZcashDeserialize` implementations,
because they can be serialized into a single byte vector:

- `Action` (new)
- `redpallas::Signature<SpendAuth>` (new)

Note: `AuthorizedAction` does not have serialize and deserialize implementations.
It must be split using `into_parts` before serialization, and
recombined using `from_parts` after deserialization.

These convenience methods convert between `AuthorizedAction` and its parts:
`Action` and the spend auth signature.

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

Note: A [consensus rule](https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus) was added to the protocol specification stating that:

> In a version 5 transaction, the reserved bits 2..7 of the flagsOrchard field MUST be zero.

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
    - A v5 transaction with no spends, but some outputs, to test the shared anchor serialization rule
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
