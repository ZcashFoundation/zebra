# Forking the Zcash Testnet with Zebra

The Zcash blockchain community consistently explores upgrades to the Zcash protocol, introducing new features to the consensus layer. This tutorial guides teams or individuals through forking the Zcash Testnet locally using Zebra, enabling testing of custom functionalities in a private testnet environment.

As of writing, the current network upgrade on the Zcash Testnet is `Nu5`. While a future upgrade (`Nu6`) activation height will be known later, for this tutorial, we aim to activate after `Nu5`, allowing us to observe our code crossing the network upgrade and continuing isolated.

To achieve this, we'll use [Zebra](https://github.com/ZcashFoundation/zebra) as the node, [s-nomp](https://github.com/ZcashFoundation/s-nomp) as the mining pool, and [nheqminer](https://github.com/ZcashFoundation/nheqminer) as the Equihash miner.

**Note:** This tutorial aims to remain generally valid after `Nu6`, with adjustments to the network upgrade name and block heights.

# Requirements

- A modified Zebra version capable of syncing up to our chosen activation height, including the changes from the [code changes step](#code-changes).
- Mining tools:
  - s-nomp pool
  - nheqminer

You may have two Zebra versions: one for syncing up to the activation height and another (preferably built on top of the first one) with the network upgrade and additional functionality.

**Note:** For mining setup please see [How to mine with Zebra on testnet](https://zebra.zfnd.org/user/mining-testnet-s-nomp.html)

## Sync the Testnet to a Block after `Nu5` Activation

Select a height for the new network upgrade after `Nu5`. In the Zcash public testnet, `Nu5` activation height is `1_842_420`, and at the time of writing, the testnet was at around block `2_598_958`. To avoid dealing with checkpoints, choose a block that is not only after `Nu5` but also in the future. In this tutorial, we chose block `2_599_958`, which is 1000 blocks ahead of the current testnet tip.

Clone Zebra, create a config file, and use `state.debug_stop_at_height` to halt the Zebra sync after reaching our chosen network upgrade block height (`2_599_958`):

The relevant parts of the config file are:

```
[network]
listen_addr = "0.0.0.0:18233"
network = "Testnet"

[state]
debug_stop_at_height = 2599958
cache_dir = "/home/user/.cache/zebra"
```

Generate a Zebra config file:

```
zebrad generate -o myconf.toml`
```

Start Zebra with the modified config:

```
zebrad -c myconf.toml start
```

Wait for the sync to complete (this may take up to 24 hours, depending on testnet conditions), resulting in a state up to the desired block in `~/cache/zebra`.

## Code changes 

We need to add the network upgrade variant to the `zcash_primitives` crate and Zebra. 


### librustzcash / zcash_primitives

Add the new network upgrade variant and a branch id among some changes needed for the library to compile. Here are some examples:

- Sample internal commit: [Unclean test code](https://github.com/zcash/librustzcash/commit/76d81db22fb4c52302f81c9b3e1d98fb6b71188c)
- Public PR adding Nu6 behind a feature: [librustzcash PR #1048](https://github.com/zcash/librustzcash/pull/1048)

After the changes, check that the library can be built with `cargo build --release`.

## Zebra

Here we are making changes to create an isolated network version of Zebra. In addition to your own changes, this Zebra version needs to have the following:

- Add a `Nu6` variant to the `NetworkUpgrade` enum located in `zebra-chain/src/parameters/network_upgrade.rs`.
- Add consensus branch id, a random non-repeated string. We used `00000006` in our tests when writing this tutorial.
- Point to the modified `zcash_primitives` in `zebra-chain/Cargo.toml`. In my case, I had to replace the dependency line with something like:

  ```
  zcash_primitives = { git = "https://github.com/oxarbitrage/librustzcash", branch = "nu6-test", features = ["transparent-inputs"] }
  ```
- Make fixes needed to compile.
- Ignore how far we are from the tip in get block template: `zebra-rpc/src/methods/get_block_template_rpcs/get_block_template.rs`

Unclean test commit for Zebra: [Zebra commit](https://github.com/ZcashFoundation/zebra/commit/d05af154c897d4820999fcb968b7b62d10b26aa8)

Make sure you can build the `zebrad` binary after the changes with `zebra build --release`

## Configuration for isolated network

Now that you have a synced state and a modified Zebra version, it's time to run your isolated network. Relevant parts of the configuration file:

Relevant parts of the configuration file:

```
[mempool]
debug_enable_at_height = 0
accept_datacarrier = true
max_datacarrier_bytes = 83
    
[mining]
miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'
    
[network]
cache_dir = false
initial_testnet_peers = [
  "dnsseed.testnet.z.cash:18233",
  "testnet.seeder.zfnd.org:18233",
  "testnet.is.yolo.money:18233",
]
listen_addr = "0.0.0.0:18233"
network = "Testnet"
    
[rpc]
listen_addr = "0.0.0.0:18232"
    
[state]
cache_dir = "/home/oxarbitrage/.cache/zebra"
```

- `debug_enable_at_height= 0` enables the mempool independently of the tip height.
- The `[mining]` section is necessary for mining blocks, and the rpc endpoint `rpc.listen_addr` too.
- `initial_testnet_peers` is needed as Zebra starts behind the fork block, approximately 100 blocks behind, so it needs to receive those blocks again. This is necessary until the new fork passes more than 100 blocks after the fork height. At that point, this network can be isolated, and initial_testnet_peers can be set to `[]`.
- Ensure your `state.cache_dir` is the same as when you saved state in step 1.

Start the chain with:

```
zebrad -c myconf.toml start
```

Start s-nomp:

```
npm start
```

Start the miner:

```
nheqminer -l 127.0.0.1:1234 -u tmRGc4CD1UyUdbSJmTUzcB6oDqk4qUaHnnh.worker1 -t 1
```

## Confirm Forked Chain

After Zebra retrieves blocks up to your activation height from the network, the network upgrade will change, and no more valid blocks could be received from outside.

After a while, in s-nomp, you should see submitted blocks from time to time after the fork height.

```
...
2023-11-24 16:32:05 [Pool]        [zcash_testnet] (Thread 1) Block notification via RPC after block submission
2023-11-24 16:32:24 [Pool]        [zcash_testnet] (Thread 1) Submitted Block using submitblock successfully to daemon instance(s)
2023-11-24 16:32:24 [Pool]        [zcash_testnet] (Thread 1) Block found: 0049f2daaaf9e90cd8b17041de0a47350e6811c2d0c9b0aed9420e91351abe43 by tmRGc4CD1UyUdbSJmTUzcB6oDqk4qUaHnnh.worker1
2023-11-24 16:32:24 [Pool]        [zcash_testnet] (Thread 1) Block notification 
...
```

You'll also see this in Zebra:

```
...
2023-11-24T19:32:05.574715Z  INFO zebra_rpc::methods::get_block_template_rpcs: submit block accepted block_hash=block::Hash("0084e1df2369a1fd5f75ab2b8b24472c49812669c812c7d528b0f8f88a798578") block_height="2599968"
2023-11-24T19:32:24.661758Z  INFO zebra_rpc::methods::get_block_template_rpcs: submit block accepted block_hash=block::Hash("0049f2daaaf9e90cd8b17041de0a47350e6811c2d0c9b0aed9420e91351abe43") block_height="2599969"
...
```

Ignore messages in Zebra related to how far you are from the tip or network/system clock issues, etc.

Check that you are in the right branch with the curl command:

```
curl --silent --data-binary '{"jsonrpc": "1.0", "id":"curltest", "method": "getblockchaininfo", "params": [] }' -H 'Content-type: application/json' http://127.0.0.1:18232/ | jq
```

In the result, verify the tip of the chain is after your activation height for `Nu6` and that you are in branch `00000006` as expected.


## Final words

Next steps depend on your use case. You might want to submit transactions with new fields, accept those transactions as part of new blocks in the forked chain, or observe changes at activation without sending transactions. Further actions are not covered in this tutorial.
