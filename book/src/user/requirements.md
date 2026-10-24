# System requirements

Size the host for initial synchronization and the services that share it, not
only for an idle node at the chain tip. See
[platform support](supported-platforms.md) for operating-system coverage.

## Resource use

| Resource | Consumers | Capacity planning |
| --- | --- | --- |
| CPU | Proof and signature verification, transaction execution, database compaction, and RPC requests | Reserve capacity for synchronization and client traffic; see [CPU worker configuration](config.md#sync). |
| RAM | Blocks awaiting verification, recent non-finalized chains, the mempool, proof parameters, and RocksDB caches and write buffers | Account for all processes and container memory limits. Reducing synchronization concurrency reduces the number of in-flight blocks; the mempool cost limit is not a process-wide memory limit. |
| Disk | Blocks, transactions, state indexes, recent-state backups, and configured log files | Keep free space for chain growth, database compaction, and upgrades. Account separately for snapshots and other services. |
| Network | Initial block download, new blocks and transactions, peer discovery, and responses to peer or RPC requests | Allow sustained outbound connectivity. Upload demand depends on the peers and clients served by the node. |

The [configuration reference](config.md) documents these controls. Monitor
[metrics](metrics.md) and host resource usage while synchronizing and under the
expected RPC load.

## Reference deployment

The Zcash Foundation's
[node deployment](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-prebuilt-nodes-gcp.yml)
and
[integration-test](https://github.com/ZcashFoundation/zebra/blob/main/.github/workflows/zfnd-deploy-integration-tests-gcp.yml)
use Google Compute Engine `c2d-standard-2` instances (2 vCPUs, 8 GB RAM) with a
400 GB balanced persistent state disk. This is not a minimum or an RPC-load
capacity guarantee.

## Disk capacity

The published Mainnet [state snapshot](snapshots.md) of 2026-10-23 reports a
source-node height of 3,527,378 and an archive size of 273,763,413,895 bytes
(255 GiB). The Testnet archive of the same day, at finalized height 4,531,081,
is 12,221,184,309 bytes (11.4 GiB). These are zstd-compressed tar archive
sizes, not measurements of the extracted databases. The snapshot index at
<https://snapshots.zfnd.org/> lists current archive sizes. The database grows
with every block.

Store state on persistent storage configured with `[state] cache_dir`. Mainnet
and Testnet have separate databases. Running both requires space for both.
The `zebra.state.rocksdb.total_disk_size_bytes` metric measures the RocksDB
table files, not all files on the filesystem. Check filesystem free space as
well.

A [state snapshot](snapshots.md) needs space for the downloaded archive and
the extracted database at the same time. Retaining an existing database during
replacement needs additional space. Do not use the compressed archive size as
the required capacity of the destination disk.

## zcashd-compat requirements

The [zcashd-compat mode](zcashd-compat.md) runs Zebra with a zcashd sidecar.
Its [Linux preflight checks](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/zcashd_compat/preflight.rs#L33-L60)
enforce these thresholds; they are not standalone Zebra benchmarks.

| Resource | Minimum | Recommended |
| --- | --- | --- |
| Logical CPUs available to the process | 4 | 8 |
| Effective RAM | 16 GiB | 32 GiB |
| Mainnet disk provisioning | 275 GiB for Zebra and 275 GiB for zcashd | 1 TiB combined |
| Testnet / Regtest disk provisioning | 30 GiB for Zebra and 30 GiB for zcashd | 100 GiB combined |

Effective RAM is the smaller of host RAM and the detected container memory
limit. The disk check measures total filesystem capacity, not free space.
Shared-filesystem minimums add: 550 GiB on Mainnet or 60 GiB on Testnet and
Regtest. Each filesystem counts once toward the combined recommendation.
Zebra checks the sidecar's disk only when it manages the sidecar or
`[zcashd_compat] zcashd_datadir` identifies the external sidecar's data directory.
A remote or otherwise unspecified sidecar needs separate capacity planning.

A failed minimum check stops startup. Values below the recommendation produce
warnings. The `zebrad start --unsafe-low-specs` option bypasses preflight
failures; it does not reduce resource use or make an undersized host safe.

## Network requirements and ports

Zebra needs outbound TCP connections to Zcash peers to synchronize. Default
peer discovery also needs working DNS resolution. Inbound peer connections
are optional for synchronization; enable them to let other nodes connect to
Zebra.

Configure the [P2P listener and advertised address](config.md#network), then
permit its [port](run.md#ports) through the firewall and publish it when
[running in Docker](docker.md#p2p-networking). Do not expose management
endpoints merely to make peer-to-peer networking reachable.
