# Running Zallet with Zebra

[Zallet](https://github.com/zcash/wallet) is the `zcashd` wallet replacement.
From the release after 0.1.0-beta.3, its only chain backend is `zebra`, which
gets all of its chain data from a `zebrad` running on the same machine. This
page covers what that requires on the Zebra side. Zallet's own configuration is
documented in Zallet's [wallet setup guide](https://zcash.github.io/zallet/guide/setup.html).

Zallet does not use Zebra's lightwalletd-compatible gRPC server
(`lightwalletd_listen_addr`). That server is for light clients; see
[Serving light clients directly from Zebra](lightwalletd.md#experimental-serving-light-clients-directly-from-zebra).

## What Zallet needs from `zebrad`

Zallet uses three connections to `zebrad` at the same time. It needs all three.

1. **Read-only access to `zebrad`'s state database.** Zallet opens `zebrad`'s
   cache directory directly and reads finalized blocks, transactions, note
   commitment trees, and UTXOs from it. This is why Zallet must run on the
   same machine as `zebrad`. Zallet also reads the spending-transaction index,
   which `zebrad` only writes when it is built with the `indexer` feature.
2. **The indexer gRPC service**, which Zallet uses to follow the non-finalized
   part of the chain: it subscribes to tip changes and non-finalized blocks,
   and fetches any blocks it is missing.
3. **JSON-RPC**, which Zallet uses only for the mempool and for submitting
   transactions: `getrawmempool`, `getrawtransaction`, `getblockchaininfo`, and
   `sendrawtransaction`.

## Configuring `zebrad`

Build `zebrad` with the `indexer` feature:

```sh
cargo install --features="default-release-binaries indexer" --locked --git https://github.com/ZcashFoundation/zebra zebrad
```

Zebra's release binaries and published Docker images do not include this
feature. To build a Docker image with it, pass
`--build-arg FEATURES="default-release-binaries indexer"`; see
[Building with Custom Features](docker.md#building-with-custom-features).

Then enable the JSON-RPC and indexer endpoints in `zebrad.toml`:

```toml
[rpc]
listen_addr = '127.0.0.1:8232'
indexer_listen_addr = '127.0.0.1:8230'
```

JSON-RPC cookie authentication is on by default. Point Zallet's
`validator_cookie_path` at the cookie file, which is in `zebrad`'s cache
directory. The indexer gRPC service has no authentication, so keep it on a
loopback address.

In Zallet's config, `[indexer] validator_address` points at `listen_addr`,
`[indexer.read_state_service] grpc_address` points at `indexer_listen_addr`, and
`zebra_state_path` points at `zebrad`'s cache directory.

## Keep the versions in step

Zallet's direct-state backend reads `zebrad`'s database through the `zebra-state`
crate, so the reader and writer must agree on the database format. As of 27 October 2026:

| Zebra database format | Matching released Zallet direct-state backend |
| --- | --- |
| 28 (Zebra 6.x) | 0.1.0-beta.3, revision `987382f67e62`, using `zebra-state` 12.0.1 |
| 29 (Zebra 7.0.0, before block-body pruning) | None |
| 30 (Zebra with block-body pruning support, including archive mode) | None |

Consult the release's [backend manifest][zallet-backend] and [lockfile][zallet-lockfile].
Check its release notes before upgrading either component.

No released Zallet's direct-state backend can read this Zebra's database.
However, beta.3 also ships a [Zaino backend][zallet-zaino]: with `backend = "zaino"`
and no `[indexer.read_state_service]`, it fetches chain data over JSON-RPC without
opening Zebra's database. That topology is untested here. The
[pool payout RPC gaps](mining-pools.md#wallet-rpc-compatibility) apply to either backend.
For the tested alternative, see
[pool payouts through the sidecar](mining-pools.md#tested-sidecar-payout-path).
The sidecar reads blocks over P2P and requires an archive node.

If the formats do not match, Zallet fails at startup with a
`no zebra-state v… database found` error. Do not rename database directories
to bypass this check.

[zallet-backend]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/backends/zebra/Cargo.toml
[zallet-lockfile]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/backends/zebra/Cargo.lock
[zallet-zaino]: https://github.com/zcash/zallet/blob/987382f67e622915228686e9f956c6a9c9a7514c/backends/zaino/src/chain.rs#L203-L236

## Known issues

- A `zebrad` built without the `indexer` feature still serves the indexer gRPC
  service, and Zallet still starts against it. Zallet then reads an empty
  spending-transaction index instead of failing
  ([#11155](https://github.com/ZcashFoundation/zebra/issues/11155)). A `zebrad`
  built without `indexer` also deletes an existing index when it starts. If
  that happens, restart `zebrad` with an `indexer` build, which rebuilds the
  index from the existing state.
- With the `indexer` feature, `zebrad` checks the spending-transaction index on
  every restart, which uses a lot of CPU
  ([#11090](https://github.com/ZcashFoundation/zebra/issues/11090)).
- The direct-state backend cannot use a `zebrad` on another machine. A replacement
  remote backend using JSON-RPC and the indexer gRPC service is proposed in
  [zcash/zallet#899](https://github.com/zcash/zallet/issues/899); it is not implemented.
