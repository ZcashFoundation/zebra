- Feature Name: 2021-03-21
- Start Date: (fill me in with today's date, YYYY-MM-DD)
- Design PR: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/pull/0000)
- Zebra Issue: [ZcashFoundation/zebra#0000](https://github.com/ZcashFoundation/zebra/issues/0000)

# Summary
[summary]: #summary

This document describes how to implement the Zcash value pools in Zebra.

# Motivation
[motivation]: #motivation

Value pools are used in consensus rules in at least 2 places:

- [ZIP-209](https://zips.z.cash/zip-0209) - Consensus rules to reject any block that results in negative pools. 
- [Miner fees](https://github.com/ZcashFoundation/zebra/blob/62b3344f4d8605aad03133b8c0fa94cfb622d4a7/book/src/dev/rfcs/xxxx-block-subsidy.md#transaction-fees-calculation) - Fees collected for a block is the value pool for that block and they belong to the miner reward. 

In addition, Zcashd has RPC calls that allow to get each pool on each block with `getblock()` and the value pools at current tip with `getblockchaininfo()`.

# Definitions
[definitions]: #definitions

- `transparent pool` - Sum of all `tx_in` fields for transactions in the block chain, minus the sum of all `tx_out` fields for transactions in the block chain.
- `sprout pool` - Sum of all `vpub_old` fields for transactions in the block chain, minus the sum of all `vpub_new` fields for transactions in the block chain [ZIP-209](https://zips.z.cash/zip-0209#terminology).
- `sapling pool` - Negation of the sum of all `valueBalanceSapling` fields for transactions in the block chain [ZIP-209](https://zips.z.cash/zip-0209#terminology).
- `orchard pool` - Negation of the sum of all `valueBalanceOrchard` fields for transactions in the block chain.
- `transaction pool`
- `block pool`

# Guide-level explanation
[guide-level-explanation]: #guide-level-explanation

Each of the 4 value pools change its value with every upcoming block. This is a state feature and Zebra handle this in the `zebra-state` crate. We propose to store the pool values among a reference block height into the disk.

The proposed design goes from calculating transaction pools, summing them all to form block pools and finally summing all block pools to form the final pool value.

# Reference-level explanation
[reference-level-explanation]: #reference-level-explanation

## Changes to `zebra-chain/src/transaction.rs`

We have a function to calculate each transaction pool for any transaction version:

### `transparent_value_pool()`

TODO

### `sprout_value_pool()`

```rust
pub fn sprout_value_pool(&self) -> Amount {
    match self {
        Transaction::V1 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V2 { joinsplit_data, .. } |
        Transaction::V3 { joinsplit_data, .. } => {
            let mut value_balance: Amount<NegativeAllowed> = Amount::try_from(0).unwrap();
            for js in joinsplit_data.as_ref().unwrap().joinsplits() {
                value_balance = (value_balance + (js.vpub_old - js.vpub_new).unwrap().constrain()).unwrap();
            }
            value_balance
        },
        Transaction::V4 { joinsplit_data, .. } => {
            let mut value_balance: Amount<NegativeAllowed> = Amount::try_from(0).unwrap();
            for js in joinsplit_data.as_ref().unwrap().joinsplits() {
                value_balance = (value_balance + (js.vpub_old - js.vpub_new).unwrap().constrain()).unwrap();
            }
            value_balance
        },
        Transaction::V5 { .. } => Amount::try_from(0).unwrap(),
    }
}
```

### `sapling_value_pool()`

```rust
pub fn sapling_value_pool(&self) -> Amount {
    match self {
        Transaction::V1 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V2 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V3 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V4 { value_balance, .. } => *value_balance,
        Transaction::V5 { .. } => Amount::try_from(0).unwrap(),
    }
}
```

### `orchard_value_pool()`

```rust
pub fn orchard_value_pool(&self) -> Amount {
    match self {
        Transaction::V1 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V2 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V3 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V4 { .. } => Amount::try_from(0).unwrap(),
        Transaction::V5 { .. } => Amount::try_from(0).unwrap(),
    }
}
```

## Changes to `zebra-chain/src/block.rs`

We propose to add a `Pool` enum here:

```rust
#[derive(Clone, Copy)]
pub enum Pool {
    Transparent,
    Sprout,
    Sapling,
    Orchard,
}
```

And a `value_pool()` function that will sum all the transaction pools for a specific block:

```rust
pub fn value_pool(&self, pool: Pool) -> Amount {
    let mut total_value_balance = Amount::try_from(0).unwrap();
    for t in self.transactions.clone() {
        let value_balance = match pool {
            Pool::Transparent => t.transparent_value_pool(),
            Pool::Sprout => t.sprout_value_pool(),
            Pool::Sapling => t.sapling_value_pool(),
            Pool::Orchard => t.orchard_value_pool(),
        };
        total_value_balance = (total_value_balance + value_balance).unwrap();
    }
    total_value_balance 
}
```

## Changes to `zebra-state/src/service/finalized_state/disk_format.rs`

Value of each pool can be represented in Zebra by `Amount<NegativeAllowed>`. In order to save `Amount`s into the disk database we must implement `IntoDisk` and `FromDisk` for `Amount`:

```rust
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

## Changes to `zebra-state/src/service/finalized_state.rs`

First we add columns for each pool:

```rust
rocksdb::ColumnFamilyDescriptor::new("transparent_pool", db_options.clone()),
rocksdb::ColumnFamilyDescriptor::new("sprout_pool", db_options.clone()),
rocksdb::ColumnFamilyDescriptor::new("sapling_pool", db_options.clone()),
rocksdb::ColumnFamilyDescriptor::new("orchard_pool", db_options.clone()),
```

At block commit(`commit_finalized_direct()`) we crete the handles for the new columns:

```rust
let transparent_pool = self.db.cf_handle("transparent_pool").unwrap();
let sprout_pool = self.db.cf_handle("sprout_pool").unwrap();
let sapling_pool = self.db.cf_handle("sapling_pool").unwrap();
let orchard_pool = self.db.cf_handle("orchard_pool").unwrap();
```

Next we save each value pool for each upcoming block:

```rust
let mut current_transparent_pool = Amount::try_from(0).unwrap();
let mut current_sprout_pool = Amount::try_from(0).unwrap();
let mut current_sapling_pool = Amount::try_from(0).unwrap();
let mut current_orchard_pool = Amount::try_from(0).unwrap();

if height > block::Height(1) {
    current_transparent_pool = self.pool(Pool::Transparent).unwrap().1;
    current_sprout_pool = self.pool(Pool::Sprout).unwrap().1;
    current_sapling_pool = self.pool(Pool::Sapling).unwrap().1;
    current_orchard_pool = self.pool(Pool::Orchard).unwrap().1;
}

batch.zs_insert(transparent_pool, height, (current_transparent_pool + block.value_pool(Pool::Transparent)).unwrap());
batch.zs_insert(sprout_pool, height, (current_sprout_pool + block.value_pool(Pool::Sprout)).unwrap());
batch.zs_insert(sapling_pool, height, (current_sapling_pool + block.value_pool(Pool::Sapling)).unwrap());
batch.zs_insert(orchard_pool, height, (current_orchard_pool + block.value_pool(Pool::Orchard)).unwrap());
```

Note that each pool is a sum of the amount in the last block plus the amount in the upcoming current block. Each block must have a value pool amount in the previous block (can be 0) except for the genesis block.

The `pool()` function will get the value of the pool at the tip as follows:

```rust
/// Returns the requested pool at tip.
pub fn pool(&self, pool_name: Pool) -> Option<(block::Height, Amount)> {
    let pool = match pool_name {
        Pool::Transparent => self.db.cf_handle("transparent_pool").unwrap(),
        Pool::Sprout => self.db.cf_handle("sprout_pool").unwrap(),
        Pool::Sapling => self.db.cf_handle("sapling_pool").unwrap(),
        Pool::Orchard => self.db.cf_handle("orchard_pool").unwrap(),
    };
    self.db
        .iterator_cf(pool, rocksdb::IteratorMode::End)
        .next()
        .map(|(height_bytes, amount_bytes)| {
            let height = block::Height::from_bytes(height_bytes);
            let amount = Amount::from_bytes(amount_bytes);

            (height, amount)
        })
}
```

## Test Plan
[test-plan]: #test-plan

# Drawbacks
[drawbacks]: #drawbacks

- Increase the HD requirements as new data will be saved.
- Increase IBD as we add computation on each upcoming block.