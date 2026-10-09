# Regtest with Zebra

The Regtest network in Zebra enables testing of custom functionalities in a private Testnet environment with configurable network upgrade activation heights. It allows for starting an isolated node which won't connect to any peers and currently allows for committing blocks without validating their Proof of Work (in the future, it may use a very low target difficulty and easier Equihash parameters instead of skipping Proof of Work validation altogether).

By default, Zebra activates network upgrades at height 1 on Regtest, but activation heights are configurable via the `[network.testnet_parameters.activation_heights]` section. Block height 0 is reserved for the Genesis network upgrade.

## Usage

In order to use Regtest, Zebra must be configured to run on the Regtest network. The `[mining]` section is also necessary for mining blocks, and the `[rpc]` section is necessary for using the `send_raw_transaction` RPC method to mine non-coinbase transactions onto the chain.

Relevant parts of the configuration file:

```toml
[mining]
miner_address = 't27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v'

[network]
network = "Regtest"

# This section may be omitted when testing only Canopy
[network.testnet_parameters.activation_heights]
# Configured activation heights must be greater than or equal to 1,
# block height 0 is reserved for the Genesis network upgrade in Zebra
NU5 = 1

# This section may be omitted if a persistent Regtest chain state is desired
[state]
ephemeral = true

# This section may be omitted if it's not necessary to send transactions to Zebra's mempool
[rpc]
listen_addr = "0.0.0.0:18232"
```

Zebra should now include the Regtest network name in its logs, for example:

```console
...  INFO {zebrad="..." net="Regtest"}: zebrad::commands::start: initializing mempool
```

There are two ways to commit blocks to Zebra's state on Regtest:

- Using the `getblocktemplate` and `submitblock` RPC methods directly
- Using Zebra's experimental `internal-miner` feature

### Testing NU7 reissuance

Use a fresh test chain and replace the activation-height section above with:

```toml
[network.testnet_parameters]
initial_nsm_value_balance = 100000000
nsm_reissuance_height = 12

[network.testnet_parameters.activation_heights]
Canopy = 1
NU5 = 2
NU7 = 9
```

The reissuance height must be at least 1, no earlier than NU7 activation, and less
than 2^31. Omitting it uses ZIP 237's scheduled-issuance crossover, which short
Regtest schedules usually never reach, so set it explicitly to test reissuance.
These are example consensus parameters, not public Mainnet or Testnet activation
heights.

At NU7 activation, the NSM reserve receives the explicitly configured initial
balance in zatoshis; it is not derived from historical issued supply. From that
block onward, 60% of aggregate transaction fees, rounded down, also enters the
reserve. The reserve is excluded from issued supply.

Starting at the configured deployment height, mining templates and
`getblocksubsidy` include the additional subsidy calculated from the parent
block's reserve. `getblocksubsidy` rejects a future reissuance height whose parent
block is not yet in the best chain.

Ordinary builds use database format v29.0.0, including the NSM balance in each
new value-pool record. The registered upgrade automatically moves compatible v28
state into `state/v29` when no v29 database exists, without resyncing. Legacy
48-byte value-pool and 52-byte block-info records remain readable with a zero NSM
balance; new writes use 56 and 60 bytes.

Before upgrading, stop Zebra and direct database readers and retain a v28 backup
for rollback. Disabling `state.delete_old_database` does not preserve the
directory that the upgrade moves. Upgrade direct database readers, including
Zallet's Zebra backend and Zaino's Zebra read-state backend, together with the
node: their `zebra-state` dependency must support v29. Start the writer before
compatible readers. A v28 reader cannot read the wider records written by v29;
manually renaming or symlinking versioned directories is not a substitute for
the supported upgrade.

Format reuse does not fix old custom-network accounting: existing state with
nonzero genesis transparent outputs must still be rebuilt, including experimental
v29 state created with the old accounting. Rebuild custom state if consensus
parameters change for blocks already in the cache.

### Using Zebra's Internal Miner

Zebra can mine blocks on the Regtest network when compiled with the experimental `internal-miner` compilation feature and configured to enable to internal miner.

Add `internal_miner = true` in the mining section of its configuration and compile Zebra with `cargo build --features "internal-miner"` (or `cargo run --features "internal-miner"` to compile and start Zebra) to use the internal miner with Regtest:

```toml
[mining]
internal_miner = true
```

Zebra mines once genesis is committed and its initial template is ready. Regtest requires neither
public peers nor synchronization, and uses null solutions instead of the production Equihash solver.
The miner waits for each of its submitted blocks to commit before extending it. The
`internal_miner_private_testnet` option is unnecessary on Regtest and never bypasses Mainnet checks.

To confirm that it's working, look for `successfully mined a new block` messages in the logs, or that the tip height is increasing.

### Using RPC methods directly

Blocks could also be mined outside of Zebra and submitted via Zebra's RPC methods. This requires enabling the RPC server in the configuration by providing a `listen_addr` field:

```toml
[rpc]
listen_addr = "0.0.0.0:18232"
```

With Proof of Work disabled on Regtest, block templates can be converted directly into blocks with the `proposal_block_from_template()` function in the `zebra-chain` crate, serialized, hex-encoded, and then submitted via the `submitblock` RPC method.

The `submitblock` RPC method should return `{ "result": null }` for successful block submissions.

For example:

```rust
let client = RpcRequestClient::new(rpc_address);

let block_template: GetBlockTemplate = client
    .json_result_from_call("getblocktemplate", "[]".to_string())
    .await
    .expect("response should be success output with a serialized `GetBlockTemplate`");

let block_data = hex::encode(
    proposal_block_from_template(&block_template, TimeSource::default(), Network::Mainnet)?
        .zcash_serialize_to_vec()?,
);

let submit_block_response = client
    .text_from_call("submitblock", format!(r#"["{block_data}"]"#))
    .await?;

let was_submission_successful = submit_block_response.contains(r#""result":null"#);
```

See the `regtest_submit_blocks()` acceptance test as a more detailed example for using Zebra's RPC methods to submit blocks on Regtest.

Note: Proof of Work validation is currently disabled on Regtest. If PoW validation is enabled in the future with a low target difficulty and easier Equihash parameters, a configuration option may be added to disable it.
