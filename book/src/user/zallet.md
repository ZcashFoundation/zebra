# Running Zallet with Zebra

[Zallet](https://github.com/zcash/zallet) provides wallet functions; Zebra
validates the chain and serves node RPCs. Use the
[API migration guide](zcashd-api-migration-guide.md) to route calls and Zallet's
[setup guide](https://zcash.github.io/zallet/guide/setup.html) to initialize the
wallet, encryption, and wallet RPC server.

## Keep the versions in step

Zallet 0.1.0-beta.3's `zebra` backend reads Zebra's database directly and
requires format **28**
([backend changelog](https://github.com/zcash/zallet/blob/v0.1.0-beta.3/backends/zebra/CHANGELOG.md)).
Its revision `987382f67e62` uses `zebra-state` 12.0.1; consult its
[backend manifest][zallet-backend] and [lockfile][zallet-lockfile].
This Zebra keeps archives at format **29** and selects format **30** for
pruning or existing v30 state. Beta.3 cannot read either format, and no
released `zebra` backend supports them.
Do not deploy the direct-reader configuration below until a compatible
backend is released. If its required format is absent, startup fails with
`no zebra-state v… database found`; changing endpoints does not fix it.

The beta.3 `zaino` backend avoids this direct-reader format limitation with
`backend = "zaino"` and no `[indexer.read_state_service]`: it
[fetches chain data over JSON-RPC](https://github.com/zcash/zallet/blob/v0.1.0-beta.3/backends/zaino/src/chain.rs#L203-L233)
without opening Zebra's database. That topology is untested here.
The [pool payout RPC gaps](mining-pools.md#wallet-rpc-compatibility) apply to
either backend. For the tested alternative, see
[pool payouts through the sidecar](mining-pools.md#tested-sidecar-payout-path);
the sidecar reads blocks over P2P and requires an archive node.

[zallet-backend]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/backends/zebra/Cargo.toml
[zallet-lockfile]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/backends/zebra/Cargo.lock

Check the backend's release notes and `zebra-state` dependency before
[upgrading Zebra](upgrading.md). Zallet opens state read-only and cannot
upgrade it. Do not rename or symlink a versioned directory to bypass the
compatibility check.

Upgrade direct readers with the writer and start the writer first; read-only
instances do not move databases, run migrations, or rebuild spending indexes.
A v30-compatible reader must also handle unavailable historical bodies.
Disabling pruning does not restore deleted bodies or select an existing v29
directory instead of v30. Never merge those directories; recover full history
from an archive snapshot or resync in a separate cache.

## What Zallet needs from Zebra

The `zebra` backend runs on Linux and needs three interfaces simultaneously:

| Interface | Purpose | Requirement |
| --- | --- | --- |
| Read-only state database access | Reads finalized blocks, transactions, shielded trees, unspent outputs, and spending-transaction indexes. | Run on the same host, with a compatible `zebra-state` version and access to Zebra's persistent state cache. |
| Indexer gRPC | Follows tip changes and non-finalized blocks, and fetches missing blocks. | Set `[rpc] indexer_listen_addr`; keep the endpoint private. |
| JSON-RPC | Calls [`getrawmempool`, `getrawtransaction`, `sendrawtransaction`, and `getblockchaininfo`](https://github.com/zcash/zallet/blob/v0.1.0-beta.3/backends/zebra/src/chain/rpc.rs) for mempool access, transactions, and the network-upgrade table. | Set `[rpc] listen_addr` and give Zallet the authentication cookie. |

The backend does not connect to Zebra's wallet-facing
`[rpc] lightwalletd_listen_addr` service. That endpoint implements a different
protocol for [light wallets](lightwalletd.md).

The direct-state backend cannot use a Zebra database on another machine.
A remote backend using JSON-RPC and indexer gRPC is proposed in
[zcash/zallet#899](https://github.com/zcash/zallet/issues/899), not implemented
by this direct-state backend.

## Configuring Zebra

For a compatible direct-reader backend, build Zebra with the
[`indexer` feature](install.md#cargo-features):

```sh
cargo install --locked --version 7.0.0 --features indexer zebrad
```

For a [custom Docker build](docker.md#building-with-custom-features), pass
`--build-arg FEATURES="default-release-binaries indexer"`.

Set the state path and both endpoints in `zebrad.toml`. Choose directories
that Zebra can write and Zallet can access:

```toml
[state]
cache_dir = "/srv/zebra/state"

[rpc]
listen_addr = "127.0.0.1:8232"
indexer_listen_addr = "127.0.0.1:8230"
cookie_dir = "/srv/zebra/rpc"
enable_cookie_auth = true
```

Start Zebra with that configuration:

```sh
zebrad -c zebrad.toml start
```

Give Zallet read access to `/srv/zebra/rpc/.cookie` without making the
[credential](rpc.md#authentication) public. Keep indexer gRPC on loopback:
it has no authentication or TLS, and JSON-RPC authentication does not protect
it. Port 8230 is an example, not a default.

## Configuring Zallet

Set these fields in the Zallet configuration produced by its setup procedure.
The root `backend` key belongs before the first TOML section.

```toml
backend = "zebra"

[consensus]
network = "main"

[indexer]
validator_address = "127.0.0.1:8232"
validator_cookie_path = "/srv/zebra/rpc/.cookie"

[indexer.read_state_service]
grpc_address = "127.0.0.1:8230"
zebra_state_path = "/srv/zebra/state"
```

The `[indexer] validator_address` field is a host and port, without an
`http://` prefix. The `[indexer] validator_cookie_path` field selects cookie
authentication; the backend reads the file again for each request, so it
picks up a new credential after Zebra restarts. Do not substitute static
`validator_user` and `validator_password` values for Zebra's generated cookie.

The `[indexer.read_state_service] zebra_state_path` field points to the cache
root configured by Zebra's `[state] cache_dir`, not to a network-specific
versioned subdirectory. Use an absolute path to avoid resolving a relative
path against Zallet's data directory. Match Zallet's consensus network to
Zebra's network.

Zallet's wallet RPC listener is separate from both of these backend
connections. Assign it a different port; applications that need wallet
methods must connect to Zallet, not to Zebra.

## Index availability and restart behavior

Zebra starts `[rpc] indexer_listen_addr` even without the `indexer` feature.
A read-only consumer then receives empty results for missing indexes rather
than an initialization error
([#11155](https://github.com/ZcashFoundation/zebra/issues/11155)).

Keep the `indexer` feature enabled on the Zebra process that writes the state.
For **unpruned** state, starting a non-indexer build against an indexed
database removes the spending-transaction indexes. Restarting an indexer
build rebuilds them from the existing blocks; a wallet reader cannot perform
that rebuild. Wait for Zebra's `finished checking/adding indexes for spending
tx ids` log before relying on the indexes.

Once bodies have been pruned, changing spending-index build mode is rejected
before the writer can commit blocks. Restore an archive or resync to change
mode. Matching-mode pruned startups skip historical spending-index scans.
Read-only consumers do not create or repair missing indexes.

An indexed writer opening unpruned state checks or builds these indexes,
scanning existing blocks even without a format change. Allow for the CPU
load when scheduling restarts
([#11090](https://github.com/ZcashFoundation/zebra/issues/11090)).
