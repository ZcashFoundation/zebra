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

V4 and V5 transactions both support sapling data but they are implemented differently. By this reason we need to split sapling data types into V4 and V5. We do this by using `enum` data types.

The order of some of the fields changed from V4 to V5. For example the `lock_time` and `expiry_height` were moved above the transparent inputs and outputs.

All of the changes proposed in this document are only to the `zebra-chain` crate.

To highlight changes all document comments from the code snippets in the [reference section](#reference-level-explanation) were removed.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

We start by looking how a V4 (already implemented) transaction is represented in Zebra.

This transaction version is specified by the protocol in the first table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus).


```
V4 {
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    lock_time: LockTime,
    expiry_height: block::Height,
    value_balance: Amount,
    joinsplit_data: Option<JoinSplitData<Groth16Proof>>,
    shielded_data: Option<ShieldedData>,
}
```

Currently the `ShieldedData` type is defined in `zebra-chain/src/transaction/shielded_data.rs` as follows:

```
pub struct ShieldedData {
    pub first: Either<Spend, Output>,
    pub rest_spends: Vec<Spend>,
    pub rest_outputs: Vec<Output>,
    pub binding_sig: Signature<Binding>,
}
```

We know by protocol (2nd table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus)) that V5 transactions will support sapling data however we also know by protocol that spends ([Spend Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#spendencodingandconsensus), See †) and outputs ([Output Description Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#outputencodingandconsensus), See †) fields change from V4 to V5.

Here we have the proposed changes for V4 transactions:

```
V4 {
    ...
    sapling_shielded_data: Option<SaplingShieldedData::V4>,
}
```


`ShieldedData` is currently defined and implemented in `zebra-chain/src/transaction/shielded_data.rs`. As this is Sapling specific we propose to move this file to `zebra-chain/src/sapling/shielded_data.rs`. Inside we will change `ShieldedData` into `SaplingShieldedData` and implement it as an enum with `V4` and `V5` variants.

```
pub enum SaplingShieldedData {
    V4 {
        /// Either a spend or output description.
        ///
        /// Storing this separately ensures that it is impossible to construct
        /// an invalid `SaplingShieldedData` with no spends or outputs.
        /// ...
        pub first: Either<Spend::V4, Output>,
        pub rest_spends: Vec<Spend::V4>,
        pub rest_outputs: Vec<Output>,
        pub sapling_value_balance: Amount,
        pub binding_sig: Signature<Binding>,
    },
    V5 {
        pub first: Either<Spend::V5, Output>,
        pub rest_spends: Vec<Spend::V5>,
        pub rest_outputs: Vec<Output>,
        pub sapling_value_balance: Amount,
        pub anchor: tree::Root,
        pub binding_sig: Signature<Binding>,
    }
}
```

Proposed `Spend` is now defined as an enum with `V4` and `V5` variants. Sapling spend code is located at `zebra-chain/src/sapling/spend.rs`. Notable difference here is that the `anchor` in `V4` is needed for every spend of the transaction while in `V5` the anchor is a single one defined in `SaplingShieldedData`:

```
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
In Zebra the output representations are the same for V4 and V5 so no variants are needed. The output code is located at `zebra-chain/src/sapling/output.rs`:

```
pub struct Output {
    pub cv: commitment::ValueCommitment,
    pub cm_u: jubjub::Fq,
    pub ephemeral_key: keys::EphemeralPublicKey,
    pub enc_ciphertext: note::EncryptedNote,
    pub out_ciphertext: note::WrappedNoteKey,
    pub zkproof: Groth16Proof,
}
```

Now lets see how the V5 transaction is specified in the protocol, this is the second table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus) and how are we going to represent it based in the above changes for Sapling fields and the new Orchard fields.

We propose the following representation for transaction V5 in Zebra:

```
V5 {
    lock_time: LockTime,
    expiry_height: block::Height,
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    sapling_shielded_data: Option<SaplingShieldedData::V5>,
    orchard_shielded_data: Option<OrchardShieldedData>,
}
```

`sapling_shielded_data` will now use `SaplingShieldedData::V5` variant located at `zebra-chain/src/transaction/sapling_shielded_data.rs` with the corresponding `Spend::V5` for the spends.

The new V5 structure will create a new `OrchardShieldedData` type. This new type will be defined in a separated file: `zebra-chain/src/orchard/shielded_data.rs` and it will look as follows:
```
pub struct OrchardShieldedData {
    /// An action description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `OrchardShieldedData` with no actions.
    pub first: Action,
    pub rest: Vec<Action>,
    pub flags: OrchardFlags,
    pub orchard_value_balance: Amount,
    pub anchor: tree::Root,
    pub binding_sig: redpallas::Signature<redpallas::Binding>,
}
```

Where `Action` is defined as [Action definition](https://github.com/ZcashFoundation/zebra/blob/68c12d045b63ed49dd1963dd2dc22eb991f3998c/zebra-chain/src/orchard/action.rs#L18-L41).

Finally, in the V5 transaction we have a new `OrchardFlags` type. This is a bitfield type defined as:

```
bitflags! {
    pub struct OrchardFlags: u8 {
        const ENABLE_SPENDS = 0b00000001;
        const ENABLE_OUTPUTS = 0b00000010;
        // Reserved, zeros (bits 2 .. 7)
    }
}
```

## Security

To avoid parsing memory exhaustion attacks, we will make the following changes across all `Transaction`, `ShieldedData`, `Spend` and `Output` variants, `V1` through to `V5`:
- Check cardinality consensus rules at parse time, before deserializing any `Vec`s
  - In general, Zcash requires that each transaction has at least one Transparent/Sprout/Sapling/Orchard transfer, this rule is not currently encoded in our data structures
- Stop parsing as soon as the first error is detected

These changes should be made in a later pull request, see [#1917](https://github.com/ZcashFoundation/zebra/issues/1917) for details.

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
