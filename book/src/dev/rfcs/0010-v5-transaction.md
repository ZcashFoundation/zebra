- Feature Name: `v5_transaction`
- Start Date: 2021-03-11
- Design PR: [ZcashFoundation/zebra#1886](https://github.com/ZcashFoundation/zebra/pull/1886)
- Zebra Issue: [ZcashFoundation/zebra#1863](https://github.com/ZcashFoundation/zebra/issues/1863)

# Summary
[summary]: #summary

Network Upgrade number 5 (`NU5`) introduces a new transaction type (transaction version 5). This document is a proposed design for implementing such a transaction version.

# Motivation
[motivation]: #motivation

The Zebra software wants to be a protocol compatible Zcash implementation. One of the tasks to do this includes the support of the new version 5 transactions that will be implemented in the next network upgrade.

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

We need the raw representation to Serialize/Deserialize the transaction however Zebra uses its own representation internally.

V5 transactions are the only ones that will support orchard transactions with `Orchard` data types.

V4 and V5 transactions both support sapling data but they are implemented differently. By this reason we need to split sapling data types into V4 and V5.

The order of some of the fields changed from V4 to V5. For example the `lock_time` and `expiry_height` were moved above the transparent inputs and outputs.

All of the changes proposed in this document are only to the `zebra-chain` crate.

To highlight changes most of the document comments from the code snippets in the [reference section](#reference-level-explanation) were removed.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Sapling Changes
[sapling-changes]: #sapling-changes

We know by protocol (2nd table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)) that V5 transactions will support sapling data however we also know by protocol that spends ([Spend Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#spendencodingandconsensus), See †) and outputs ([Output Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#outputencodingandconsensus), See †) fields change from V4 to V5.

### Changes to V4 Transactions
[changes-to-v4-transactions]: #changes-to-v4-transactions

Here we have the proposed changes for `Transaction::V4`:
* make `sapling_shielded_data` use the `V4` shielded data type (**TODO: how?**)
* rename `shielded_data` to `sapling_shielded_data`
* move `value_balance` into the `sapling::ShieldedData` type
* order fields based on the **last** data deserialized for each field

```rust
pub enum Transaction::V4 {
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    lock_time: LockTime,
    expiry_height: block::Height,
    joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    sapling_shielded_data: Option<sapling::ShieldedData::V4>, // Note: enum variants can't be generic parameters in Rust
}
```

### Changes to Sapling ShieldedData
[changes-to-sapling-shieldeddata]: #changes-to-sapling-shieldeddata

`ShieldedData` is currently defined and implemented in `zebra-chain/src/transaction/shielded_data.rs`. As this is Sapling specific we propose to move this file to `zebra-chain/src/sapling/shielded_data.rs`. We will also change `ShieldedData` into an enum with `V4` and `V5` variants.

```rust
pub enum sapling::ShieldedData {
    V4 {
        pub value_balance: Amount,
        /// Either a spend or output description.
        ///
        /// Storing this separately ensures that it is impossible to construct
        /// an invalid `ShieldedData` with no spends or outputs.
        /// ...
        pub first: Either<Spend::V4, Output>, // Note: enum variants can't be generic parameters in Rust
        pub rest_spends: Vec<Spend::V4>, // Note: enum variants can't be generic parameters in Rust
        pub rest_outputs: Vec<Output>,
        pub binding_sig: Signature<Binding>,
    },
    V5 {
        pub value_balance: Amount,
        pub anchor: tree::Root,
        pub first: Either<Spend::V5, Output>, // Note: enum variants can't be generic parameters in Rust
        pub rest_spends: Vec<Spend::V5>, // Note: enum variants can't be generic parameters in Rust
        pub rest_outputs: Vec<Output>,
        pub binding_sig: Signature<Binding>,
    }
}
```

### Adding V5 Sapling Spend
[adding-v5-sapling-spend]: #adding-v5-sapling-spend

Proposed `Spend` is now defined as an enum with `V4` and `V5` variants. Sapling spend code is located at `zebra-chain/src/sapling/spend.rs`. Notable difference here is that the `anchor` in `V4` is needed for every spend of the transaction while in `V5` the anchor is a single one defined in `sapling::ShieldedData`:

```rust
pub enum Spend {
    V4 {
        pub cv: commitment::ValueCommitment,
        pub anchor: tree::Root,
        pub nullifier: note::Nullifier,
        pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
        pub zkproof: Groth16Proof,
        pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
    },
    V5 {
        pub cv: commitment::ValueCommitment,
        pub nullifier: note::Nullifier,
        pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
        pub zkproof: Groth16Proof,
        pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
    }
}
```

### No Changes to Sapling Output
[no-changes-to-sapling-output]: #no-changes-to-sapling-output

In Zcash the Sapling output representations are the same for V4 and V5 transactions, so no variants are needed. The output code is located at `zebra-chain/src/sapling/output.rs`:

```rust
pub struct Output {
    pub cv: commitment::ValueCommitment,
    pub cm_u: jubjub::Fq,
    pub ephemeral_key: keys::EphemeralPublicKey,
    pub enc_ciphertext: note::EncryptedNote,
    pub out_ciphertext: note::WrappedNoteKey,
    pub zkproof: Groth16Proof,
}
```

## Orchard Additions
[orchard-additions]: #orchard-additions

### Adding V5 Transactions
[adding-v5-transactions]: #adding-v5-transactions

Now lets see how the V5 transaction is specified in the protocol, this is the second table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus) and how are we going to represent it based in the above changes for Sapling fields and the new Orchard fields.

We propose the following representation for transaction V5 in Zebra:

```rust
pub enum Transaction::V5 {
    lock_time: LockTime,
    expiry_height: block::Height,
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    sapling_shielded_data: Option<sapling::ShieldedData::V5>, // Note: enum variants can't be generic parameters in Rust
    orchard_shielded_data: Option<orchard::ShieldedData>,
}
```

`sapling_shielded_data` will now use `sapling::ShieldedData::V5` variant located at `zebra-chain/src/transaction/sapling/shielded_data.rs` with the corresponding `Spend::V5` for the spends. (**TODO: how?**)

### Adding Orchard ShieldedData
[adding-orchard-shieldeddata]: #adding-orchard-shieldeddata

The new V5 structure will create a new `orchard::ShieldedData` type. This new type will be defined in a separated file: `zebra-chain/src/orchard/shielded_data.rs` and it will look as follows:

```rust
pub struct orchard::ShieldedData {
    pub flags: Flags,
    pub value_balance: Amount,
    pub anchor: tree::Root,
    combined_proof: Halo2Proof,
    /// An authorized action description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no actions.
    pub first: AuthorizedAction,
    pub rest: Vec<AuthorizedAction>,
    pub binding_sig: redpallas::Signature<redpallas::Binding>,
}
```

The fields are ordered based on the **last** data deserialized for each field.

### Adding Orchard AuthorizedAction
[adding-orchard-authorizedaction]: #adding-orchard-authorizedaction

In `V5` transactions, there is one `SpendAuth` signature for every `Action`. To ensure that this structural rule is followed, we create an `AuthorizedAction` type:

```rust
/// An authorized action description.
///
/// Every authorized Orchard `Action` must have a corresponding `SpendAuth` signature.
pub struct orchard::AuthorizedAction {
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
    pub struct orchard::Flags: u8 {
        const ENABLE_SPENDS = 0b00000001;
        const ENABLE_OUTPUTS = 0b00000010;
        // Reserved, zeros (bits 2 .. 7)
    }
}
```

## Test Plan
[test-plan]: #test-plan

- All renamed, modified and new types should serialize and deserialize. 
- The full V4 and V5 transactions should serialize and deserialize.
- Prop test strategies for v4 and v5 will be updated and created.
- Before NU5 activation on testnet, test on the following test vectors:
  - Hand-crafted Orchard-only, Orchard/Sapling, Orchard/Transparent, and Orchard/Sapling/Transparent transactions based on the spec
  - "Fake" Sapling-only and Sapling/Transparent transactions based on the existing test vectors, converted from `V4` to `V5` format
    - We can write a test utility function to automatically do these conversions
  - An empty transaction, with no Orchard, Sapling, or Transparent data
  - Any available `zcashd` test vectors
- After NU5 activation on testnet:
  - Add test vectors using the testnet activation block and 2 more post-activation blocks
- After NU5 activation on mainnet:
  - Add test vectors using the mainnet activation block and 2 more post-activation blocks

# Security

To avoid parsing memory exhaustion attacks, we will make the following changes across all `Transaction`, `ShieldedData`, `Spend` and `Output` variants, `V1` through to `V5`:
- Check cardinality consensus rules at parse time, before deserializing any `Vec`s
  - In general, Zcash requires that each transaction has at least one Transparent/Sprout/Sapling/Orchard transfer, this rule is not currently encoded in our data structures
- Stop parsing as soon as the first error is detected

These changes should be made in a later pull request, see [#1917](https://github.com/ZcashFoundation/zebra/issues/1917) for details.
