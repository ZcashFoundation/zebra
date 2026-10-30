# Running lightwalletd with Zebra

[lightwalletd](https://github.com/zcash/lightwalletd) serves compact blocks and
transaction queries to light wallets over gRPC. Zebra validates the chain;
lightwalletd reads Zebra's JSON-RPC API and maintains its own compact-block
cache. Wallets connect to lightwalletd, not to Zebra's JSON-RPC port.

[Zaino](https://github.com/zingolabs/zaino), a Rust indexer, serves the same
light-wallet `CompactTxStreamer` gRPC protocol but uses different backend and
server configuration. Follow its deployment instructions.

The [Docker deployment](docker.md#running-zebra-with-lightwalletd) runs Zebra
and lightwalletd as separate services. Zebra also has an
[experimental direct gRPC service](#experimental-serving-light-clients-directly-from-zebra).

## Configure Zebra for lightwalletd

Generate a Zebra configuration file:

```sh
zebrad generate -o zebrad.toml
```

Edit the existing sections rather than adding duplicate TOML tables.

## JSON-RPC

Set these fields in `zebrad.toml` for a Mainnet backend:

```toml
[rpc]
listen_addr = "127.0.0.1:8232"
enable_cookie_auth = false
```

For Testnet, set `[network] network = "Testnet"` and use the conventional
[RPC port](run.md#ports), `127.0.0.1:18232`.

Lightwalletd does not read Zebra's
[authentication cookie](rpc.md#authentication), so this setup disables
authentication. Keep the listener on loopback or an isolated container
network; do not expose it to wallet users.

Lightwalletd does not require Zebra's `indexer` Cargo feature or the
`[rpc] indexer_listen_addr` endpoint. It uses JSON-RPC rather than direct
access to Zebra's database.

## Sync Zebra

Start Zebra with the configured listener:

```sh
zebrad -c zebrad.toml start
```

Keep Zebra running while lightwalletd serves clients. Monitor
[synchronization](startup.md), or start from a [state snapshot](snapshots.md).
A successful RPC connection does not establish that Zebra has reached the tip.

## Configure lightwalletd

Install the `zcash/lightwalletd` implementation using its
[installation instructions](https://github.com/zcash/lightwalletd#lightwalletd).
Create its backend connection file, `zcash.conf`:

```text
rpcbind=127.0.0.1
rpcport=8232
rpcuser=lightwalletd
rpcpassword=unused
```

Zebra does not read this file. Lightwalletd's parser requires a nonempty
`rpcpassword` even though Zebra ignores it with authentication disabled;
an empty file is insufficient, and the sample password is not access control.

For Testnet, change `rpcport` to `18232`. The parser uses `testnet=1` only to
choose that port when `rpcport` is absent; it does not change Zebra's network.
Restart lightwalletd when switching the backend network.

For containers, use the backend service hostname instead of loopback. The
[repository's Compose setup](docker.md#running-zebra-with-lightwalletd)
connects lightwalletd to `zebra:8232` without publishing RPC to the host. It
still publishes plaintext gRPC and HTTP; restrict access before using it
outside a trusted test environment.

Current upstream lightwalletd also accepts a Zebra `.toml` file through
`--zcash-conf-path`, reading `[rpc] listen_addr`, but not Zebra's cookie.

## Sync lightwalletd

For local testing, start lightwalletd with an explicit loopback gRPC listener:

```sh
mkdir -p "$HOME/.cache/lightwalletd"
lightwalletd \
  --zcash-conf-path ./zcash.conf \
  --data-dir "$HOME/.cache/lightwalletd" \
  --grpc-bind-addr 127.0.0.1:9067 \
  --http-bind-addr 127.0.0.1:9068 \
  --no-tls-very-insecure \
  --log-file /dev/stdout
```

Lightwalletd serves requests while filling its compact-block cache; uncached
requests read from Zebra. A quiet log does not prove either service is synced.

Query the service with
[grpcurl](https://github.com/fullstorydev/grpcurl):

```sh
grpcurl -plaintext 127.0.0.1:9067 \
  cash.z.wallet.sdk.rpc.CompactTxStreamer/GetLightdInfo
```

At its default log level, lightwalletd enables gRPC reflection, so this command
needs no local protocol files. The response reports the backend network,
height, and version. Compare the height with Zebra's sync status before
connecting wallets.

## Connect a wallet to lightwalletd

Configure the wallet's server address to point at lightwalletd's gRPC
listener. The local test example uses `127.0.0.1:9067`; loopback on another
device refers to that device, not to the server.

For a network-facing deployment, remove `--no-tls-very-insecure` and supply
`--tls-cert` and `--tls-key` with a certificate and private key trusted by the
wallet clients. Keep the backend JSON-RPC listener and lightwalletd's HTTP
listener private. Follow lightwalletd's
[production deployment instructions](https://github.com/zcash/lightwalletd#production-usage)
for certificate configuration.

## Run tests

Zebra's [lightwalletd integration tests](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/stateful/lightwalletd.rs)
launch a separate lightwalletd process. Install `lightwalletd` and `protoc`,
enable the `lightwalletd-grpc-tests` Cargo feature, and set
`TEST_LIGHTWALLETD=1`. These tests are ignored by default; select them using
the [stateful test instructions](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/tests/main.rs#L24-L43).

Sync and wallet tests also need cached Zebra state. Tests that reuse
lightwalletd state accept `LWD_CACHE_DIR`; without it, the harness looks for
the platform cache directory, such as `$HOME/.cache/lwd` on Linux. Use
disposable test copies, not the directories of running production services.

## Experimental: serving light clients directly from Zebra

Zebra's experimental `CompactTxStreamer` service runs without lightwalletd.
Its behavior and configuration can change incompatibly in any release.

Enable it in Zebra's [`[rpc]` section](config.md#rpc):

```toml
[rpc]
lightwalletd_listen_addr = "127.0.0.1:9067"
```

The listener calls Zebra's RPC handlers in-process and reads state and mempool
services directly. It needs no additional Cargo feature, JSON-RPC listener,
`zcash.conf`, or lightwalletd cache. Do not bind lightwalletd to the same
address and port.

This is not [Zallet's indexer gRPC protocol](zallet.md):
`[rpc] indexer_listen_addr` enables a different service.

### The port is not protected

The direct gRPC listener serves plaintext HTTP/2 without TLS or
authentication. JSON-RPC cookie authentication does not apply. Anyone who
can reach it can query chain data and submit transactions; network observers
can read or modify unencrypted traffic, including transparent-address
queries.

Bind it to loopback or a restricted private interface. For remote wallets,
put a reverse proxy with gRPC support in front of it, terminate TLS at the
proxy, and block direct access to Zebra's plaintext port. Configure proxy
stream timeouts for long-lived block-range and mempool requests.

### Do not run it on a user's machine

Run this unauthenticated service on infrastructure you administer, not on
end users' computers. A loopback bind limits network reachability but does
not isolate the service from untrusted local processes.

### Compatibility limits

The [direct service](https://github.com/ZcashFoundation/zebra/blob/main/zebra-rpc/src/lightwalletd/methods.rs)
differs from lightwalletd:

- The `Ping` method returns gRPC `unimplemented`.
- The `GetLightdInfo` response leaves `git_commit`, `branch`, `build_date`,
  `build_user`, and `donation_address` empty.
- Compact blocks have an empty `header` field.

Test wallet synchronization, transaction submission, and recovery across chain
reorganizations with the intended client before deployment; a successful
connection does not establish interoperability.
