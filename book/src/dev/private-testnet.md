# Private Testnet Test

The objective of a private Testnet test is to test Testnet activation of an upcoming
network upgrade in an isolated fashion, before the actual Testnet activation. 
It is usually done using the current state of the existing Testnet. For NU6, it was done
by ZF and ECC engineers over a call.

## Steps

### Make Backup

Make a backup of your current Testnet state. Rename/copy the `testnet` folder in
Zebra's state cache directory to the lowercase version of the configured network name,
or the default `unknowntestnet` if no network name is explicitly configured.

### Set Protocol Version

Double check that Zebra has bumped its protocol version.

### Set Up Lightwalletd Server

It's a good idea to set up a lightwalletd server connected to your node, and
have a (Testnet) wallet connected to your lightwalletd server.

### Connect to Peers

Make sure everyone can connect to each other. You can **use Tailscale** to do
that. Everyone needs to send invites to everyone else. Note that being able to
access someone's node does not imply that they can access yours, it needs to be
enabled both ways.

### Choose an Activation Height

Choose an activation height with the other participants. It should be in
the near future, but with enough time for people to set things up; something
like 30 minutes in the future?

### Ensure the Activation Height is Set in Code

While Zebra allows creating a private Testnet in the config file, the height is
also set in some librustzcash crates. For this reason, someone will need to
**create a branch of librustzcash** with the chosen height set and you will need
to **change Zebra to use that**. However, double check if that's still
necessary.

### Configure Zebra to use a custom testnet

See sample config file below. The critical part is setting the activation
height. It is good to enable verbose logging to help debug things. Some of the
participants must enable mining also. It's not a huge deal to keep the DNS
seeders; the blockchain will fork when the activation happens and only the
participants will stay connected. On the other hand, if you want to ensure you
won't connect to anyone else, set `cache_dir = false` in the `[network]` section
and delete the peers file (`~/.cache/zebra/network/unknowntestnet.peers`).

### Run Nodes

Everyone runs their nodes, and checks if they connect to other nodes. You can use
e.g. `curl --data-binary '{"jsonrpc": "1.0", "id":"curltest", "method":
"getpeerinfo", "params": [] }' -H 'Content-Type: application/json'
http://127.0.0.1:8232` to check that. See "Getting Peers" section below.

### Wait Until Activation Happens

And monitor logs for behaviour.

### Do Tests

Do tests, including sending transactions if possible (which will require the
lightwalletd server). Check if whatever activated in the upgrade works.


## Zebra

Relevant information about Zebra for the testing process.

### Getting peers

It seems Zebra is not very reliable at returning its currently connected peers;
you can use `getpeerinfo` RPC as above or check the peers file
(`~/.cache/zebra/network/unknowntestnet.peers`) if `cache_dir = true` in the
`[network]` section. You might want to sort this out before the next private
testnet test.

### Unredact IPs

Zebra redacts IPs when logging for privacy reasons. However, for a test like
this it can be annoying. You can disable that by editing `peer_addr.rs`
with something like


```diff
--- a/zebra-network/src/meta_addr/peer_addr.rs
+++ b/zebra-network/src/meta_addr/peer_addr.rs
@@ -30,7 +30,7 @@ impl fmt::Display for PeerSocketAddr {
         let ip_version = if self.is_ipv4() { "v4" } else { "v6" };

         // The port is usually not sensitive, and it's useful for debugging.
-        f.pad(&format!("{}redacted:{}", ip_version, self.port()))
+        f.pad(&format!("{}:{}", self.ip(), self.port()))
     }
 }
```

### Sample config file

Note: Zebra's db path will end in "unknowntestnet" instead of "testnet" with
this configuration.

```
[consensus]
checkpoint_sync = true

[mempool]
eviction_memory_time = "1h"
tx_cost_limit = 80000000
max_datacarrier_bytes = 83

[metrics]

[mining]
miner_address = "t27eWDgjFYJGVXmzrXeVjnb5J3uXDM9xH9v"
# if you want to enable mining, which also requires selecting the `internal-miner` compilation feature
internal_miner = true

[network]
# This will save peers to a file. Take care that it also reads peers from it;
# if you want to be truly isolated and only connect to the other participants,
# either disable this or delete the peers file before starting.
cache_dir = true
crawl_new_peer_interval = "1m 1s"

initial_mainnet_peers = []

initial_testnet_peers = [
    # List the other participant's Tailscale IPs here.
    # You can also keep the default DNS seeders if you wish.
    "100.10.0.1:18233",
]

listen_addr = "0.0.0.0:18233"
max_connections_per_ip = 1
network = "Testnet"
peerset_initial_target_size = 25

[network.testnet_parameters]

[network.testnet_parameters.activation_heights]
BeforeOverwinter = 1
Overwinter = 207_500
Sapling = 280_000
Blossom = 584_000
Heartwood = 903_800
Canopy = 1_028_500
NU5 = 1_842_420
NU6 = 2_969_920

[rpc]
debug_force_finished_sync = false
parallel_cpu_threads = 0
listen_addr = "127.0.0.1:8232"
indexer_listen_addr = "127.0.0.1:8231"

[state]
delete_old_database = true
ephemeral = false

[sync]
checkpoint_verify_concurrency_limit = 1000
download_concurrency_limit = 50
full_verify_concurrency_limit = 20
parallel_cpu_threads = 0

[tracing]
buffer_limit = 128000
force_use_color = false
use_color = true
use_journald = false
# This enables debug network logging. It can be useful but it's very verbose!
filter = 'info,zebra_network=debug'
```
