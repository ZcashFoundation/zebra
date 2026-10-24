# Troubleshooting

Use the first error and the startup context, not only the process exit code.
Record the Zebra version, network, effective configuration, selected state
path, and the last height that advanced. The startup log includes build and
state-format metadata; see [Startup and sync](startup.md).

## Configuration does not load or take effect

| Symptom | Cause | Remedy |
| --- | --- | --- |
| `Zebra could not load the provided configuration file and/or environment variables` | Unknown fields or invalid TOML/environment values | Compare the reported field with the [configuration reference](config.md) and a generated reference file. |
| A file change has no effect | Zebra loaded a different file | Check `Using config file at:`; pass `--config /path/to/zebrad.toml` before `start` to select it explicitly. |
| A loaded file's value has no effect | An environment override replaces it | Check [environment variables](config.md#environment-variables), including service-manager and container settings. |
| An environment variable is rejected as sensitive | The field forbids environment overrides | Set it in the configuration file. |
| The P2P port stays unchanged after switching networks | An explicit `[network] listen_addr` retains its port | Remove that setting or change its port. |

Generate the reference without overwriting the active file:

```sh
zebrad generate --output-file zebrad-defaults.toml
```

## A listener cannot bind

The bind error names the endpoint and requested address; the P2P message starts
with `Opening Zcash network protocol listener`.

For an address-in-use error, identify the conflicting listener on Linux:

```sh
ss -ltnp
```

Stop the conflicting process or choose a distinct address for that endpoint;
each listener has its own [port setting](run.md#ports).

For an unavailable-address error, choose an address assigned to the host or
container. Without IPv6 support, use an IPv4 listener:

```toml
[network]
listen_addr = "0.0.0.0:8233"
```

This example listens on all IPv4 interfaces for Mainnet; use the
[network's P2P port](run.md#ports) or an explicit alternate port. Do not expose
administrative endpoints to work around a bind error.

## The database is locked or cannot open

- **`Database likely already open`:** another writer or an I/O error prevents
  opening state. Check services, containers, and manually started nodes. Stop
  the existing writer or give the second node a distinct `[state] cache_dir`
  and listeners. If no writer exists, check permissions and storage health.
  Do not delete RocksDB's `LOCK` file; wait for the owner to exit before
  modifying state.
- **`Permission denied creating` or `Opening database`:** check access to the
  parent directories and write permission on the cache root. In containers,
  check the mounted volume and effective UID/GID; see
  [Docker ownership handling](docker.md).

## Disk space is exhausted

The error `No space left on device` means that a write or directory creation
cannot complete. On Linux, check both capacity and available inodes on the
filesystem containing the cache. Replace the path below with an existing
cache directory:

```sh
df -h /path/to/zebra-cache
df -i /path/to/zebra-cache
```

Stop Zebra before moving its database. Free unrelated files or move the cache
to storage with sufficient free space, then update `[state] cache_dir` and any
environment or mount overrides. Do not delete individual RocksDB files to
make room: they form one database.

Keep room for growth and maintenance; snapshot restoration also needs
[space for both the archive and extracted state](snapshots.md#resolve-the-archive).

## Sync is slow or stalled

The syncer retries downloads and sync rounds automatically; `waiting to
restart sync` does not request a process restart. Check `current_height` and
`time_since_last_state_block` for continued progress.

For `initial sync can't download and verify the genesis block` or
`chain updates have stalled`, match the accompanying evidence:

| Symptom | Cause | Remedy |
| --- | --- | --- |
| No successful peer activity | Peer connectivity is failing | Check DNS errors, `resolved seed peer IP addresses`, configured seed addresses, and outbound connectivity. |
| Block-header times are rejected | A header is more than two hours ahead of the node's clock, violating the [protocol rule][blockheader] | Correct the time source if the clock is skewed; do not disable validation. Clock skew also distorts progress estimates. |
| Storage errors or resource saturation accompany the stall | The node cannot sustain downloads, verification, or state writes | Check capacity and permissions; compare [system requirements](requirements.md) and [limit contention](#limit-resource-contention). |
| `Zebra refuses to run` | The Mainnet release has reached [end of support](run.md#end-of-support) | [Upgrade](upgrading.md); restarting the same binary does not help. |
| A restored node starts from genesis | Zebra did not open the intended snapshot | Follow [snapshot troubleshooting](snapshots.md#troubleshooting) before allowing another full download. |

[blockheader]: https://zips.z.cash/protocol/protocol.pdf#blockheader

### Limit resource contention

For CPU or logging contention, use a release build and remove temporary debug
filters. In builds that retain debug instrumentation, restrict it to the
component under investigation:

```toml
[tracing]
filter = "info,zebra_network=debug"
```

Default release builds compile out debug and trace events; a filter cannot
restore them. See [compiled log levels](tracing.md#filters-and-compiled-log-levels).

Queued blocks consume memory, and concurrent verification competes for CPU.
Choose [sync concurrency limits](config.md#sync) for measured capacity rather
than raising every limit.

For a bandwidth-constrained node, reduce download concurrency before changing
verification limits:

```toml
[sync]
download_concurrency_limit = 25
```

Measure height advancement and resource use before and after a change; use
[metrics](metrics.md). Zebra raises limits below the
[minimum bounds](config.md#sync) and logs the adjustment.

### Linux TCP tuning for block propagation

Linux enables `net.ipv4.tcp_slow_start_after_idle` by default. It reduces a TCP
connection's congestion window after an idle period defined by the current
retransmission timeout. When peers are idle between block requests, this
restart can limit subsequent block-transfer throughput. Zebra checks the
setting on Linux and warns at startup when it is nonzero.

Inspect the effective setting:

```sh
sysctl net.ipv4.tcp_slow_start_after_idle
```

To retain the congestion window across idle periods, set:

```sh
sudo sysctl -w net.ipv4.tcp_slow_start_after_idle=0
```

This setting affects all TCP users in the network namespace, not only Zebra.
Review other workloads before changing it. To persist it for the host, put
this line in `/etc/sysctl.d/99-zebra-network.conf`:

```text
net.ipv4.tcp_slow_start_after_idle=0
```

Apply persistent host settings with `sudo sysctl --system`. See the
[Linux TCP sysctl reference][ip-sysctl] for the kernel behavior.

#### Running Zebra in Docker

Apply the setting in the network namespace containing Zebra. With Docker's
separate container networking, add
`--sysctl net.ipv4.tcp_slow_start_after_idle=0` to the `docker run` options.
With `--network=host`, configure the host instead; Docker does not allow
network sysctl overrides in host-network mode. Changing only the host setting
is not a substitute for configuring an existing separate container network
namespace. See [Docker's sysctl rules][docker-sysctl].

[ip-sysctl]: https://docs.kernel.org/networking/ip-sysctl.html
[docker-sysctl]: https://docs.docker.com/reference/cli/docker/container/run/#sysctl

## Known limitation: private peer addresses

Zebra does not reject private IP addresses merely because they are private.
It can connect to them and distribute peer addresses; filtering them by
default remains tracked in
[issue #3117](https://github.com/ZcashFoundation/zebra/issues/3117).

Use network-level egress controls if the node must not reach internal
services. Do not rely on the public Zcash network configuration to isolate a
node from private address ranges.

## Build and test resource failures

For compilation memory exhaustion, reduce [Cargo build parallelism][cargo-env]:

```sh
export CARGO_BUILD_JOBS=2
```

This limits build jobs, not node threads. For test resource contention, limit
[nextest's test concurrency][nextest-threads] separately:

```sh
cargo nextest run --locked --test-threads=2
```

For CI's nextest profiles, follow
[continuous integration](../dev/continuous-integration.md).

For unreliable external connectivity, set `SKIP_NETWORK_TESTS=1` to skip tests
that use Zebra's [network-test guard][network-test-guard]. It does not skip
every network-dependent test or disable IPv6 tests. Unset it before claiming
network-test coverage.

[cargo-env]: https://doc.rust-lang.org/cargo/reference/environment-variables.html
[nextest-threads]: https://nexte.st/docs/running/
[network-test-guard]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-test/src/net.rs

## Reporting a persistent failure

Include the recorded version, network, configuration, first error, and
surrounding logs in a [bug report][issues]. Redact RPC cookies, credentials,
wallet information, and private infrastructure details before publishing
logs. For suspected vulnerabilities, follow the
[security policy](https://github.com/ZcashFoundation/zebra/security/policy)
instead of opening a public issue.

[issues]: https://github.com/ZcashFoundation/zebra/issues/new/choose
