# Regtest with Zebra

The Regtest network in Zebra enables testing of custom functionalities in a private testnet environment with configurable network upgrade activation heights. It allows for starting an isolated node which won't connect to any peers and currently allows for committing blocks without validating their Proof of Work (in the future, it may use a very low target difficulty and easier Equihash parameters instead of skipping Proof of Work validation altogether).

Zebra always activates the Canopy network upgrade at block height 1 due to limitations on its block construction.

In order to use Regtest, Zebra must be configured to run on the Regtest network. The `[mining]` section is also necessary for mining blocks, and the `[rpc]` section is necessary for using the `send_raw_transaction` RPC method to mine non-coinbase transactions onto the chain.

Relevant parts of the configuration file:

```toml
[mining]
miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'
    
[network]
network = "Regtest"

// This section may be omitted when testing only Canopy
[network.testnet_parameters.activation_heights]
// Configured activation heights must be greater than or equal to 1, 
// block height 0 is reserved for the Genesis network upgrade in Zebra
NU5 = 1 

// This section may be omitted if a persistent Regtest chain state is desired
[state]
ephemeral = true

// This section may be omitted if it's not necessary to send transactions to Zebra's mempool
[rpc]
listen_addr = "0.0.0.0:18232"
```

There are two ways to commit blocks to Zebra's state on Regtest:
- Using the `getblocktemplate` and `submitblock` RPC methods directly
- Using Zebra's experimental `internal-miner` feature

## Using Zebra's Internal Miner

Zebra can mine blocks on the Regtest network when compiled with the experimental `internal-miner` compilation feature and configured to enable to internal miner.

Compile Zebra with `cargo build --features "internal-miner"` and add `internal_miner = true` in the mining section of its configuration to use the internal miner with Regtest:

```toml
[mining]
internal_miner = true
```

## Using RPC methods directly

Blocks could also be mined outside of Zebra and submitted via Zebra's RPC methods. This requires enabling the RPC server in the configuration by providing a `listen_addr` field:

```toml
[rpc]
listen_addr = "0.0.0.0:18232"
```

With Proof of Work disabled on Regtest, block templates can be converted directly into blocks with the `proposal_block_from_template()` function in the `zebra-chain` crate, serialized, hex-encoded, and then submitted via the `submitblock` RPC method.

When Proof of Work validation is enabled for Regtest with a low target difficulty and easy Equihash parameters, Zebra may have a `network.testnet_parameters.disable_pow` field in its configuration so that this would continue working.

See the `regtest_submit_blocks()` acceptance test as an example for using Zebra's RPC methods to submit blocks on Regtest.