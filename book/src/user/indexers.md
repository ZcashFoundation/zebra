# Running Zebra for Indexers

Zebra can serve as the validator backend for Zcash indexers such as
[Zaino](https://github.com/zingolabs/zaino), [Zallet](./zallet.md), or custom
gRPC and JSON-RPC indexers. This guide covers the RPC endpoints, concurrency
limits, open file descriptor limits, and security boundaries to configure on
`zebrad`.

If you are deploying Zallet specifically, also see
[Running Zallet with Zebra](./zallet.md) for Zallet's database-format matching
and three-connection requirements.

## Configure RPC endpoints

Both RPC servers are disabled by default. Enable the endpoints your indexer
needs in the `[rpc]` section of `zebrad.toml`:

- `listen_addr`: HTTP JSON-RPC server (recommended ports: `127.0.0.1:8232` on
  Mainnet, `127.0.0.1:18232` on Testnet).
- `indexer_listen_addr`: gRPC indexer server (for example `127.0.0.1:8230`),
  which streams chain-tip updates, non-finalized blocks, and mempool changes.

Example configuration for Mainnet on loopback:

```toml
[rpc]
listen_addr = '127.0.0.1:8232'
indexer_listen_addr = '127.0.0.1:8230'
# Maximum concurrent in-flight HTTP JSON-RPC requests on listen_addr (default: 100).
max_connections = 100
```

If your indexer reads Zebra's spending-transaction index directly from disk,
compile `zebrad` with the `indexer` Cargo feature:

```sh
cargo install --features="default-release-binaries indexer" --locked --git https://github.com/ZcashFoundation/zebra zebrad
```

Official Zebra release binaries and published Docker images do not enable the
`indexer` feature. For custom Docker builds, pass
`--build-arg FEATURES="default-release-binaries indexer"` as described in
[Building with Custom Features](./docker.md#building-with-custom-features).

### JSON-RPC authentication

JSON-RPC cookie authentication (`enable_cookie_auth = true`) is enabled by
default on `listen_addr`. Zebra writes a random cookie to `<cookie_dir>/.cookie`
at startup (`[rpc] cookie_dir`, which defaults to Zebra's cache directory).

If your indexer cannot read the cookie file or connects over a private network,
configure [static credentials](./docker.md#static-credentials) with `rpc_user`
and `rpc_password_file`. Configuring static credentials disables cookie
generation.

## Security boundaries

Exposing Zebra's RPC ports on untrusted interfaces lets any reachable client
query your node's state, consume RPC capacity, or read and modify traffic in
transit.

- **JSON-RPC (`listen_addr`):** Cookie and static HTTP Basic authentication
  apply only to `listen_addr`. HTTP Basic authentication sends credentials
  unencrypted with every request, so keep `listen_addr` on loopback or an
  isolated private network, or terminate TLS in front of it.
- **Indexer gRPC (`indexer_listen_addr`):** This port serves plaintext HTTP/2
  with no TLS and no authentication. Cookie and HTTP Basic authentication do not
  apply to it. Bind `indexer_listen_addr` to a loopback address (`127.0.0.1`) or
  an isolated private network interface, and never expose it to untrusted
  networks.

## Concurrency and open file limits

### JSON-RPC connection limits

`[rpc] max_connections` sets the maximum number of concurrent in-flight HTTP
JSON-RPC requests on `listen_addr` (default `100`, minimum `1`). Requests beyond
this limit receive HTTP 429 Too Many Requests. This setting does not apply to
`indexer_listen_addr` or `lightwalletd_listen_addr`.

At most `max_connections / 2` requests (`50` at the default `100`) may wait in
`getblocktemplate` long polling at once. Excess long polls receive the retryable
JSON-RPC error `-32009` (`Server is busy`). This reserves the remaining
JSON-RPC capacity for `submitblock`, `getblockchaininfo`, and wallet or indexer
RPCs.

The indexer gRPC server allows up to `20` concurrent HTTP/2 streams per
connection and sends HTTP/2 keepalive pings every 30 seconds to detect dead
connections.

### Open file descriptors

At startup, `zebrad` tries to raise its soft open file descriptor limit to
`16_384` (`IDEAL_OPEN_FILE_LIMIT`). Zebra allocates half of its available file
descriptors (after a small fixed reserve) to RocksDB, leaving the remainder for
peer connections and RPC sockets.

The shipped
[systemd unit](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/systemd/zebrad.service)
sets `LimitNOFILE=65536`. If you raise `max_connections` or run many concurrent
clients against `zebrad`, raise your service, shell, or container `nofile` limit
so RocksDB and incoming sockets do not exhaust the process file descriptor
limit.

## Direct database access

Indexers that open Zebra's RocksDB state directory directly must run on the same
machine as `zebrad` with read access to `state.cache_dir`.

Direct readers use the `zebra-state` crate and can only read the database format
version they support. Keep your indexer and `zebrad` on compatible versions, and
upgrade direct readers together with `zebrad` as described in
[Upgrading the State Database](../dev/state-db-upgrades.md).

## Observability and health checks

- Enable [Zebra Metrics](./metrics.md) to monitor RPC latency
  (`rpc.request.duration_seconds`), parked long polls
  (`rpc.getblocktemplate.active_long_polls`), and file descriptor headroom
  (`process_open_fds` against `process_max_fds`).
- Enable [Zebra Health Endpoints](./health.md) (`/healthy` and `/ready`) for
  load balancers, container probes, and waiting until `zebrad` is synced to the
  tip before routing indexer traffic.
