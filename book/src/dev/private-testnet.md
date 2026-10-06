# Private Testnet Test

The objective of a private Testnet test is to test Testnet activation of an upcoming
network upgrade in an isolated fashion, before the actual Testnet activation.
It is usually done using the current state of the existing Testnet. For NU6, it was done
by ZF and ECC engineers over a call.

## Steps

### Make Backup

Choose a public Testnet snapshot whose blocks are all below the first height at
which the private network's consensus rules differ. Keep the public genesis and
all consensus parameters through that snapshot unchanged, including earlier
activation heights. Agree on the starting tip height and hash with the other
participants, then stop Zebra before copying its state.

Keep an untouched backup of the public state. Copy, rather than move, the
finalized database and its matching non-finalized backup, if present, into the
private network's namespace, using an empty destination rather than merging
with state from an earlier test. A compatible public Testnet prefix can be
reused; changing only the name and wire magic does not require a full resync.

Custom storage names are `<lowercase-name>-<eight-digit-hex-magic>`. The sample
below uses `Nu7Private` and `[0, 1, 0, 7]`, giving `nu7private-00010007`. Under
`state.cache_dir`, copy:

| Public source | Private destination |
| --- | --- |
| `state/vN/testnet` | `state/vN/nu7private-00010007` |
| `non_finalized_state/testnet` | `non_finalized_state/nu7private-00010007` |

Use the actual database version `N` supported by the test binary. The NU7/NSM
database uses `v29`. Prepare the public snapshot with a v29-compatible writer
before forking: its registered upgrade automatically moves compatible v28 state
into `state/v29` when no v29 database exists, without a public Testnet resync.
Legacy records remain readable with a zero NSM balance; new writes use the wider
layout. Stop Zebra again before copying the upgraded snapshot, and keep the
snapshot and any copied non-finalized blocks below the private activation.

Retain an untouched v28 backup before upgrading if rollback is needed.
`state.delete_old_database = false` does not preserve the directory that the
upgrade moves. Manually renaming or symlinking versioned directories is not a
substitute for the supported upgrade. Upgrade direct database readers, including
Zallet and Zaino backends, with the writer as described in
[State Database Upgrades](state-db-upgrades.md#upgrading-the-state-database).

On Linux the default cache root is `$XDG_CACHE_HOME/zebra`, or
`~/.cache/zebra` if that variable is unset. If `state.cache_dir` is configured,
use that root instead. Do not copy the public peer cache into the private
namespace, and never copy private fork state back into public Testnet.

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
like 30 minutes in the future? For NU7, choose a height divisible by three, as
required by ZIP 259.

### Check Activation Heights Across Implementations

Zebra passes its configured activation heights to librustzcash through its
network parameters. Check that the other participating nodes and wallet SDKs
also use the chosen private height rather than hard-coded public Testnet
parameters. If an implementation does not support custom heights, use a test
branch with the agreed height for that implementation.

### Configure Zebra to use a custom testnet

See the sample config below and [Custom Testnets](../user/custom-testnets.md).
All participants must use the same custom name, distinct network magic, and
activation heights. Custom consensus rules with public Testnet magic are
rejected, even if the peer list contains only private addresses.

List only the other participants in `initial_testnet_peers`; do not keep public
DNS seeders. The sample disables peer caching with `network.cache_dir = false`.
If enabling it, start with an empty private cache at
`<network.cache_dir>/network/nu7private-00010007.peers`, not public
`testnet.peers`. Restrict inbound access to the participants using your firewall
or Tailscale policy; wire magic is not authentication.

Keep historical consensus settings compatible with the copied snapshot. Supply
checkpoints on the shared prefix, including genesis and coverage through at least
Canopy minus one (1,028,499 for this sample), but none at or after the fork.
From the Zebra source checkout, prepare the sample's checkpoint file:

```sh
awk '$1 < 4200000' zebra-chain/src/parameters/checkpoint/test-checkpoints.txt > nu7-private-checkpoints.txt
```

Replace `4200000` with the agreed fork height. Share the file with all
participants, and run Zebra from the directory containing it or configure its
absolute path. `checkpoints = true` is safe only if every bundled checkpoint is
before the fork. Enable verbose logging to help debug the test, and enable
mining on some participants.

### Run Nodes

Everyone runs their nodes, and checks if they connect to other nodes. You can use
e.g. `curl --data-binary '{"jsonrpc": "1.0", "id":"curltest", "method":
"getpeerinfo", "params": [] }' -H 'Content-Type: application/json'
http://127.0.0.1:8232` to check that. See "Getting Peers" section below.

PoW-enabled private Testnets must finish synchronizing with their peers before
`getblocktemplate` or the internal miner will produce work. Unlike Mainnet, they
do not require a tip less than 125 minutes old: once synchronized, participants
can resume mining from an old snapshot even if setup took longer than that.
`rpc.debug_force_finished_sync` only changes `getblockchaininfo`; it does not
bypass mining synchronization. Keep the participants on the same agreed tip
and check peer connectivity if mining still reports that Zebra is not synced.

### Wait Until Activation Happens

And monitor logs for behaviour.

### Do Tests

Do tests, including sending transactions if possible (which will require the
lightwalletd server). Check if whatever activated in the upgrade works.

## Zebra

Relevant information about Zebra for the testing process.

### Getting peers

Use `getpeerinfo` as above to inspect connected peers. The sample disables peer
caching, so it does not write a peers file. If `network.cache_dir = true`, the
default Linux peer cache is
`~/.cache/zebra/network/nu7private-00010007.peers` (or under
`$XDG_CACHE_HOME/zebra` when set). A custom `network.cache_dir` replaces that
root. The file contains cached addresses, not a list of current connections.

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

Replace the example NU7 height with the agreed future height; it is not a
scheduled public activation. With this name and magic, the database path ends
in `state/vN/nu7private-00010007` and the non-finalized backup path ends in
`non_finalized_state/nu7private-00010007`. Keep old database deletion disabled
while preparing and running the test so that backups are not removed.

The sample carries public Testnet's historical reserve seed into the copied
chain. With NU7 at 4,200,000, ZIP 237 derives reissuance height 7,835,274.
For an accelerated test, set `nsm_reissuance_height` explicitly and agree
on that override across all participants.

```toml
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
# Only use explicitly configured participants; do not load a previous peer cache.
cache_dir = false
crawl_new_peer_interval = "1m 1s"

initial_mainnet_peers = []

initial_testnet_peers = [
    # List the other participant's Tailscale IPs here.
    "100.64.0.1:18233",
]

listen_addr = "0.0.0.0:18233"
max_connections_per_ip = 1
network = "Testnet"
peerset_initial_target_size = 25

[network.testnet_parameters]
network_name = "Nu7Private"
# Use the same distinct magic on every participant; do not use public Testnet magic.
network_magic = [0, 1, 0, 7]
checkpoints = "nu7-private-checkpoints.txt"
initial_nsm_value_balance = 55_768_414_957

[network.testnet_parameters.activation_heights]
BeforeOverwinter = 1
Overwinter = 207_500
Sapling = 280_000
Blossom = 584_000
Heartwood = 903_800
Canopy = 1_028_500
NU5 = 1_842_420
NU6 = 2_976_000
"NU6.1" = 3_536_500
"NU6.2" = 4_052_000
"NU6.3" = 4_134_000
# NU7 has no scheduled activation height yet; this is an example value.
NU7 = 4_200_000

[rpc]
debug_force_finished_sync = false
parallel_cpu_threads = 0
listen_addr = "127.0.0.1:8232"
indexer_listen_addr = "127.0.0.1:8231"

[state]
delete_old_database = false
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
