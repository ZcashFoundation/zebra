# Running Zebra

Zebra downloads, verifies, stores, and relays the Zcash chain. The `zebrad`
process runs continuously; it does not stop when the initial sync finishes.
Enable [JSON-RPC](rpc.md) to serve applications such as
[lightwalletd](lightwalletd.md), [Zallet](zallet.md), and
[mining pools](mining.md).

## Commands

Run Zebra with its default Mainnet configuration:

```sh
zebrad start
```

Omitting the subcommand also starts Zebra. Put top-level options, including
`--config`, before the subcommand:

```sh
zebrad --config /path/to/zebrad.toml start
```

| Command | Purpose |
| --- | --- |
| `zebrad start` | Run the node. Optional positional arguments override the tracing filter, for example `zebrad start info,zebra_network=debug`. |
| `zebrad generate` | Print a default TOML configuration to standard output. Use `--output-file /path/to/zebrad.toml` to write it to a file. |
| `zebrad tip-height --network Mainnet` | Print the finalized chain tip height stored on disk. Use `--cache-dir /path/to/cache` to override the configured cache root. |
| `zebrad copy-state --target-config-path /path/to/target.toml` | Copy finalized blocks to another state database for debugging. The source uses the main configuration; only `[state]` settings are used from the target configuration. |
| `zebrad help COMMAND` | Show the options for a command. |
| `zebrad --version` | Print the build version. |

The `tip-height` command opens a read-only state instance, does not connect to
peers, and does not upgrade an older database. Its output is not the live,
non-finalized chain tip. On a read failure it logs an error rather than printing
a height; check for numeric output, not only its exit status.

The `copy-state` command is an expert diagnostic tool, not a snapshot download
or full revalidation command. Use a distinct target cache directory. Without
`--target-config-path`, it uses temporary state that is deleted on a successful
exit. The `--max-source-height HEIGHT` option limits the copy.

## Configuration and network

See [configuration](config.md) for file selection, precedence, and environment
overrides.

Generate a starting configuration on Linux:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}"
zebrad generate --output-file "${XDG_CONFIG_HOME:-$HOME/.config}/zebrad.toml"
```

Keep only fields that need an explicit setting.

The `[network] network` field selects `"Mainnet"` by default. To use public
Testnet, set:

```toml
[network]
network = "Testnet"
```

When switching networks, remove an explicit `[network] listen_addr` to use the
new network's default port, or change its port. To select Testnet through the
environment:

```sh
ZEBRA_NETWORK__NETWORK=Testnet zebrad start
```

Use `"Regtest"` for a local development chain; see [Regtest](regtest.md).
See [custom Testnets](custom-testnets.md) for networks with other consensus
parameters. Separate processes need separate listener addresses and must not
write the same state database. Give each RPC server its own `[rpc] cookie_dir`,
even on different networks, so one server does not overwrite another's cookie.

## State directory

The [state cache](config.md#state) contains a finalized RocksDB database of
blocks, unspent transaction outputs, and validation/query indexes, plus
backups of recent blocks that can still be reorganized.

Zebra's chain state contains no wallet keys. Deleting it requires downloading
and verifying the chain again. Do not enable `[state] ephemeral` for a
persistent node. See [state snapshots](snapshots.md) for restoration and
[upgrading](upgrading.md) for format migrations and rollback.

## First sync

An empty node starts with the genesis block and downloads the chain from peers.
A checkpoint is a block height and hash compiled into Zebra that identifies a
known chain. With the default `[consensus] checkpoint_sync = true`, Zebra
checks the chain of block hashes against those checkpoints through the last
checkpoint. It does not repeat all proof and signature checks for those
blocks. Above that height, Zebra fully verifies new blocks, including their
transactions, before accepting them.

The [protocol's blockchain rules][blockchain] require validators to follow the
chain containing the settled network upgrades. Zebra's
[checkpoint routing][checkpoint-routing] enforces known checkpoint hashes
both while syncing and when checking existing state at startup.

The `checkpoint_sync = false` setting is for development. It starts full
verification after the first checkpoint at or above the block immediately
before Canopy activation. It does **not** disable mandatory checkpoints or
provide full verification from genesis: Zebra does not implement every legacy
pre-Canopy validation rule. Disabling checkpoint sync is slower and removes
protection provided by later checkpoints; keep the default for production.

A [state snapshot](snapshots.md) replaces the initial download and validation
of archived blocks with trust in the snapshot producer. See
[Startup and sync](startup.md) for the task sequence and log messages.

[blockchain]: https://zips.z.cash/protocol/protocol.pdf#blockchain
[checkpoint-routing]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-consensus/src/router.rs

## Progress and readiness

Zebra logs sync progress approximately once a minute; see
[progress messages](startup.md#fully-verify-recent-blocks) for the fields.
Keep the system clock synchronized: progress estimates use local time.

For automated readiness checks, configure the [health server](health.md) and
query `/ready`. A listening RPC port alone does not establish that the node
is synced.

Enable terminal progress bars with:

```toml
[tracing]
progress_bar = "summary"
```

Progress bars require the `progress-bar`
[Cargo feature](install.md#cargo-features) and redirect logs to a file.
See [tracing configuration](config.md#tracing) for modes and log paths, and
[metrics](metrics.md) for monitoring without a terminal.

## Ports

The table describes native `zebrad` defaults, not container port mappings.
All endpoints use TCP.

| Endpoint | Configuration field | Default |
| --- | --- | --- |
| Mainnet P2P | `[network] listen_addr` | `[::]:8233` |
| Public Testnet P2P | `[network] listen_addr` | `[::]:18233` |
| Regtest P2P | `[network] listen_addr` | `[::]:18233`, the same port as Testnet |
| JSON-RPC | `[rpc] listen_addr` | Disabled; no default port. Recommended explicit ports are 8232 for Mainnet and 18232 for Testnet. |
| Indexer gRPC | `[rpc] indexer_listen_addr` | Disabled; no default port. |
| Experimental lightwalletd-compatible gRPC | `[rpc] lightwalletd_listen_addr` | Disabled; no default port. |
| Prometheus metrics | `[metrics] endpoint_addr` | Disabled; no default port; requires the `prometheus` feature. |
| HTTP health and readiness | `[health] listen_addr` | Disabled; no default port. |
| Tracing-filter reload | `[tracing] endpoint_addr` | Disabled; no default port; requires the `filter-reload` feature. |
| [Tokio console](../dev/tokio-console.md) | `TOKIO_CONSOLE_BIND` environment variable | `127.0.0.1:6669`; requires the `tokio-console` feature and `tokio_unstable` compiler configuration. |

The P2P wildcard listener accepts IPv6 and IPv4 connections where the operating
system supports dual-stack sockets. Set a specific address to restrict the
listener. Zebra's Regtest default differs from zcashd's port 18344.

Bind administrative and monitoring endpoints to loopback or a protected
network. See [RPC authentication](rpc.md#authentication) before exposing RPC;
the indexer, lightwalletd-compatible, and health endpoints do not inherit
JSON-RPC authentication.

## Stopping Zebra

On Unix, press Ctrl-C or send `SIGINT` or `SIGTERM` to the `zebrad` process.
Zebra logs `received SIGINT, starting shutdown` or the corresponding `SIGTERM`
message. On Windows, Ctrl-C starts shutdown; Zebra does not implement Windows
service-control messages directly.

Zebra stops its asynchronous runtime and waits up to 20 seconds for
runtime-owned work to stop. Asynchronous tasks are canceled rather than
drained. State cleanup closes the block-write channels and flushes RocksDB
data and its write-ahead log. This does not guarantee that every in-flight
block or latest non-finalized block has reached disk: cleanup can be
interrupted, and the non-finalized backup is periodic. Zebra resumes from
persisted state on restart.

Wait for the process to exit before moving, copying, or replacing its state.
Avoid `SIGKILL` for routine shutdown because it skips cleanup. If Zebra manages
a zcashd child in [compatibility mode](zcashd-compat.md), shutdown also waits
for that child using `[zcashd_compat] shutdown_grace_period`.

## Exit codes

| Status | Meaning |
| --- | --- |
| `0` | Normal command completion, help/version output, or a handled shutdown signal. |
| `1` | Application initialization failure, a returned runtime error, or a failure during application shutdown. |
| `2` | Command-line usage error. The application also reserves this code for its explicit `Shutdown::Crash` path. |
| Platform-dependent | Termination by a signal, an abort, or another operating-system failure. |

A panic is not reliably exit code 2. Zebra's development and release profiles
use `panic = "abort"`; a panic terminates the process without normal unwinding.
On Unix, process supervisors distinguish this signal termination from a normal
exit status. Do not interpret every code 2 as a crash or every code 0 as proof
that a utility produced valid output; in particular, check `tip-height` output.

## End of support

Zebra enforces a height-based support window on Mainnet. Zebra 7.0.0 uses these
[compiled constants][end-of-support-source]:

| Constant | Value | Effect |
| --- | --- | --- |
| `ESTIMATED_RELEASE_HEIGHT` | 3,526,000 | Starting height for the support-window calculation. |
| `EOS_WARN_AFTER` | 42 days | Warn after the tip exceeds the height estimated 42 days after the starting height. |
| `EOS_PANIC_AFTER` | 56 days | Panic after the tip exceeds the height estimated 56 days after the starting height. |

The calculation advances from the starting height using each network
upgrade's target block spacing, including spacing changes within the window.
It is not a timer measured from installation, process startup, or the release's
calendar date. The last supported height is inclusive; the first unsupported
height is one block higher.

The task waits ten seconds after startup, then checks the local best tip once
per hour when a tip exists. It does not enforce expiry on Testnet, configured
Testnets, or Regtest. Mainnet warnings begin with
`Your Zebra release is too old and it will stop running`; expiry panics begin
with `Zebra refuses to run`. [Upgrade to a supported release](upgrading.md)
before reaching the reported cutoff. Restarting the same binary does not
extend its support window.

[end-of-support-source]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/src/components/sync/end_of_support.rs
