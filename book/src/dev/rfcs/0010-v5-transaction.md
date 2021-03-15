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
    sapling_value_balance: Amount,
    ...
    sapling_shielded_data: Option<SaplingShieldedDataV4>,
}
```

`ShieldedData` is currently defined and implemented in `src/transaction/shielded_data.rs`. We propose to rename this file to `src/transaction/sapling_shielded_data.rs`. Inside we will change `ShieldedData` into `SaplingShieldedDataV4` and add the new version `SaplingShieldedDataV5`.

The difference between V4 and V5 shielded data is in the spends and outputs. For this reason we need to add V4 and V5 spend and output versions to our code:

```
pub struct SaplingShieldedDataV4 {
    /// Either a spend or output description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `ShieldedData` with no spends or outputs.
    /// ...
    pub first: Either<SpendV4, OutputV4>,
    pub rest_spends: Vec<SpendV4>,
    pub rest_outputs: Vec<OutputV4>,
    pub binding_sig: Signature<Binding>,
}
```

Proposed `SpendV4` and `OutputV4` is now described, this is the same as the current implementation but just changing the struct names. 
Sapling spend code is located at `zebra-chain/src/sapling/spend.rs`:

```
pub struct SpendV4 {
    pub cv: commitment::ValueCommitment,
    pub anchor: tree::Root,
    pub nullifier: note::Nullifier,
    pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
    pub zkproof: Groth16Proof,
    pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
}
```

The output code is located at `zebra-chain/src/sapling/output.rs`:

```
pub struct OutputV4 {
    pub cv: commitment::ValueCommitment,
    pub cm_u: jubjub::Fq,
    pub ephemeral_key: keys::EphemeralPublicKey,
    pub enc_ciphertext: note::EncryptedNote,
    pub out_ciphertext: note::WrappedNoteKey,
    pub zkproof: Groth16Proof,
}
```

Now lets see how the v5 transaction is specified in the protocol, this is the second table of [Transaction Encoding and Consensus](https://zips.z.cash/protocol/nu5.pdf#txnencodingandconsensus).

We propose the following representation for transaction V5 in Zebra:

```
V5 {
    lock_time: LockTime,
    expiry_height: block::Height,
    inputs: Vec<transparent::Input>,
    outputs: Vec<transparent::Output>,
    sapling_value_balance: Amount,
    sapling_anchor: tree::Root,
    sapling_shielded_data: Option<SaplingShieldedDataV5>,
    orchard_flags: OrchardFlags,
    orchard_value_balance: Amount,
    orchard_anchor: tree::Root,
    orchard_shielded_data: Option<OrchardShieldedData>,
}
```

`SaplingShieldedDataV5` is different from `SaplingShieldedDataV4` so a new type will be defined and implemented in `zebra-chain/src/transaction/sapling_shielded_data.rs`.
Definition will look as follows:

```
pub struct SaplingShieldedDataV5 {
    pub first: Either<SpendV5, OutputV5>,
    pub rest_spends: Vec<SpendV5>,
    pub rest_outputs: Vec<OutputV5>,
    pub binding_sig: Signature<Binding>,
}
```

This leads to new sapling output and spend types.

V5 sapling spend will be defined and implemented in `zebra-chain/src/sapling/spend.rs` (this is in the same file as `SpendV4`):
```
pub struct SpendV5 {
    pub cv: commitment::ValueCommitment,
    pub nullifier: note::Nullifier,
    pub rk: redjubjub::VerificationKeyBytes<SpendAuth>,
    pub zkproof: Groth16Proof,
    pub spend_auth_sig: redjubjub::Signature<SpendAuth>,
}
```

V5 sapling output will live in `zebra-chain/src/sapling/output.rs` (this is in the same file as `OutputV4`).

Note: the V4 and V5 sapling outputs currently have identical fields. We use different types to consistently distinguish V4 and V5 transaction data.

```
pub struct OutputV5 {
    pub cv: commitment::ValueCommitment,
    pub cm_u: jubjub::Fq,
    pub ephemeral_key: keys::EphemeralPublicKey,
    pub enc_ciphertext: note::EncryptedNote,
    pub out_ciphertext: note::WrappedNoteKey,
    pub zkproof: Groth16Proof,
}
```
To abstract over the `V4` and `V5` `Transaction` enum variants, we will implement `Transaction` methods to access sapling and orchard data. 

To abstract over the `SaplingShieldedDataV4`/`SaplingShieldedDataV5`, `SpendV4`/`SpendV5`, and `OutputV4`/`OutputV5` structs, we will implement `SaplingShieldedData`, `Spend`, and `Output` traits to access sapling data.

The methods in these traits will be implemented in a similar way to the existing `Transaction` methods.
The new V5 structure will create a new `OrchardShieldedData` type. This new type will be defined in a separated file: `zebra-chain/src/transaction/orchard_shielded_data.rs` and it will look as follows:
```
pub struct OrchardShieldedData {
    /// An action description.
    ///
    /// Storing this separately ensures that it is impossible to construct
    /// an invalid `OrchardShieldedData` with no actions.
    pub first: Action,
    pub rest: Vec<Action>,
    pub binding_sig: Signature<Binding>,
}
```

Where `Action` is defined as [Action definition](https://github.com/ZcashFoundation/zebra/blob/68c12d045b63ed49dd1963dd2dc22eb991f3998c/zebra-chain/src/orchard/action.rs#L18-L41).

Finally, in the V5 transaction we have a new `OrchardFlags`. This is a bitfield type defined as:

```
bitflags! {
    pub struct OrchardFlags: u8 {
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
  - Hand-crafted Orchard, Orchard/Sapling, Orchard/Sprout, and Orchard/Sapling/Sprout transactions based on the spec
  - Converted Sapling and Sapling/Sprout transactions in the existing test vectors from `V4` to `V5` format
  - Any available `zcashd` test vectors
- After NU5 activation on testnet:
  - Add test vectors using the testnet activation block and 2 more post-activation blocks
- After NU5 activation on mainnet:
  - Add test vectors using the mainnet activation block and 2 more post-activation blocks
