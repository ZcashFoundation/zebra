# zcashd-compat Mode (`zebrad start --zcashd-compat`)

zcashd-compat mode is for operators — typically exchanges and custodial
services who want to migrate to Zebra while keeping the `zcashd` wallet and
RPC surface their integration already depends on. Zebra faces the Zcash P2P
network and is the consensus node; `zcashd` runs as a **P2P sidecar** that
makes a single outbound peer connection to the local Zebra node and listens
for nothing. zcashd never touches the public network directly.

Your systems keep talking to `zcashd` exactly as before:

| Provided by `zcashd`, unchanged          | Moved to Zebra                             |
|------------------------------------------|---------------------------------------------|
| Wallet RPC methods (transparent + Sapling) | Public P2P networking and peer selection    |
| Local block files, chainstate, indexes   | Network-facing block and transaction relay  |
| ZMQ notifications                        | Block templates for miners                  |
| Local RPC response semantics             | DNS seeding and peer discovery              |

```text
Zcash network ═P2P (8233)═▶ zebrad ◀═P2P, internal only═ zcashd ◀─wallet RPC, ZMQ─ your systems
                            (front)   connect=zebra:8233  (sidecar)
```

The whole topology is two lines of zcashd configuration:

```text
connect=<zebra-host>:8233  # one outbound peer: Zebra
listen=0                    # no inbound P2P
```

`-connect` makes zcashd peer with the given address and _only_ that address:
zcashd itself then disables DNS seeding, inbound listening, and peer
discovery. zcashd syncs blocks and relays transactions over the standard
Zcash P2P protocol, with Zebra as its entire network.

There are two ways to run the pair:

- **Externally managed (default):** you run `zcashd` yourself with
  `connect=`/`listen=0` pointed at Zebra's legacy P2P listener.
- **Supervised:** Zebra spawns and manages `zcashd` itself when
  `[zcashd_compat].manage_zcashd = true`, passing the peer-pinning arguments
  automatically.

## Quick start: the installer

The recommended way to set up either variant is the interactive installer:

```console
curl -fsSL https://raw.githubusercontent.com/ZcashFoundation/zebra/main/scripts/install-zebra.sh | bash
```

Choose **2) With Zcashd compatibility** at the first prompt, then one of the
zcashd-compat modes:

| Mode                      | What it does                                                             |
|---------------------------|--------------------------------------------------------------------------|
| `split-binary` (default)  | Downloads `zebrad` and the sidecar `zcashd`; prints two start commands  |
| `supervised`              | Downloads `zebrad`; Zebra downloads hash-pinned `zcashd` at startup and supervises it |
| `docker-split-containers` | Pulls the Zebra and zcashd images; prints two `docker run` commands     |
| `docker-supervised`       | Pulls the Zebra image; prints one supervised `docker run` command (Zebra downloads hash-pinned `zcashd` at first start unless the image vendors it) |
| `build-from-source`       | Validates source trees and toolchains; prints build and start commands   |

The installer:

1. runs the same hardware preflight as `zebrad` itself (see
   [Hardware preflight](#hardware-preflight-linux)); `--unsafe-low-specs`
   downgrades failures to warnings for test rigs;
2. detects existing Zebra state directories and zcashd datadirs on mounted
   filesystems and offers them as defaults, so a migration continues from
   your synced data;
3. downloads SHA256-pinned release binaries (or pulls pinned Docker images);
4. bootstraps a minimal `zcash.conf` if none exists;
5. for Docker modes, bind-mounts the Zebra state cache so it survives `--rm`
   containers, and runs Zebra with `--network host` so the Zcash TCP P2P
   listener binds on the host's real addresses (bridge NAT would advertise
   an unreachable `172.17.0.x`);
6. prints ready-to-copy start commands for the mode you chose.

Every prompt has a matching flag for unattended runs, e.g.:

```console
install-zebra.sh --install-profile zcashd-compat --mode supervised \
  --network Mainnet --zcashd-datadir /var/lib/zcashd --non-interactive
```

See `install-zebra.sh --help` for the full list, including `--dry-run`.

## The sidecar zcashd build

Use the sidecar `zcashd` build from
[ZcashFoundation/zcashd](https://github.com/ZcashFoundation/zcashd) (the
`zcashd-compat` branch, forked from
[valargroup/zcashd](https://github.com/valargroup/zcashd)). The installer and
Zebra's embedded download both pin its release archives by SHA256. It differs
from stock `zcash/zcash` in three ways:

1. **P2P sidecar mode is hard-locked.** The binary refuses to start unless
   exactly one `-connect=<zebra-address>` peer is configured. It never opens a
   P2P listener, refuses peer-expanding options such as `addnode`, `seednode`,
   `bind`, and `whitebind`, and does not register the `addnode` RPC. This makes
   Zebra the only possible P2P peer.
2. **Miner RPCs are removed.** `getblocktemplate`, `submitblock`,
   `getgenerate`, `setgenerate`, and `generate` are not registered and return
   JSON-RPC `Method not found` (-32601). Zebra is the canonical source of
   block templates (see [Mining](#mining-zebra-is-canonical)). Read-only
   mining info RPCs (`getmininginfo`, `getnetworksolps`, `getblocksubsidy`,
   `prioritisetransaction`) remain.
3. **The upstream end-of-support halt is disabled.** Stock zcashd shuts
   itself down at its deprecation height; the sidecar build logs a warning
   and keeps serving its wallet/RPC surface. Consensus safety comes from
   Zebra, which fully validates every block before relaying it to zcashd.

Everything else — chainstate format, RPC semantics, ZMQ — matches stock
zcashd. The wallet carries the Ironwood/Orchard shielded-pool limits from the
sidecar baseline (see [Wallet shielded-pool support (Orchard &
Ironwood)](#wallet-shielded-pool-support-orchard--ironwood)).

## Running externally managed

Run Zebra normally (with `zcashd_compat.enabled = true` if you want
preflight checks and the RPC guardrails), then run zcashd yourself:

```console
zcashd -datadir=/var/lib/zcashd \
       -connect=127.0.0.1:8233 -listen=0 -dnsseed=0 -listenonion=0 -discover=0 \
       -printtoconsole
```

Or put the equivalent in `zcash.conf`:

```text
connect=127.0.0.1:8233
listen=0
```

`make compat-zcashd-start-standalone` (see `make/zcashd-compat.mk`) wraps
this command, and `make compat-zebrad-start-unsupervised` starts the
matching front node.

## Running supervised

```console
zebrad start --zcashd-compat
```

with a config like:

```toml
[zcashd_compat]
enabled = true
manage_zcashd = true
zcashd_source = "embedded"
zcashd_datadir = "/var/lib/zcashd"
zcashd_extra_args = ["-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1"]
```

(`zcashd_source = "embedded"` downloads the SHA256-pinned sidecar build from
Zebra's embedded release manifest; use `zcashd_source = "path"` plus
`zcashd_path` to run a binary you provide.)

> [!WARNING]
> The `embedded` source is **experimental**: it downloads a pinned `zcashd`
> build from [ZcashFoundation/zcashd](https://github.com/ZcashFoundation/zcashd)
> releases. The current `zebra-compat-v1.0.0` artifact is re-hosted from
> valargroup's build and has not yet been rebuilt by the foundation's own CI.
> Until then, production deployments should build the sidecar from source (or
> otherwise verify it) and use `zcashd_source = "path"`.

On start, Zebra:

1. runs Linux hardware and filesystem preflight checks (see
   [Hardware preflight](#hardware-preflight-linux); `--unsafe-low-specs`
   skips the minimums for test rigs);
2. resolves the zcashd binary (embedded download or local path) and
   bootstraps the zcashd datadir and a minimal `zcash.conf` if none exists;
3. spawns `zcashd` pinned to Zebra's own legacy P2P listener:

   ```text
   zcashd -datadir=... [-testnet | -regtest -regtestacceptunvalidatedpow] \
          -printtoconsole <your extra args> \
          -connect=<zebra p2p addr> -listen=0 -dnsseed=0 -listenonion=0 -discover=0
   ```

   The network-selection flags follow Zebra's own configured network, and
   `-printtoconsole` is always included so zcashd's output lands in Zebra's
   logs.

4. supervises it: restarts on unexpected exit with capped exponential
   backoff, and shuts it down gracefully (SIGTERM, then a configurable grace
   period) when Zebra stops.

The forced peer-pinning arguments are placed _after_ `zcashd_extra_args`
because zcashd takes the last occurrence of a single-valued command-line
argument. Peer-selection options (`-connect`, `-addnode`, `-seednode`) in
`zcashd_extra_args` are rejected at startup: the sidecar must peer with
Zebra alone.

By default the supervisor derives the `-connect` address from Zebra's own
bound legacy P2P listener (`network.listen_addr`), substituting `127.0.0.1`
when Zebra listens on an unspecified address. Set
`zcashd_compat.p2p_connect_addr` when zcashd must reach Zebra through a
different address (for example across containers).

When `zcashd_compat.enabled = true`, Zebra always includes inbound sidecar
peers in block inventory gossip, so zcashd does not depend on random peer
sampling to learn about new blocks. If `block_gossip_peer_ips` is empty, Zebra
defaults it to loopback addresses. For an externally managed sidecar that
connects from another local or private IP, enable zcashd-compat without
supervision and configure the IP **the sidecar's connections come from**
(not the loopback default — a sidecar connecting from another container or
host loses every sidecar privilege if its source IP is not listed):

```toml
[zcashd_compat]
enabled = true
manage_zcashd = false
block_gossip_peer_ips = ["10.0.0.2"]
```

> [!WARNING]
> When the fronting Zebra runs in Docker with a published P2P port, all
> connections arriving through `docker-proxy` (including a sidecar zcashd
> connecting to `127.0.0.1:8233` on the host) share one source IP. Zebra's
> `network.max_connections_per_ip` defaults to **1**, so the sidecar can lose
> that single slot to a proxied public peer and silently never connect. Set
> `ZEBRA_NETWORK__MAX_CONNECTIONS_PER_IP=8` (or similar) on a Dockerised
> front — the installer's Docker modes do this for you — or attach the
> sidecar to the container network directly.

### Verify the integration

Confirm zcashd is talking only to Zebra and exposes no P2P or mining
surface:

```console
$ zcash-cli getpeerinfo
# -> exactly ONE peer: the Zebra node ("subver": "/Zebra:.../", "inbound": false)

$ zcash-cli getconnectioncount
1

$ zcash-cli getblocktemplate
error code: -32601  # Method not found: miners must use Zebra

$ ss -tlnp | grep 8233
# -> only zebrad listening; zcashd has no P2P listener
```

Then confirm the tips converge: heights track each other and
`getbestblockhash` matches on both nodes once the drift reaches zero.

`deploy/zcashd-compat/sync-check.sh` and `make compat-status-sync` automate
the process/peer-pinning/height-drift checks, and the deploy watchdog's
`zcashd_compat_sync` check mirrors them for continuous monitoring.

The shield (single peer, no listener, no miner RPCs) is in effect immediately
on startup; you do not need a fully synced chain to verify it.

## Mining: Zebra is canonical

Miners and pools must request block templates from **Zebra's** RPC, not
zcashd's. Enable Zebra's RPC listener and set a miner address:

```toml
[rpc]
listen_addr = "127.0.0.1:8232"

[mining]
miner_address = "t1YourTransparentOrShieldedAddress"
```

Zebra's `getblocktemplate` and `submitblock` are always compiled in; no
special build is needed. See [Mining](mining.md) for details. The sidecar
zcashd returns `Method not found` for all template and submission RPCs, so a
misconfigured miner fails loudly instead of building on a lagging view.

## Wallet shielded-pool support (Orchard & Ironwood)

Orchard and Ironwood are shielded pools, exercised through the unified `z_*` wallet RPCs
(`z_sendmany`, and the rest). Those RPCs remain registered, but restricted
operations are rejected when the transaction is prepared. This is not the same
mechanism as the removed miner RPCs:

| | Miner RPCs (`getblocktemplate`, …) | Orchard / Ironwood |
| --- | --- | --- |
| Mechanism | Method not registered | Method registered; operation rejected at prep time |
| Error | `-32601` Method not found | `RPC_INVALID_PARAMETER` (`-8`) with a descope message |
| Scope | Always | Height-gated on NU6.3 activation |

These limits are a property of the sidecar `zcashd` build itself (the
Ironwood baseline), not the P2P sidecar layer. They apply identically whether
zcashd is externally managed or supervised.

- **Ironwood (the NU6.3 pool):** permanently unsupported. The zcashd wallet
  never supports Ironwood — this is a permanent descope, not a "not yet
  available" gate.
- **Orchard:** rejected from NU6.3 onward. Once NU6.3 is active for the next
  block, any Orchard involvement — spends of existing Orchard notes, Orchard
  payments, or Orchard change — fails at transaction-preparation time. Before
  NU6.3 activation, Orchard sends still work.
- **Transparent and Sapling:** unaffected. Existing transparent and Sapling
  wallet flows keep working.

When a restricted operation is attempted, zcashd returns `RPC_INVALID_PARAMETER`
(`-8`) with this message:

```text
zcashd does not support the Ironwood pool, and Orchard payments (including
spends of existing Orchard notes) are unsupported from NU6.3. Use transparent
or Sapling funds with zcashd, or a Z3-stack wallet for shielded payments.
```

For continued Orchard or Ironwood shielded support, migrate wallet flows to a
[Z3-stack wallet](https://github.com/zcash/wallet) ([Zallet](https://github.com/zcash/wallet),
[Zaino](https://github.com/zingolabs/zaino), or
[librustzcash](https://github.com/zcash/librustzcash)). This aligns with the
broader zcashd retirement path implied by the sidecar build's disabled
end-of-support halt.

> [!WARNING]
> Exchange and custodial integrations that rely on Orchard sends from the
> zcashd wallet must migrate those flows before NU6.3 activation on their
> network. After activation, Orchard operations fail at prep time with the
> message above — not with `Method not found`. Plan the migration alongside
> other network-upgrade readiness work (for example, deploying an updated
> sidecar build before each activation height).

## Initial sync and existing datadirs

The sidecar syncs the whole chain through its single Zebra peer. That works,
but initial block download through one peer is slow — for production
migrations, **bring your existing synced zcashd datadir** and let the sidecar
continue from its current height. The chainstate and block files are the
stock zcashd format; no conversion is needed. The installer searches mounted
filesystems for existing Zebra state directories and zcashd datadirs and
offers them as defaults; zcashd datadir snapshots are available from the
location it prints (currently <https://zcashd.valargroup.dev/>).

zcashd still performs its own full validation of every block Zebra relays —
the sidecar removes zcashd's _network exposure_, not its consensus checks.

## Configuration reference

```toml
[zcashd_compat]
# Master switch; also enabled by the `--zcashd-compat` CLI flag.
enabled = true

# Spawn and supervise zcashd (true) or run it yourself (false, default).
manage_zcashd = true

# "path" (default; use zcashd_path) or "embedded" (SHA256-pinned sidecar
# download from the embedded release manifest). An explicit zcashd_path
# always wins over this setting.
zcashd_source = "embedded"
# zcashd_path = "/usr/local/bin/zcashd"

# zcashd datadir; defaults to a subdirectory of state.cache_dir.
zcashd_datadir = "/var/lib/zcashd"

# Extra zcashd arguments. Peer-selection options are rejected.
zcashd_extra_args = ["-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1"]

# Zebra P2P address zcashd connects to. Defaults to Zebra's own bound
# legacy listener (loopback-substituted). Set for cross-container setups.
# p2p_connect_addr = "10.0.0.2:8233"

# Supervision lifecycle.
startup_delay = "1s"
restart_backoff = "2s"
restart_backoff_max = "5m"
restart_reset_after = "1h"
shutdown_grace_period = "5m"
```

All values can also be set through environment variables with the `ZEBRA_`
prefix, e.g. `ZEBRA_ZCASHD_COMPAT__ZCASHD_PATH=/usr/local/bin/zcashd`.
Because environment values cannot express TOML arrays, `zcashd_extra_args`
also accepts a JSON array string:

```console
ZEBRA_ZCASHD_COMPAT__ZCASHD_EXTRA_ARGS='["-rpcbind=0.0.0.0","-rpcallowip=0.0.0.0/0"]'
```

## Hardware preflight (Linux)

When `zcashd_compat.enabled` is set, Zebra checks at startup that the host
meets the minimum hardware requirements for running both nodes. Startup fails
below the minimums; `--unsafe-low-specs` overrides the failure for test
environments. Warnings (not failures) are printed between the minimum and
recommended tiers.

| Resource            | Minimum                        | Recommended        |
|---------------------|--------------------------------|--------------------|
| Logical CPUs        | 4                              | 8                  |
| Memory              | 16 GiB                         | 32 GiB             |
| Disk (mainnet)      | 275 GiB per datadir mount      | 1 TiB combined     |
| Disk (testnet)      | 30 GiB per datadir mount       | 100 GiB combined   |

If the Zebra state and zcashd datadir share one mount, that mount needs the
sum of both minimums (550 GiB on mainnet). The installer runs the same checks
before anything is downloaded.
