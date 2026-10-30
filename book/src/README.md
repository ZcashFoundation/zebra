# Zebra

Zebra is an independent implementation of the Zcash protocol, written in Rust
by the [Zcash Foundation](https://zfnd.org/). Its binary, `zebrad`, is a full
node: it downloads every block from the peer-to-peer network, checks it against
the consensus rules of the [protocol
specification](https://zips.z.cash/protocol/protocol.pdf) and the
[ZIPs](https://zips.z.cash/), and maintains the resulting chain state. The
original node, `zcashd`, has
[reached end of life](https://zcash.github.io/zcash/user/end-of-life.html).

## What Zebra does

- **Validation.** Blocks pass structural checks while parsing, semantic checks
  that need no chain context (proofs, signatures, scripts), and contextual
  checks against the chain they extend (double spends, value pools,
  difficulty). By default, blocks up to the last hard-coded checkpoint get
  only proof-of-work and Merkle-root checks and are accepted once their hashes
  chain to a checkpoint. [Architecture](dev/overview.md) describes the
  pipeline.
- **Chain state.** The finalized chain lives in RocksDB; the most recent blocks
  of every competing chain stay in memory, so Zebra can switch to a chain with
  more work.
- **Network.** Zebra speaks the Zcash peer-to-peer protocol and relays blocks
  and, once near the tip, unmined transactions from its mempool.
- **Interfaces.** A [JSON-RPC server](user/rpc.md) that follows the `zcashd`
  API serves light-wallet servers, wallets, explorers, exchanges, and
  [mining pools](user/mining.md).

Zebra holds no keys. Wallets use [Zallet](user/zallet.md); light clients use a
light-wallet server such as Zaino or [lightwalletd](user/lightwalletd.md), both
backed by Zebra. Systems that still depend on the `zcashd` wallet RPC can run
`zcashd` behind Zebra in [zcashd-compat mode](user/zcashd-compat.md); the
[API migration guide](user/zcashd-api-migration-guide.md) records which
component now owns each `zcashd` method.

## Networks and network upgrades

Zebra runs on Mainnet, Testnet, Regtest, and
[custom test networks](user/custom-testnets.md). Zebra 7.0.0 implements every
network upgrade through NU7, at these
[compiled-in heights](https://github.com/ZcashFoundation/zebra/blob/main/zebra-chain/src/parameters/constants.rs):

| Network upgrade | Mainnet height | Testnet height | Deployment ZIP |
| --- | ---: | ---: | --- |
| Overwinter | 347,500 | 207,500 | [ZIP 201](https://zips.z.cash/zip-0201) |
| Sapling | 419,200 | 280,000 | [ZIP 205](https://zips.z.cash/zip-0205) |
| Blossom | 653,600 | 584,000 | [ZIP 206](https://zips.z.cash/zip-0206) |
| Heartwood | 903,000 | 903,800 | [ZIP 250](https://zips.z.cash/zip-0250) |
| Canopy | 1,046,400 | 1,028,500 | [ZIP 251](https://zips.z.cash/zip-0251) |
| NU5 | 1,687,104 | 1,842,420 | [ZIP 252](https://zips.z.cash/zip-0252) |
| NU6 | 2,726,400 | 2,976,000 | [ZIP 253](https://zips.z.cash/zip-0253) |
| NU6.1 | 3,146,400 | 3,536,500 | [ZIP 255](https://zips.z.cash/zip-0255) |
| NU6.2 | 3,364,600 | 4,052,000 | [ZIP 257](https://zips.z.cash/zip-0257) |
| NU6.3 (Ironwood) | 3,428,143 | 4,134,000 | [ZIP 258](https://zips.z.cash/zip-0258) |
| NU7 | 3,543,405 | 4,465,026 | [ZIP 259](https://zips.z.cash/zip-0259) |

A release that does not implement an upgrade cannot follow the chain past its
activation height, and on Mainnet every release halts at its
[end-of-support height](user/run.md#end-of-support).
[Upgrading Zebra](user/upgrading.md) covers both deadlines.

## Getting started

Run the latest release in Docker, with the P2P port published and the state on
a named volume:

```sh
docker run -d --name zebra \
  -p 8233:8233 \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:latest
```

<a id="manual-install"></a>

Or install the pre-built binary for Linux on `x86_64` or `aarch64`:

```sh
cargo binstall zebrad
zebrad start
```

[Installing Zebra](user/install.md), [Running Zebra](user/run.md), and
[Zebra with Docker](user/docker.md) cover the details.

## Documentation

The [user documentation](user.md) is for operators and integrators, the
[developer documentation](dev.md) for contributors, and the
[API reference](api.md) points to the crates' rustdoc.

## Support and security

Report bugs in the
[issue tracker](https://github.com/ZcashFoundation/zebra/issues/new/choose);
[Troubleshooting](user/troubleshooting.md) lists known problems. Discussion
happens in the `zebra-support` channel of the
[Zcash Foundation Discord](https://discord.com/invite/aRgNRVwsM8). Report
vulnerabilities privately, as the
[security policy](https://github.com/ZcashFoundation/zebra/blob/main/SECURITY.md)
describes.

Zebra is dual-licensed under MIT and Apache 2.0; some crates are MIT-only,
as their `Cargo.toml` states.
