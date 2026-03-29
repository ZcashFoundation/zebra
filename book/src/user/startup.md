# Zebra Startup & Sync Process

When Zebra starts up, it launches [a lot of concurrent tasks](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/commands/start.rs).

## Grafana Dashboards

You can see what Zebra is doing during the sync using its [Grafana dashboards](metrics.md).

## Example Startup Logs

Zebra logs the progress of each of its major startup tasks.

In this section, we re-ordered some logs, trimmed (`...`) verbose parts of the logs, and re-formatted some long lines.

(The order of the logs or tasks might be different in each Zebra run, because these tasks are run concurrently.)

### Load Configuration

Zebra loads its configuration from its config file.

Some important parts of the config are:

- `consensus.checkpoint_sync`: use all the checkpoints, or just use the mandatory ones
- `network.listen_addr`: the Zcash listener port
- `network.network`: the configured Zcash network (mainnet or testnet)
- `network.initial_mainnet_peers`: the initial DNS seeder or fixed address peers
- `network.peerset_initial_target_size`: the initial target size of the peer set, also used to calculate connection limits
- `state.cache_dir`: where the cached state is stored on disk
- `rpc.listen_addr`: optional JSON-RPC listener port

See [the full list of configuration options](https://docs.rs/zebrad/latest/zebrad/config/struct.ZebradConfig.html).

```text
zebrad::commands::start: Starting zebrad
zebrad::commands::start: config=ZebradConfig {
  consensus: Config { checkpoint_sync: true, ... },
  network: Config {
    listen_addr: 127.0.0.1:8233,
    network: Mainnet,
    initial_mainnet_peers: {
      "dnsseed.z.cash:8233",
      "mainnet.is.yolo.money:8233",
      "mainnet.seeder.zfnd.org:8233",
      "dnsseed.str4d.xyz:8233"
    },
    peerset_initial_target_size: 25, ...
  },
  state: Config { cache_dir: "/home/user/.cache/state/v24/mainnet", ... },
  rpc: Config { listen_addr: Some(127.0.0.1:8232) }, ...
}
```

### Open Cached State

Zebra opens the configured cached state, or creates a new empty state.

```text
zebrad::commands::start: initializing node state
zebra_state::service::finalized_state::disk_db: the open file limit is high enough for Zebra current_limit=1048576 min_limit=512 ideal_limit=1024
zebra_state::service::finalized_state::disk_db: Opened Zebra state cache at /home/user/.cache/state/v24/mainnet
zebra_state::service::finalized_state: loaded Zebra state cache tip=None
zebra_state::service: created new read-only state service
...
```

### Peer Handling

#### Discover Initial Peers

Zebra queries DNS seeders for initial peer IP addresses, then tries to connect to `network.peerset_initial_target_size` initial peers.

```text
zebrad::commands::start: initializing network
open_listener{addr=127.0.0.1:8233}: zebra_network::peer_set::initialize: Trying to open Zcash protocol endpoint at 127.0.0.1:8233...
open_listener{addr=127.0.0.1:8233}: zebra_network::peer_set::initialize: Opened Zcash protocol endpoint at 127.0.0.1:8233
zebra_network::address_book_updater: starting the address book updater
add_initial_peers: zebra_network::config: resolved seed peer IP addresses seed="mainnet.seeder.zfnd.org:8233" remote_ip_count=25
...
add_initial_peers: zebra_network::peer_set::initialize: limiting the initial peers list from 112 to 25
```

**DNS Seeder**: A DNS server which returns IP addresses of full nodes on the Zcash network to assist in peer discovery. [Zcash Foundation dnsseeder](https://github.com/ZcashFoundation/dnsseeder)

#### Connect to Initial Peers

Zebra connects to the initial peers, and starts an ongoing task to crawl for more peers. If Zebra is making a lot of requests, it opens new connections to peers, up to the connection limit.

It also starts a service that responds to Zcash network queries from remote peers.

```text
add_initial_peers: zebra_network::peer_set::initialize: connecting to initial peer set initial_peer_count=25 initial_peers={[2a01:4f9:c010:7391::1]:8233, 202.61.207.45:8233, ...}
add_initial_peers: zebra_network::peer_set::initialize: an initial peer connection failed successes=5 errors=8 addr=89.58.36.182:8233 e=Connection refused (os error 111)
...
add_initial_peers: zebra_network::peer_set::initialize: finished connecting to initial seed peers handshake_success_total=11 handshake_error_total=14 outbound_connections=11
zebra_network::peer_set::initialize: sending initial request for peers active_initial_peer_count=11
crawl_and_dial: zebra_network::peer_set::initialize: starting the peer address crawler crawl_new_peer_interval=30s outbound_connections=11
```

#### Requests to Peers

Zebra randomly chooses peers for each request:

- choosing peers that are not already answering a request, and
- prefers peers that have answered fewer requests recently.

This helps load balance requests across peers.

### Initialize JSON-RPC Server

Applications like `lightwalletd` can query and send data to Zebra using JSON-RPC.

Submitted transactions are retried a few times, using an ongoing retry task.

The RPC service is optional, if it is not configured, its tasks do not run.

```text
zebra_rpc::server: Trying to open RPC endpoint at 127.0.0.1:57638...
zebra_rpc::server: Opened RPC endpoint at 127.0.0.1:57638
```

### Initialize Verifiers

Zebra verifies blocks and transactions using these services, which depend on the Sprout and Sapling parameters. If the parameters have not been cached, downloading and verifying them can take a few minutes.

Zebra has two different verification modes for blocks:

- checkpoint: check that the block hashes between checkpoints form a chain
- full verification: check signatures, proofs, spent transparent outputs, and all the other consensus rules

Mempool transactions are always fully verified.

Zebra also starts ongoing tasks to batch verify signatures and proofs.

```text
zebrad::commands::start: initializing verifiers
init{config=Config { ... } ... }: zebra_consensus::primitives::groth16::params: checking and loading Zcash Sapling and Sprout parameters
init{config=Config { checkpoint_sync: true, ... } ... }: zebra_consensus::chain: initializing chain verifier tip=None max_checkpoint_height=Height(1644839)
...
```

### Initialize Transaction Mempool

Zebra syncs transactions from other peers, verifies them, then gossips their hashes.

This involves a number of ongoing tasks.

The mempool isn't activated until Zebra reaches the network consensus chain tip.

```text
zebrad::commands::start: initializing mempool
zebrad::components::mempool::crawler: initializing mempool crawler task
zebrad::components::mempool::queue_checker: initializing mempool queue checker task
zebrad::components::mempool::gossip: initializing transaction gossip task
```

### Syncing the Block Chain

#### Initialize Block Syncer

Zebra syncs blocks from other peers, verifies them, then gossips their hashes:

1. Download the genesis block by hash
2. Ask a few peers for the hashes of the next 500 blocks
3. Download the blocks based on the hashes from peers
4. Verify, save, and gossip the downloaded blocks
5. Repeat from step 2 until we run out of blocks to sync

This involves a number of ongoing tasks.

```text
zebrad::commands::start: initializing syncer
zebrad::components::sync::gossip: initializing block gossip task
```

#### Sync to the Chain Tip

Starting at the cached state chain tip, Zebra syncs to the network consensus chain tip, using ongoing tasks.

Zebra also has an ongoing sync progress task, which logs progress towards the tip every minute.

```text
zebrad::commands::start: spawned initial Zebra tasks
zebrad::components::sync: starting genesis block download and verify
zebrad::commands::start: initial sync is waiting to download the genesis block sync_percent=0.000 % current_height=None
sync:checkpoint: zebra_consensus::checkpoint: verified checkpoint range block_count=1 current_range=(Unbounded, Included(Height(0)))
sync:try_to_sync: zebrad::components::sync: starting sync, obtaining new tips state_tip=Some(Height(0))
sync:try_to_sync: zebrad::components::sync: extending tips tips.len=1 in_flight=499 lookahead_limit=2000 state_tip=Some(Height(0))
sync:try_to_sync:obtain_tips:checkpoint: zebra_consensus::checkpoint: verified checkpoint range block_count=400 current_range=(Excluded(Height(0)), Included(Height(400)))
...
zebrad::commands::start: estimated progress to chain tip sync_percent=0.537 % current_height=Height(9119) remaining_sync_blocks=1687657 time_since_last_state_block=PT0S
...
```

#### Handling Gossiped Block Hashes

Zebra's peer request service downloads gossiped block hashes, then verifies, saves, and gossips their hashes itself.

This handles small numbers of new blocks near the chain tip.
