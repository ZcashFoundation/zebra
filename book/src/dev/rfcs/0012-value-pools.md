- Feature Name: value_pools
- Start Date: 2021-06-29
- Design PR: [ZcashFoundation/zebra#2430](https://github.com/ZcashFoundation/zebra/pull/2430)
- Zebra Issue: [ZcashFoundation/zebra#2152](https://github.com/ZcashFoundation/zebra/issues/2152)

# Summary
[summary]: #summary

This document describes how to verify the Zcash chain and transaction value pools in Zebra.

# Motivation
[motivation]: #motivation

In the Zcash protocol there are consensus rules that:
    - prohibit negative chain value pools [ZIP-209], and
    - restrict the creation of new money to a specific number of coins in each coinbase transaction. [Spec Section 3.4](https://zips.z.cash/protocol/protocol.pdf#transactions)

These rules make sure that a fixed amount of Zcash is created by each block, even if there are vulnerabilities in some shielded pools.

Checking the coins created by coinbase transactions and funding streams is out of scope for this design.

[ZIP-209]: https://zips.z.cash/zip-0209

# Definitions
[definitions]: #definitions

- `value balance` - The total change in value caused by a subset of the blockchain.
- `transparent value balance` - The change in the value of the transparent pool. The sum of the outputs spent by transparent inputs in `tx_in` fields, minus the sum of newly created outputs in `tx_out` fields.
- `coinbase transparent value balance` - The change in the value of the transparent pool due to a coinbase transaction. The coins newly created by the block, minus the sum of newly created outputs in `tx_out` fields. In this design, we temporarily assume that all coinbase outputs are valid, to avoid checking the created coins.
- `sprout value balance` - The change in the sprout value pool. The sum of all sprout `vpub_old` fields, minus the sum of all `vpub_new` fields.
- `sapling value balance` - The change in the sapling value pool. The negation of the sum of all `valueBalanceSapling` fields.
- `orchard value balance` - The change in the orchard value pool. The negation of the sum of all `valueBalanceOrchard` fields.
- `remaining transaction value` - The leftover value in each transaction, collected by miners as a fee. This value must be non-negative. In Zebra, calculated by subtracting the sprout, sapling, and orchard value balances from the transparent value balance. In the spec, defined as the sum of transparent inputs, minus transparent outputs, plus `v_sprout_new`, minus `v_sprout_old`, plus `vbalanceSapling`, plus `vbalanceOrchard`.
- `transaction value pool balance` - The sum of all the value balances in each transaction. There is a separate value for each transparent and shielded pool.
- `block value pool balance` - The sum of all the value balances in each block. There is a separate value for each transparent and shielded pool.
- `chain value pool balance` - The sum of all the value balances in a valid blockchain. Each of the transparent, sprout, sapling, and orchard chain value pool balances must be non-negative.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

There is a value pool for transparent funds, and for each kind of shielded transfer. These value pools exist in each transaction, each block, and each chain.

We need to check each chain value pool as blocks are added to the chain, to make sure that chain balances never go negative.

We also need to check that non-coinbase transactions only spend the coins provided by their inputs.

Each of the chain value pools can change its value with every block added to the chain. This is a state feature and Zebra handle this in the `zebra-state` crate. We propose to store the pool values for the finalized tip height on disk.

## Summary of the implementation:

- Create a new type `ValueBalance` that will contain `Amount`s for each pool(transparent, sprout, sapling, orchard).
- Create `value_pool()` methods on each relevant submodule (transparent, joinsplit, sapling and orchard).
- Create a `value_pool()` method in transaction with all the above and in block with all the transaction value balances.
- Pass the value balance of the incoming block into the state.
- Get a previously stored value balance.
- With both values check the consensus rules (constraint violations).
- Update the saved values for the new tip.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Consensus rules
[consensus-rules]: #consensus-rules

### Shielded Chain Value Pools

If any of the "Sprout chain value pool balance", "Sapling chain value pool balance", or "Orchard chain value pool balance" would become negative in the block chain created as a result of accepting a block, then all nodes MUST reject the block as invalid.

Nodes MAY relay transactions even if one or more of them cannot be mined due to the aforementioned restriction.

https://zips.z.cash/zip-0209#specification

### Transparent Transaction Value Pool & Remaining Value

Transparent inputs to a transaction insert value into a transparent transaction value pool associated with the transaction, and transparent outputs remove value from this pool. As in Bitcoin, the remaining value in the pool is available to miners as a fee.

Consensus rule: The remaining value in the transparent transaction value pool MUST be nonnegative.

https://zips.z.cash/protocol/protocol.pdf#transactions

Note: there is no explicit rule that the remaining balance in the transparent chain value pool must be non-negative. But it follows from the transparent transaction value pool consensus rule, and the definition of value addition.

### Sprout Chain Value Pool

Each JoinSplit transfer can be seen, from the perspective of the transparent transaction value pool , as an input and an output simultaneously.

`vold` takes value from the transparent transaction value pool and `vnew` adds value to the transparent transaction value pool . As a result, `vold` is treated like an output value, whereas `vnew` is treated like an input value.

As defined in [ZIP-209], the Sprout chain value pool balance for a given block chain is the sum of all `vold` field values for transactions in the block chain, minus the sum of all `vnew` fields values for transactions in the block chain.

Consensus rule: If the Sprout chain value pool balance would become negative in the block chain created as a result of accepting a block, then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#joinsplitbalance

### Sapling Chain Value Pool

A positive Sapling balancing value takes value from the Sapling transaction value pool and adds it to the transparent transaction value pool. A negative Sapling balancing value does the reverse. As a result, positive `vbalanceSapling` is treated like an input to the transparent transaction value pool, whereas negative `vbalanceSapling` is treated like an output from that pool.

As defined in [ZIP-209], the Sapling chain value pool balance for a given block chain is the negation of the sum of all `valueBalanceSapling` field values for transactions in the block chain.

Consensus rule: If the Sapling chain value pool balance would become negative in the block chain created as a result of accepting a block, then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#saplingbalance

### Orchard Chain Value Pool

Orchard introduces Action transfers, each of which can optionally perform a spend, and optionally perform an output. Similarly to Sapling, the net value of Orchard spends minus outputs in a transaction is called the Orchard balancing value, measured in zatoshi as a signed integer `vbalanceOrchard`.

`vbalanceOrchard` is encoded in a transaction as the field `valueBalanceOrchard`. If a transaction has no Action descriptions, `vbalanceOrchard` is implicitly zero. Transaction fields are described in § 7.1 ‘Transaction Encoding and Consensus’ on p. 116.

A positive Orchard balancing value takes value from the Orchard transaction value pool and adds it to the transparent transaction value pool. A negative Orchard balancing value does the reverse. As a result, positive `vbalanceOrchard` is treated like an input to the transparent transaction value pool, whereas negative `vbalanceOrchard` is treated like an output from that pool.

Similarly to the Sapling chain value pool balance defined in [ZIP-209], the Orchard chain value pool balance for a given block chain is the negation of the sum of all `valueBalanceOrchard` field values for transactions in the block chain.

Consensus rule: If the Orchard chain value pool balance would become negative in the block chain created as a result of accepting a block , then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#orchardbalance

## Proposed Implementation

### Create a new `ValueBalance` type

- Code will be located in a new file: `zebra-chain/src/value_balance.rs`.
- Supported operators apply to all the `Amount`s inside the type: `+`, `-`, `+=`, `-=`, `sum()`.
- Implementation of the above operators are similar to the ones implemented for `Amount<C>` in `zebra-chain/src/amount.rs`. In particular, we want to return a `Result` on them so we can error when a constraint is violated.
- We will use `Default` to represent a totally empty `ValueBalance`, this is the state of all pools at the genesis block.

```rust
#[serde(bound = "C: Constraint")]
struct ValueBalance<C = NegativeAllowed> {
    transparent: Amount<C>,
    sprout: Amount<C>,
    sapling: Amount<C>,
    orchard: Amount<C>,
}

impl ValueBalance {
    /// [Consensus rule]: The remaining value in the transparent transaction value pool MUST be nonnegative.
    ///
    /// This rule applies to Block and Mempool transactions.
    ///
    /// [Consensus rule]: https://zips.z.cash/protocol/protocol.pdf#transactions
    fn remaining_transaction_value(&self) -> Result<Amount<NonNegative>, Err> {
        // This rule checks the transparent value balance minus the sum of the sprout, sapling, and orchard
        // value balances in a transaction is nonnegative
        self.transparent - [self.sprout + self.sapling + self.orchard].sum()
    }
}

impl Add for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl Sub for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl AddAssign for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl SubAssign for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl Sum for Result<ValueBalance<C>>
where
    C: Constraint,
{

}

impl Default for ValueBalance<C>
where
    C: Constraint,
{

}
```

### Create a method in `Transaction` that returns `ValueBalance<NegativeAllowed>` for the transaction

We first add `value_balance()` methods in all the modules we need and use them to get the value balance for the whole transaction.

#### Create a method in `Input` that returns `ValueBalance<NegativeAllowed>`

- Method location is at `zebra-chain/src/transparent.rs`.
- Method need `utxos`, this information is available in `verify_transparent_inputs_and_outputs`.
- If the utxos are not available in the block or state, verification will timeout and return an error

```rust
impl Input {
    fn value_balance(&self, utxos: &HashMap<OutPoint, Utxo>) -> ValueBalance<NegativeAllowed> {

    }
}
```

#### Create a method in `Output` that returns `ValueBalance<NegativeAllowed>`

- Method location is at `zebra-chain/src/transparent.rs`.

```rust
impl Output {
    fn value_balance(&self) -> ValueBalance<NegativeAllowed> {

    }
}
```

#### Create a method in `JoinSplitData` that returns `ValueBalance<NegativeAllowed>`

- Method location is at `zebra-chain/src/transaction/joinsplit.rs`

```rust
pub fn value_balance(&self) -> ValueBalance<NegativeAllowed> {

}
```

#### Create a method in `sapling::ShieldedData` that returns `ValueBalance<NegativeAllowed>`

- Method location is at `zebra-chain/src/transaction/sapling/shielded_data.rs`

```rust
pub fn value_balance(&self) -> ValueBalance<NegativeAllowed> {

}
```

#### Create a method in `orchard::ShieldedData` that returns `ValueBalance<NegativeAllowed>`

- Method location is at `zebra-chain/src/transaction/orchard/shielded_data.rs`

```rust
pub fn value_balance(&self) -> ValueBalance<NegativeAllowed> {

}
```

#### Create the `Transaction` method

- Method location: `zebra-chain/src/transaction.rs`
- Method will use all the `value_balances()` we created until now.

```rust
/// utxos must contain the utxos of every input in the transaction,
/// including UTXOs created by earlier transactions in this block.
pub fn value_balance(&self, utxos: &HashMap<transparent::OutPoint, Utxo>) -> ValueBalance<NegativeAllowed> {

}
```

### Create a method in `Block` that returns `ValueBalance<NegativeAllowed>` for the block

- Method location is at `zebra-chain/src/block.rs`.
- Method will make use of `Transaction::value_balance` method created before.

```rust
/// utxos must contain the utxos of every input in the transaction,
/// including UTXOs created by a transaction in this block,
/// then spent by a later transaction that's also in this block.
pub fn value_balance(&self, utxos: &HashMap<transparent::OutPoint, Utxo>) -> ValueBalance<NegativeAllowed> {
    self.transactions()
        .map(Transaction::value_balance)
        .sum()
        .expect("Each block should have at least one coinbase transaction")
}
```

### Check the remaining transaction value consensus rule

- Do the check in `zebra-consensus/src/transaction.rs`
- Make the check part of the [basic checks](https://github.com/ZcashFoundation/zebra/blob/f817df638b1ba8cf8c261c536a30599c805cf04c/zebra-consensus/src/transaction.rs#L168-L177)

```rust
..
// Check the remaining transaction value consensus rule:
tx.value_balance().remaining_transaction_value()?;
..
```

### Pass the value balance for this block from the consensus into the state

- Add a new field into `PreparedBlock` located at `zebra-state/src/request.rs`, this is the `NonFinalized` section of the state.

```rust
pub struct PreparedBlock {
    ..
    /// The value balances for each pool for this block.
    pub block_value_balance: ValuePool<NegativeAllowed>,
}
```
- In `zebra-consensus/src/block.rs` pass the value balance to the zebra-state:

```rust
let block_value_balance = block.value_balance();
let prepared_block = zs::PreparedBlock {
    ..
    block_value_balance,
};
```

### Add a value pool into the state `Chain` struct

- This is the value pool for the non finalized part of the blockchain.
- Location of the `Chain` structure where the pool field will be added: `zebra-state/src/service/non_finalized_state/chain.rs`

```rust
pub struct Chain {
    ..
    /// The chain value pool balance at the tip of this chain.
    value_pool: ValueBalance<NonNegative>,
}
```
- Add a new argument `finalized_tip_value_balance` to the `commit_new_chain()` method located in the same file.
- Pass the new argument to the Chain in:

```rust
let mut chain = Chain::new(finalized_tip_history_tree, finalized_tip_value_balance);
```

Note: We don't need to pass the finalized tip value balance into the `commit_block()` method.

### Check the consensus rules when the chain is updated or reversed

- Location: `zebra-state/src/service/non_finalized_state/chain.rs`

```rust
impl UpdateWith<ValueBalance<NegativeAllowed>> for Chain {
    fn update_chain_state_with(&mut self, value_balance: &ValueBalance<NegativeAllowed>) -> Result<(), Err> {
        self.value_pool = (self.value_pool + value_balance)?;
        Ok(())
    }
    fn revert_chain_state_with(&mut self, value_balance: &ValueBalance<NegativeAllowed>) -> Result<(), Err> {
        self.value_pool = (self.value_pool + value_balance)?;
        Ok(())
    }
}
```

### Changes to finalized state

The state service will call `commit_new_chain()`. We need to pass the value pool from the disk into this function.

```rust
self.mem
    .commit_new_chain(prepared, self.disk.history_tree(), self.disk.get_pool())?;
```

We now detail what is needed in order to have the `get_pool()` method available.

#### Serialization of `ValueBalance<C>`

In order to save `ValueBalance` into the disk database we must implement `IntoDisk` and `FromDisk` for `ValueBalance` and for `Amount`:

```rust
impl IntoDisk for ValueBalance<C> {
    type Bytes = [u8; 32];

    fn as_bytes(&self) -> Self::Bytes {
        [self.transparent.to_bytes(), self.sprout.to_bytes(),
        self.sapling.to_bytes(), self.orchard.to_bytes()].concat()
    }
}

impl FromDisk for ValueBalance<C> {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        ValueBalance {
            transparent: Amount::from_bytes(array[0..8]).try_into().unwrap()
            sprout: Amount::from_bytes(array[8..16]).try_into().unwrap()
            sapling: Amount::from_bytes(array[16..24]).try_into().unwrap()
            orchard: Amount::from_bytes(array[24..32]).try_into().unwrap()
        }
    }
}

impl IntoDisk for Amount {
    type Bytes = [u8; 8];

    fn as_bytes(&self) -> Self::Bytes {
        self.to_bytes()
    }
}

impl FromDisk for Amount {
    fn from_bytes(bytes: impl AsRef<[u8]>) -> Self {
        let array = bytes.as_ref().try_into().unwrap();
        Amount::from_bytes(array)
    }
}
```
The above code is going to need a `Amount::from_bytes` new method.

#### Add a `from_bytes` method in `Amount`

- Method location is at `zebra-chain/src/amount.rs`
- A `to_bytes()` method already exist, place `from_bytes()` right after it.

```rust
/// From little endian byte array
pub fn from_bytes(&self, bytes: [u8; 8]) -> Self {
    let amount = i64::from_le_bytes(bytes).try_into().unwrap();
    Self(amount, PhantomData)
}
```

#### Changes to `zebra-state/src/request.rs`

Add a new field to `FinalizedState`:

```rust
pub struct FinalizedBlock {
    ..
    /// The value balance for transparent, sprout, sapling and orchard
    /// inside all the transactions of this block.
    pub(crate) block_value_balance: ValueBalance<NegativeAllowed>,
}
```

Populate it when `PreparedBlock` is converted into `FinalizedBlock`:

```rust
impl From<PreparedBlock> for FinalizedBlock {
    fn from(prepared: PreparedBlock) -> Self {
        let PreparedBlock {
            ..
            block_value_balance,
        } = prepared;
        Self {
            ..
            block_value_balance,
        }
    }
}
```

#### Changes to `zebra-state/src/service/finalized_state.rs`

First we add a column of type `ValueBalance` that will contain `Amount`s for all the pools: transparent, sprout, sapling, orchard:

```rust
rocksdb::ColumnFamilyDescriptor::new("tip_chain_value_pool", db_options.clone()),
```

At block commit(`commit_finalized_direct()`) we create the handle for the new column:

```rust
let tip_chain_value_pool = self.db.cf_handle("tip_chain_value_pool").unwrap();
```

Next we save each tip value pool into the field for each upcoming block except for the genesis block:

```rust
// Consensus rule: The block height of the genesis block is 0
// https://zips.z.cash/protocol/protocol.pdf#blockchain
if height == block::Height(0) {
    batch.zs_insert(tip_chain_value_pool, height, ValueBalance::default());
} else {
    let current_pool = self.current_value_pool();
    batch.zs_insert(tip_chain_value_pool, height, (current_pool + finalized.block_value_balance)?);
}
```

The `current_value_pool()` function will get the stored value of the pool at the tip as follows:

```rust
pub fn current_value_pool(&self) -> ValuePool<NonNegative> {
    self.db.cf_handle("tip_chain_value_pool")
}
```

## Test Plan
[test-plan]: #test-plan

### Unit tests
 - Create a transaction that has a negative remaining value.
   - Test that the transaction fails the verification in `Transaction::value_balance()`
   - To avoid passing the utxo we can have `0` as the amount of the transparent pool and some negative shielded pool.

### Prop tests

 - Create a chain strategy that ends up with a valid value balance for all the pools (transparent, sprout, sapling, orchard)
   - Test that the amounts are all added to disk.
 - Add new blocks that will make each pool became negative.
   - Test for constraint violations in the value balances for each case.
   - Failures should be at `update_chain_state_with()`.
 - Test consensus rules success and failures in `revert_chain_state_with()`
   - TODO: how?
 - serialize and deserialize `ValueBalance` using `IntoDisk` and `FromDisk`

 ### Manual tests

 - Zebra must sync up to tip computing all value balances and never breaking the value pool rules.

## Future Work
[future-work]: #future-work

Add an extra state request to verify the speculative chain balance after applying a Mempool transaction. (This is out of scope for our current NU5 and mempool work.)

Note: The chain value pool balance rules apply to Block transactions, but they are optional for Mempool transactions:
> Nodes MAY relay transactions even if one or more of them cannot be mined due to the aforementioned restriction.

https://zips.z.cash/zip-0209#specification

Since Zebra does chain value pool balance validation in the state, we want to skip verifying the speculative chain balance of Mempool transactions.
