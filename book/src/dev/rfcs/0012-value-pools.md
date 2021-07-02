- Feature Name: value_pools
- Start Date: 2021-06-29
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
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

Checking the coins created by coinbase transactions is out of scope.

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

Question: can we also implement a value_balance method on transparent::Input, transparent::Output, JoinSplit, sapling::ShieldedData, and orchard::ShieldedData?
- the transparent::Input method will need the UTXO referenced by the OutPoint
  - this information is available in verify_transparent_inputs_and_outputs
  - we'll need to look up the UTXOs in the transaction verifier, not the script verifier (TODO: open a ticket for this refactor)
  - if the utxos are not available in the block or state, verification will timeout and return an error

- Method location is at `zebra-chain/src/transaction.rs`.
- Depending on if we implement per-shielded data, the implementations may also go into their respective modules

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

// May return an Err if we exceed maximum possible balance, for example

### Create a method in `Block` that returns `ValueBalance<NegativeAllowed>` for the block

- Method location is at `zebra-chain/src/block.rs`.
- Method will make use of `Transaction::value_balance` method created before.
- Do we want to 
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

- Add a new field into `PreparedBlock` located at `zebra-state/src/request.rs`, this is NonFinalized section of the state.

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

### Update `update_chain_state_with() ` for `Chain` struct to calculate Chain.value_pool

- Location of the `Chain` structure where the pool field will be added: `zebra-state/src/service/non_finalized_state/chain.rs`

```rust
fn update_chain_state_with(&mut self, prepared: &PreparedBlock) {        
    ...
    self.value_pool += prepared.block_value_balance;
    ...
}
```

### Check pool balance consensus rules

- Check that no pool is negative.
- Do the check in `commit_block` and `commit_new_chain`, located in `zebra-state/src/service/non_finalized_state.rs`. (See PR #2301.)
  - We might want to add a method that is called from both `commit_block` and `commit_new_chain` with the chain
  - https://github.com/ZcashFoundation/zebra/pull/2301/files
- We have the `Chain` and the `PreparedBlock` to apply the consensus rules.

```rust
add some code
```

#### Consensus rules

The chain value pool balance rules apply to Block transactions, but they are optional for Mempool transactions:
> Nodes MAY relay transactions even if one or more of them cannot be mined due to the aforementioned restriction.

https://zips.z.cash/zip-0209#specification

Since Zebra does chain value pool balance validation in the state, we want to skip verifying the speculative chain balance of Mempool transactions.

### When committing a PreparedBlock (non-finalized block), pass the Chain's pool value balance

Explain why..

In zebra-state/request.rs

Request::CommitBlock(PreparedBlock)

### When committing a FinalizedBlock, calculate the value balance for that block

Explain why.

In zebra-state/request.rs

Request::CommitFinalizedBlock(FinalizedBlock)

### Calculate block value balances

### Calculate pool value balances

### In `commit_finalized_direct` update the pool value balances in the state as part of each block commit batch

### Serialization of `ValueBalance<C>`

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

### Changes to `zebra-state/src/service/finalized_state.rs`

First we add a column for all the pools: transparent, sprout, sapling, orchard

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