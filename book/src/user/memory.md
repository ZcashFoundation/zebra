# Memory

Zebra's memory use depends on what it is doing: initial sync keeps many blocks in
flight, RocksDB caches and buffers grow with the database, and the mempool and
peer connections grow with the network. The `[resources] memory_budget` setting
tells Zebra how much memory it may use, and Zebra derives the limits of each of
these components from it.

Without a budget, which is the default, Zebra applies no budget and every
component keeps its usual limits.

## Setting a budget

```toml
[resources]
memory_budget = "2 GiB"
```

The budget is `"auto"` or a whole number with a unit: `B`, `KiB`, `MiB` or `GiB`
(binary), or `MB` or `GB` (decimal), in any case, with or without a space. A
number without a unit is rejected. The environment variable
`ZEBRA_RESOURCES__MEMORY_BUDGET="2 GiB"` sets it too, which is convenient in
containers.

`"auto"` budgets half of the memory available to Zebra: the smaller of the
host's memory and the memory limit of Zebra's cgroup (a container or systemd
memory limit). It budgets at least the initial sync minimum and at most 8 GiB,
and warns when it has to raise the budget to the minimum because the host has
little headroom. `"auto"` reads `/proc` and `/sys/fs/cgroup`, so it only works
on Linux; on other platforms, set the budget to a size.

Zebra warns at startup when a budget is more than three quarters of the memory
left after 512 MiB for the host, because the host can then run out of memory
before Zebra reaches its budget.

## How the budget is split

Zebra reserves 384 MiB for costs no limit shrinks (its runtimes, the
verification keys and parameters, and the state at open) and holds back 15 % of
the budget as slack for allocator retention, transient deserialization, and the
soft limits below. The components share the rest:

| Component | Share | Floor | Ceiling | What the share sets |
| --- | --- | --- | --- | --- |
| RocksDB | 35 % | 256 MiB | 4 GiB | 160 MiB of pinned database metadata, then 60 % for memtables (a write buffer manager, with smaller memtables per column family) and 40 % for the block cache |
| Sync pipeline | 25 % | 160 MiB for initial sync, 32 MiB at the tip | 2 GiB | The memory of downloaded blocks that are not yet committed, and the state's queue of out-of-order blocks (64 to 320 MiB) |
| Non-finalized state | 15 % | 64 MiB | 2 GiB | A resident target enforced by early finalization, down to a 100-block rollback floor |
| Network | 10 % | 57 MiB (70 MiB with `v2_checkpoint_sync`) | 512 MiB | `peerset_initial_target_size` and QUIC windows, derived from the connections' buffered-request charges |
| Mempool | 10 % | 25 MiB | 200 MiB | `tx_cost_limit`, between 10 and 80 million |
| RPC | 5 % | none | 512 MiB | The block proposal cache of `getblocktemplate` |

A component below its floor is raised to it, and the others give up memory in
proportion to their headroom. A component above its ceiling gives its surplus to
RocksDB and the sync pipeline. A node that mines (`[mining] miner_address` or
`internal_miner`) moves 5 % from the sync pipeline to the RPC proposal cache.

With a budget, the writer finalizes best-chain roots while their distinct
retained charge is above the non-finalized target, until it reaches the target
or 100 blocks remain. Without a budget, it finalizes only past the usual
1,000-block reorg limit. This is early finalization, not body eviction or a
spill store. If 100 blocks alone exceed the target, the target remains
exceeded.

The network charge includes each legacy connection's maximum message
reservation, inbound or outbound, two largest `get-tx` request bodies per
version 2 connection, and every version 2 connection's QUIC receive and send
windows. The largest peer target whose charge fits is selected. Compact QUIC
windows are upgraded only when the same target also fits with the usual
windows, which are the larger serving windows on a version 2 node. These are
buffer charges, not a bound on TLS state or total network memory use.

With `[sync] v2_checkpoint_sync`, the checkpoint sync engine downloads blocks
instead of the legacy syncer while it runs, so a budget also limits its
`v2_lookahead_bytes` to the sync pipeline's share. Peers it streams from split
that lookahead between their QUIC receive windows, and other peers keep the
windows the network share charges. The engine streams from at least two peers,
so Zebra opens at least two outbound version 2 connections for it whatever the
peer target, and the network share always pays for both: its floor rises from
57 MiB to 70 MiB, and the engine's lookahead counts toward the sync pipeline's
usage.

Zebra logs the derived limits once at startup, starting with `memory budget`.

## Profiles

Zebra uses one of two profiles:

- **Initial sync** (`ibd`): the sync pipeline gets a large share. Its floor is
  one checkpoint range of blocks, which Zebra always admits so that the sync
  never stalls.
- **At the tip** (`tip_only`): the sync pipeline only holds a few
  full-verification blocks, and its share goes to the other components.

A budget of at least **1,113 MiB** opens the node with the initial sync profile.
When the node reaches the last checkpoint, Zebra switches to the tip profile and
resizes the non-finalized target and RocksDB's memtables and block cache without
a restart.

A budget between **963 MiB** and 1,113 MiB only fits a node at the tip: an
already synced node, or one restored from a [state snapshot](snapshots.md). If
the state is below the last checkpoint, Zebra refuses to start with
`IBD needs >= 1,113 MiB: sync from a snapshot or raise the budget`.

Below 963 MiB, Zebra refuses to start, and lists each component whose floor the
budget does not cover. `zebrad start --unsafe-low-specs` starts anyway, with
every component at its floor, and with initial sync at a tip-only budget: Zebra
then uses more memory than its budget.

With `[sync] v2_checkpoint_sync`, the larger network floor raises these
minimums to **1,129 MiB** for initial sync and **978 MiB** at the tip.

These minimums are derived from the floors, the 384 MiB reserve and the slack,
so they change if those do.

Snapshot bootstrap also uses the plan before dialing any peers. Half of its
pipeline share bounds downloaded-piece reservations and half bounds the full
pieces read by concurrent SST builds; the network share caps the bootstrap
connections. Snapshot installation still finishes before the state opens.

## What each budget gives up

The shapes below are the derived limits. The memory use is **predicted**: it has
not yet been measured on Mainnet for every budget. The 1 GiB budget is
**experimental**.

| Budget | Profile | Derived limits | Gives up | Predicted memory use |
| --- | --- | --- | --- | --- |
| 1 GiB | At the tip only | RocksDB 256 MiB (64 MiB memtables, 32 MiB cache), pipeline 32 MiB, 3 peer targets without v2 or 1 with compact v2 windows, mempool cost 19 million, 16 MiB proposal cache | Initial sync (sync from a snapshot); address-index RPCs miss the small cache often; fewer peers; no room to cache a full-size block proposal | 0.8 to 1.05 GiB at the tip |
| 2 GiB | Initial sync | RocksDB 474 MiB (188 MiB memtables, 125 MiB cache), pipeline 339 MiB, 8 peer targets without v2 or 1 with serving v2 windows (2 outbound v2 peers with `v2_checkpoint_sync`), mempool cost 57 million, 67 MiB proposal cache | Fewer large blocks in flight during initial sync, which can make it slower on fast links; fewer peers; small memtables | At most 1.9 GiB during initial sync, about 1.2 GiB at the tip |
| 4 GiB | Initial sync, then at the tip | RocksDB 1,150 MiB, pipeline 818 MiB, 19 peer targets without v2 or 4 with compact v2 windows, mempool cost 80 million, 154 MiB proposal cache | Today's limits, without the unbounded worst cases | At most 3.6 GiB |
| 8 GiB | Initial sync, then at the tip | RocksDB 2,665 MiB, pipeline 1,886 MiB, 25 peer targets without v2 or 7 with compact v2 windows, mempool cost 80 million, 328 MiB proposal cache | Nothing | Bounded by the budget |

## Settings that win over the budget

A budget derives `[network] peerset_initial_target_size` and `[mempool]
tx_cost_limit`, unless you set them explicitly: then your value is used, and
Zebra warns at startup if it is above the derived one. A value is explicit when
it differs from its default, so setting a field to its default value is the same
as leaving it out.

A budget can lower `tx_cost_limit` below [ZIP 401](https://zips.z.cash/zip-0401)'s
recommended 80 million; Zebra logs it at startup when it does.

The `[sync]` concurrency limits are not derived: the budget bounds the sync
pipeline in bytes on top of them.

## Allocator

On Linux with glibc, Zebra limits the allocator to 16 malloc arenas at startup.
With a budget, it lowers the limit to a quarter of the CPU cores it uses, between
2 and 16, before it starts its worker threads. `MALLOC_ARENA_MAX` or the
`glibc.malloc.arena_max` tunable in `GLIBC_TUNABLES` wins over both.

glibc keeps freed memory for reuse, so a process's memory can stay high after a
burst. When Zebra's memory rises above 85 % of its budget, it returns freed
memory to the operating system, at most once every 10 minutes.

## Containers and service managers

The budget is a target, not a hard limit: RocksDB's memtables and cache can
briefly exceed their share, and Zebra does not stop itself when it uses more
than its budget. Use the container or cgroup memory limit to enforce one, and
set the budget below it:

- set `memory_budget` to at most 80 % of the container's memory limit;
- turn swap off for the container, so the limit is a real memory limit;
- remember that the cgroup also counts the page cache of the state database,
  which the kernel reclaims under pressure.

```sh
docker run -d \
  --name zebra \
  --memory=2.5g --memory-swap=2.5g \
  -e ZEBRA_RESOURCES__MEMORY_BUDGET="2 GiB" \
  -p 8233:8233 \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:latest
```

```sh
systemd-run --user --scope -p MemoryMax=2.5G -p MemorySwapMax=0 \
  zebrad start
```

## Monitoring

With [metrics](metrics.md) enabled, Zebra exports:

- `zebra.memory.budget_bytes`: the budget, or 0 without one;
- `zebra.memory.plan_bytes`, with a `component` label: each component's share,
  and the `fixed` reserve and `slack`;
- `zebra.memory.profile`: 1 during initial sync, 0 at the tip;
- `zebra.memory.usage_bytes`, with a `component` label: RocksDB's memtables,
  block cache and table readers (`rocksdb`), the non-finalized state's retained
  blocks (`non_finalized`), and the blocks charged to the sync pipeline,
  including the checkpoint sync engine's lookahead (`pipeline`);
- `zebra.memory.pressure`: 0 below 85 % of the budget, 1 from 85 %, 2 from 95 %;
- `zebra.process.rss_bytes`, `zebra.process.rss_anon_bytes` and
  `zebra.process.vm_hwm_bytes`: the process's resident memory, its anonymous
  part, and its peak, on Linux.

Zebra measures its pressure on the anonymous resident memory, which leaves out
the reclaimable file-backed pages. See [troubleshooting](troubleshooting.md#memory)
for what to check when memory use is higher than expected.
