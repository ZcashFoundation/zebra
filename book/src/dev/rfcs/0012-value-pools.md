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

- `value balance` - the total value represented by a subset of the blockchain.
- `transparent value balance` - the sum of the outputs spent by all transparent `tx_in` fields, minus the sum of all `tx_out` fields.
- `sprout value balance` - the sum of all sprout `vpub_old` fields, minus the sum of all `vpub_new` fields.
- `sapling value balance` - the negation of the sum of all `valueBalanceSapling` fields.
- `orchard value balance` - the negation of the sum of all `valueBalanceOrchard` fields.
- `transaction value pool balance` - the sum of all the value balances in each transaction.
- `block value pool balance` - the sum of all the value balances in each block.
- `chain value pool balance` - the sum of all the value balances in a valid blockchain. Each of the transparent, sprout, sapling, and orchard chain value pool balances must be non-negative.

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

There is a value pool for transparent funds, and for each kind of shielded transfer. These value pools exist in each transaction, each block, and each chain.

We need to check each chain value pool as blocks are added to the chain, to make sure that chain balances never go negative.

We also need to check that non-coinbase transactions only spend the coins provided by their inputs.

Each of the chain value pools can change its value with every block added to the chain. This is a state feature and Zebra handle this in the `zebra-state` crate. We propose to store the pool values for the finalized tip height on disk.

TODO: summarise the implementation

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

vold takes value from the transparent transaction value pool and vnew adds value to the transparent transaction value pool . As a result, vold is treated like an output value, whereas vnew is treated like an input value.

As defined in [ZIP-209], the Sprout chain value pool balance for a given block chain is the sum of all vold field values for transactions in the block chain, minus the sum of all vnew fields values for transactions in the block chain.

Consensus rule: If the Sprout chain value pool balance would become negative in the block chain created as a result of accepting a block , then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#joinsplitbalance

### Sapling Chain Value Pool

A positive Sapling balancing value takes value from the Sapling transaction value pool and adds it to the transparent transaction value pool . A negative Sapling balancing value does the reverse. As a result, positive vbalanceSapling is treated like an input to the transparent transaction value pool , whereas negative vbalanceSapling is treated like an output from that pool.

As defined in [ZIP-209], the Sapling chain value pool balance for a given block chain is the negation of the sum of all valueBalanceSapling field values for transactions in the block chain.

Consensus rule: If the Sapling chain value pool balance would become negative in the block chain created as a result of accepting a block , then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#saplingbalance

### Orchard Chain Value Pool

Orchard introduces Action transfers, each of which can optionally perform a spend, and optionally perform an output. Similarly to Sapling, the net value of Orchard spends minus outputs in a transaction is called the Orchard balancing value, measured in zatoshi as a signed integer vbalanceOrchard.

vbalanceOrchard is encoded in a transaction as the field valueBalanceOrchard. If a transaction has no Action descriptions, vbalanceOrchard is implicitly zero. Transaction fields are described in § 7.1 ‘Transaction Encoding and Consensus’ on p. 116.

A positive Orchard balancing value takes value from the Orchard transaction value pool and adds it to the transpar- ent transaction value pool . A negative Orchard balancing value does the reverse. As a result, positive vbalanceOrchard is treated like an input to the transparent transaction value pool , whereas negative vbalanceOrchard is treated like an output from that pool.

Similarly to the Sapling chain value pool balance defined in [ZIP-209], the Orchard chain value pool balance for a given block chain is the negation of the sum of all valueBalanceOrchard field values for transactions in the block chain.

Consensus rule: If the Orchard chain value pool balance would become negative in the block chain created as a result of accepting a block , then all nodes MUST reject the block as invalid.

https://zips.z.cash/protocol/protocol.pdf#orchardbalance

## Proposed Implementation

### Create a new `ValueBalance` type

- Code will be located in a new file: `zebra-chain/src/value_balance.rs`.
- Supported operators apply to all the `Amount`s inside the type: `+`, `-`, `+=`, `-=`, `sum()`.
- We will use `Default` to represent a totally empty `ValueBalance`, this is the state of all pools at the genesis block.

```rust
struct ValueBalance<C> {
    transparent: Amount<C>,
    sprout: Amount<C>,
    sapling: Amount<C>,
    orchard: Amount<C>,
}

impl ValueBalance {
    /// Consensus rule: The remaining value in the transparent transaction value pool MUST be nonnegative.
    ///
    /// This rule applies to Block and Mempool transactions
    fn remaining_transparent_value(&self) -> Result<Amount<NonNegative>,Err> {
        // This rule checks the sum of the transparent, sprout, sapling, and orchard value balances in a transaction, not `ValueBalance.transparent`.
        [self.transparent, self.sprout, self.sapling, self.orchard].sum()
    }
}

impl Add for ValueBalance<C> {

}

impl Sub for ValueBalance<C> {
    
}

impl AddAssign for ValueBalance<C> {

}

impl SubAssign for ValueBalance<C> {
    
}

impl Sum for ValueBalance<C> {
    
}

impl Default for ValueBalance<C> {
    
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

```rust
/// utxos must contain the utxos of every input in the transaction
/// TODO: what about UTXOs created and spent in the transaction?
pub fn value_balance(&self, utxos: &HashMap<transparent::OutPoint, Utxo>) -> Result<ValueBalance<NegativeAllowed>, Err> {
    match self {
        Transaction::V1 { .. } => {},
        Transaction::V2 { .. } => {},
        Transaction::V3 { .. } => {},
        Transaction::V4 { .. } => {},
        Transaction::V5 { .. } => {
            
        },
    }
}
```

### Create a method in `Block` that returns `ValueBalance<NegativeAllowed>` for the block

- Method location is at `zebra-chain/src/block.rs`.
- Method will make use of `Transaction::value_balance` method created before.

```rust
/// utxos must contain the utxos of every input in the block
/// TODO: what about UTXOs created and spent in the block?
pub fn value_balance(&self, utxos: &HashMap<transparent::OutPoint, Utxo>) -> ValueBalance<NegativeAllowed> {
    self.transactions()
        .map(Transaction::value_balance)
        .sum()
        .unwrap_or_else(ValueBalance::default)
}
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
    value_pool: ValueBalance<NonNegative>,
}
```

### Update `update_chain_state_with()` for `Chain` struct to calculate Chain.value_pool

- Location: `zebra-state/src/service/non_finalized_state/chain.rs`

```rust
fn update_chain_state_with(&mut self, prepared: &PreparedBlock) {        
    ...
    self.value_pool += prepared.block_value_balance;
    ...
}
```

### Check pool balance consensus rules

#### Check that no pool is negative

- Do the check in `commit_block` and `commit_new_chain`, located in `zebra-state/src/service/non_finalized_state.rs`. Both methods will need new a new argument with the value pool saved in the disk.
- Pass this value to a new method `check_value_pools()` that will make the consensus rules check.

```rust
pub fn commit_block(
    &mut self,
    prepared: PreparedBlock,
    finalized_tip_history_tree: &HistoryTree,
    finalized_value_pool: ValueBalance<NonNegative>,
) -> Result<(), ValidateContextError> {
    ..
    check_value_pools(finalized_value_pools, prepared.value_balance)?;
    ..
}

pub fn commit_new_chain(
    &mut self,
    prepared: PreparedBlock,
    finalized_tip_history_tree: HistoryTree,
    finalized_value_pool: ValueBalance<NonNegative>,
) -> Result<(), ValidateContextError> {
    ..
    check_value_pools(finalized_value_pools, prepared.value_balance)?;
    ..
}

fn check_value_pools(
    finalized_value_pools: ValueBalance<NonNegative>,
    block_value_balance: ValueBalance<NegativeAllowed>
) -> Result<()> {
    if finalized_value_pools + block_value_balance < 0 {
        Err("Value pool can't be negative");
    }
    Ok(())
}
```

##### Consensus rule

The chain value pool balance rules apply to Block transactions, but they are optional for Mempool transactions:
> Nodes MAY relay transactions even if one or more of them cannot be mined due to the aforementioned restriction.

https://zips.z.cash/zip-0209#specification

Since Zebra does chain value pool balance validation in the state, we want to skip verifying the speculative chain balance of Mempool transactions.

### Changes to finalized state

The state service is what will call `commit_block()` and `commit_new_chain()`. We need to pass the value pool from the disk into this functions.

```
self.mem
    .commit_block(prepared, self.disk.history_tree(), self.disk.get_pool())?;
..
self.mem
    .commit_new_chain(prepared, self.disk.history_tree(), self.disk.get_pool())?;
```

We now detail what is needed in order to have the `get_pool()` method available.

#### Serialization of `ValueBalance<C>`

In order to save `ValueBalance` into the disk database we must implement `IntoDisk` and `FromDisk` for `ValueBalance` and for `Amount`:

```rust

impl IntoDisk for ValueBalance<C> {
    ..
}

impl FromDisk for ValueBalance<C> {
    ..
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
        i64::from_be_bytes(array).try_into().unwrap()
    }
}
```

#### Changes to `zebra-state/src/service/finalized_state.rs`

First we add a column for all the pools: transparent, sprout, sapling, orchard:

```rust
rocksdb::ColumnFamilyDescriptor::new("value_pools", db_options.clone()),
```

At block commit(`commit_finalized_direct()`) we create the handles for the new columns:

```rust
let value_pools = self.db.cf_handle("value_pools").unwrap();
```

Next we save each value pool into the field for each upcoming block except for the genesis block:

```rust
// Consensus rule: The block height of the genesis block is 0
// https://zips.z.cash/protocol/protocol.pdf#blockchain
if height == block::Height(0) {
    batch.zs_insert(value_pools, height, ValueBalance::default());
} else {
    let current_pool = self.get_pool();
    batch.zs_insert(value_pools, height, (current_pool + block.value_pool())?);
}

```

The `get_pool()` function will get the value of the pool at the tip as follows:

```rust
pub fn get_pool(&self, pool_name: Pool) -> ValuePool<NonNegative> {
    self.db.cf_handle("value_pools")
}
```

## Test Plan
[test-plan]: #test-plan

## Future Work
[future-work]: #future-work

Add an extra state request to verify the speculative chain balance after applying a Mempool transaction. (This is out of scope for our current NU5 and mempool work.)