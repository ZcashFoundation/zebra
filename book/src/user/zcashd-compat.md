# zcashd-compat mode

Zebra's zcashd-compat mode keeps legacy wallet integrations running during
migration to [Zallet](zallet.md) and Zebra's [JSON-RPC API](rpc.md). It uses a
separate, restricted `zcashd` sidecar with
[shielded-pool limitations](#wallet-shielded-pool-support-orchard--ironwood).
Zebra neither proxies wallet RPCs nor reads `wallet.dat`.

## Process and network boundaries

Zebra validates blocks from public peers; the sidecar receives them over one
outbound peer-to-peer (P2P) connection to Zebra. Each node validates and stores
its own chain. Both binaries must support the current network upgrade.

```text
public Zcash peers <--- P2P ---> zebrad <--- one P2P connection --- zcashd
                                 ^                                  ^
                           node/mining RPC                    wallet RPC, ZMQ
                                 |                                  |
                              miners                         legacy integration
```

| Responsibility | Zebra | Sidecar |
| --- | --- | --- |
| Public P2P connections and peer discovery | Yes | No |
| Chain validation and storage | Zebra consensus and state | Separate validation, block files, and chainstate |
| Wallet keys, balances, and transaction construction | No wallet | Existing `zcashd` wallet |
| Mining templates and block submission | `getblocktemplate`, `submitblock` | Methods not registered |
| Legacy wallet RPC and configured [ZeroMQ (ZMQ)](https://zeromq.org/) topics | Not forwarded | Served directly |

Configure both nodes for the same network and use distinct RPC listeners.
Compat mode does not move or secure RPC credentials or ZMQ endpoints.
The [migration configuration](zcashd-compat-migration.md#2-write-the-config)
keeps the legacy wallet endpoint for existing clients; see
[ports](run.md#ports) for network defaults.

## Quick start: the installer

Download and inspect the [installer][installer], then run it:

```sh
curl -fsSL https://raw.githubusercontent.com/ZcashFoundation/zebra/main/scripts/install-zebra.sh -o install-zebra.sh
less install-zebra.sh
bash install-zebra.sh --install-profile zcashd-compat
```

| Installer mode | Behavior |
| --- | --- |
| `split-binary` (default) | Downloads Zebra and the sidecar; prints separate start commands. |
| `supervised` | Downloads Zebra; prints a command that selects Zebra's embedded sidecar download and supervision. |
| `build-from-source` | Checks source trees and toolchains; prints build and start commands. |

The installer checks platform, hardware, filesystem capacity, and permissions;
it offers discovered data directories and verifies downloaded archives against
SHA-256 checksums. It does not start the nodes. For unattended installation,
supply the mode and paths explicitly:

```sh
bash install-zebra.sh --install-profile zcashd-compat --mode supervised \
  --network Mainnet --zebra-state-dir /var/lib/zebra \
  --zcashd-datadir /var/lib/zcashd --non-interactive
```

The installer has no compat Docker option, and Zebra publishes no sidecar
image. To use a [regular Zebra container](docker.md), provide a compatible
sidecar binary or run it separately, mount persistent data, and configure
container-reachable addresses.

## The sidecar zcashd build

Use the [Zcash Foundation sidecar release][sidecar-release], not stock
`zcashd`. Zebra 7.0.0 embeds this [release manifest][manifest]:

| Property | Pinned value |
| --- | --- |
| Release | `zebra-compat-v1.3.0` |
| Download target | `x86_64-pc-linux-gnu` only |
| Archive | `zcashd-zebra-compat-v1.3.0-linux-x86_64.tar.gz` |
| Archive SHA-256 | `a419147c49b08a15be2f1f1f5fea2801b5228491f62835bfa8caae8b06350c13` |
| Executable archive member | `./bin/zcashd` |

The pinned build supports NU7; `zebra-compat-v1.2.0` and earlier cannot follow
it. See [activation heights](../index.html#networks-and-network-upgrades).

The sidecar enforces these restrictions independently of Zebra:

- Exactly one nonempty `-connect` peer is required. Additional peers through
  `-addnode` or `-seednode`, P2P binds, and `-listen=1` are rejected. The
  `addnode` RPC is not registered.
- The `getblocktemplate`, `submitblock`, `getgenerate`, `setgenerate`, and
  `generate` methods are not registered; calls return `-32601`.
- The upstream scheduled end-of-support halt and warning are absent. This
  does not remove the need to update the sidecar for consensus changes.

The sidecar's network RPCs describe its connection to Zebra, not Zebra's
public peer set. Its `prioritisetransaction` method affects only its own
mempool, not Zebra's mining templates.

## Running externally managed

Set the compat configuration in Zebra's TOML file:

```toml
[zcashd_compat]
enabled = true
manage_zcashd = false
zcashd_datadir = "/var/lib/zcashd"
```

Start Zebra with that file, then start the sidecar separately:

```sh
zebrad -c zebrad.toml start --zcashd-compat
```

```sh
zcashd -datadir=/var/lib/zcashd -connect=127.0.0.1:8233 \
  -listen=0 -dnsseed=0 -listenonion=0 -discover=0 -printtoconsole
```

For Testnet, add `-testnet` and use Zebra's Testnet P2P address. For Regtest,
add `-regtest -regtestacceptunvalidatedpow`: Zebra's generated Regtest blocks
omit proof of work, so the sidecar must accept them.

The sidecar needs an existing `zcash.conf`; an empty file is sufficient for
peer selection when the arguments above supply it. Preserve existing wallet,
index, and RPC settings. Remove conflicting peer-selection settings. The
installer prepares a missing config for its externally managed modes; Zebra
does not bootstrap or supervise an externally managed process.

## Running supervised

Use the [supervised configuration](zcashd-compat-migration.md#2-write-the-config)
and [start command](zcashd-compat-migration.md#3-start).
The CLI flag enables compat mode, not supervision; explicitly set
`[zcashd_compat] manage_zcashd = true`.

With `zcashd_source = "embedded"`, Zebra downloads the pinned archive over
HTTPS, verifies its digest, and caches the executable under
`<state.cache_dir>/zcashd-compat/bin/<release>/<target>/zcashd`. Cached binaries
are checked against their recorded digest before reuse; an invalid cache
triggers a fresh download. Downloads have a 10-minute timeout and a 1 GiB
size limit. Unsupported targets, failed downloads, and archive digest
mismatches fail startup. To supply a verified local build, set
`zcashd_source = "path"` and `zcashd_path`; an explicit path takes precedence
over `zcashd_source` and bypasses the embedded download and checksum policy.

Zebra creates a missing sidecar data directory and minimal `zcash.conf` but
never overwrites an existing config. Prefer `[zcashd_compat] zcashd_datadir`
to a path in `zcashd_extra_args`; see the
[path and extra-argument rules](config.md#zcashd_compat).

The supervisor passes the selected data directory, network flags, and
`-printtoconsole`, followed by extra arguments. It then appends the forced
peer settings and `-daemon=0`, keeping the child in the foreground. Testnet
adds `-testnet`; Regtest adds `-regtest -regtestacceptunvalidatedpow`.
Peer-selection arguments (`-connect`, `-addnode`, `-seednode`, including
negated forms) are forbidden in `zcashd_extra_args`.

The sidecar's stdout and stderr enter Zebra's logs under
`zcashd_compat.zcashd`. With the normal `-printtoconsole` setting, the sidecar
writes there instead of `debug.log`.

### Sidecar peer privileges

Set `[zcashd_compat] p2p_connect_addr` if the sidecar needs a destination other
than Zebra's bound listener. The `block_gossip_peer_ips` field instead matches
the **source IP** Zebra sees. For a separate host or container, configure
that source:

```toml
[zcashd_compat]
enabled = true
manage_zcashd = false
block_gossip_peer_ips = ["10.0.0.2"]
```

Listed IPs receive block inventory broadcasts without random peer sampling.
They share a reserved inbound pool with one slot per distinct source IP,
capped to preserve at least one public inbound slot. The pool is shared,
not a per-IP quota. Reserved connections bypass the recent-IP reconnection
limit; overflow connections compete for public slots with normal rate
limiting. All listed IPs are exempt from the `FindBlocks`/`FindHeaders`
sync-stall detector.

Treat this list as a trust boundary. List only addresses whose originating
processes are all trusted. Do not grant a shared Docker proxy or NAT address
these privileges if untrusted public connections also arrive from it. Use
networking that distinguishes the sidecar's source. Merely raising
`[network] max_connections_per_ip` does not establish that distinction.

### Supervision and shutdown

See [supervisor timing fields](config.md#zcashd_compat) for delays and defaults.

The supervisor retries spawn failures and every pre-shutdown child exit,
including success, indefinitely with exponential backoff. Persistent config
errors therefore cause restart loops. If supervision itself fails, Zebra
logs the failure and keeps running; monitor the wallet process separately.

Stop Zebra rather than stopping its supervised child independently. Normal
shutdown sends the child SIGTERM, waits for the grace period, then uses
SIGKILL if necessary. An abrupt kill of Zebra cannot perform this cleanup;
check for a surviving child before starting another process on the same data
directory.

### Verify the integration

Query the sidecar with its own credentials:

```sh
zcash-cli -datadir=/var/lib/zcashd getpeerinfo
zcash-cli -datadir=/var/lib/zcashd getconnectioncount
zcash-cli -datadir=/var/lib/zcashd getblocktemplate
```

Check the sidecar's single-peer and disabled-mining restrictions above.
Inspect listeners with `ss -tlnp`: only Zebra should listen for P2P.
Then [compare fixed-height block hashes and wallet state](zcashd-compat-migration.md#5-verify).

## Mining: Zebra is canonical

Send mining requests to Zebra, not the sidecar; configure its payout address
as described in [Mining](mining.md).

## Wallet shielded-pool support (Orchard & Ironwood)

The [sidecar transaction builder][wallet-builder] enforces these restrictions:

| Pool | Sidecar wallet behavior |
| --- | --- |
| Ironwood | Unsupported by the wallet. |
| Orchard | From NU6.3 for the next block, excludes Orchard notes from selection and rejects transactions requiring Orchard spends, payments, or change. |
| Sapling | Supported; from NU7, transaction construction also enforces the [ZIP 218](https://zips.z.cash/zip-0218) Sapling action limit. |
| Transparent | Supported. |
| Sprout | From NU7, spending is rejected because [ZIP 2003](https://zips.z.cash/zip-2003) removes version 4 transactions. |

The Orchard restriction can produce `RPC_INVALID_PARAMETER` (`-8`) with the
message beginning `zcashd does not support the Ironwood pool`. An unsupported
recipient can instead fail address resolution. A unified address with a
supported receiver does not require an Orchard payment: selection also
respects the requested privacy policy and available funds.

Migrate affected wallet workflows to [Zallet](zallet.md), not an indexer or
wallet library.

## Initial sync and existing datadirs

Follow [Migrating an existing zcashd node](zcashd-compat-migration.md) for
data reuse, sync gates, backup, cutover, troubleshooting, and rollback.

## Configuration reference

See the [configuration reference](config.md#zcashd_compat) for fields,
defaults, and array parsing. To override extra arguments through the
environment:

```sh
export ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-rpcbind=127.0.0.1","-rpcallowip=127.0.0.1"]'
```

## Hardware preflight (Linux)

Enabled compat mode runs Linux startup checks even without supervision.
Preflight checks available CPUs, cgroup-limited RAM, directory permissions,
and [filesystem capacity](requirements.md#zcashd-compat-requirements), not
free space. Check free space separately and allow for growth.

Minimum shortfalls and check errors fail startup. The `--unsafe-low-specs`
flag downgrades errors to warnings; it does not repair permissions, provide
missing libraries, bypass checksum validation, or prevent later failures.
Preflight checks executable permissions, not dynamic-library loading.
Non-Linux compat startup also needs this override; embedded downloads remain
Linux x86-64 only.

[installer]: https://github.com/ZcashFoundation/zebra/blob/main/scripts/install-zebra.sh
[manifest]: https://github.com/ZcashFoundation/zebra/blob/main/zebrad/zcashd-compat-manifest.json
[sidecar-release]: https://github.com/ZcashFoundation/zcashd/releases/tag/zebra-compat-v1.3.0
[wallet-builder]: https://github.com/ZcashFoundation/zcashd/blob/zebra-compat-v1.3.0/src/wallet/wallet_tx_builder.cpp
