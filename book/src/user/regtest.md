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

### Experimental early block relay

For private relay experiments, set `experimental_early_block_relay = true` in
`[network]` on each participating Zebra node. It defaults to `false` and requires
both ends of a connection to advertise the experimental capability during the
handshake. Zakura and non-opted-in Zebra peers retain ordinary full-block relay.

After NU5, opted-in nodes can advertise and serve an authenticated child of the
committed best tip before transaction verification finishes. Full validation is
still required before committing it. The cache retains one body for at most
15 seconds, with a 2 MB wire limit and a conservative 64 MiB decoded-content
charge. Rejection, cancellation, or expiry withdraws the body; after withdrawal,
the admission slot stays reserved until its deadline or the next committed tip
to bound replayed advertisements. Locally invalidated hashes are ineligible.
At most eight authenticated, deterministic rejection hashes are remembered for
60 seconds to suppress repeated expensive verification. Unauthenticated variants,
timeouts, and infrastructure failures do not populate this set. Ordinary
committed announcements are unchanged. Compatible peers do not punish one another
for later transaction validation failures, but still reject malformed or
unauthenticated bodies.

These connections also negotiate provisional compact encoding version 1 with
`sendcmpct`. At most three recipients per announcement receive `cmpctblock`;
other connections receive ordinary inventory and full blocks. The compact
encoding includes the complete Zcash header, a nonce, six-byte SipHash-2-4 short
IDs, and a prefilled coinbase. Short IDs use the complete witnessed identity for
v5/v6 transactions and the transaction ID for earlier versions.

Reconstruction uses the verified mempool, then one `getblocktxn`/`blocktxn`
exchange for missing transactions. Collisions, commitment mismatches, unknown
parents, and a two-second recovery timeout fall back to a full block without
punishing the announcing peer. Retained bodies expire after 15 seconds and are
globally bounded; rejected early bodies are no longer served. The normal
consensus verifier still checks every reconstructed block. Coinbase-only blocks
use full relay because compact encoding would not save bytes.

This uses experimental service bit 25 from the
[temporary-experiment range inherited from Bitcoin](https://github.com/zcash/zcash/blob/16ac743764a513e41dafb2cd79c2417c5bb41e81/src/protocol.h#L77-L83).
[Zakura v1.7.0](https://github.com/zakura-core/zakura/blob/0ca01e6a46d711f8b46fb64742048bb4911a7052/zakura-network/src/protocol/external/types.rs#L159)
uses bit 24 for `NODE_P2P_V2`, not early relay; experimental assignments
must remain distinct from Zakura's capabilities. This is not a production protocol
allocation: use it only with explicitly coordinated participants. Regtest's disabled
proof of work does not measure the cost of authenticated admission on a public network.

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

Zebra should now mine blocks on Regtest when it starts after a short delay (of around 30 seconds).

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
