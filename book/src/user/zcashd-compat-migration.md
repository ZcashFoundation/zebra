# Migrating an Existing zcashd Node

Your `zcashd` halted at block 3,417,100 on 18 July 2026 and will not restart —
the end-of-support height is hard-coded. Its datadir and `wallet.dat` are
still perfectly good, and this page attaches them to Zebra with `zcashd`
running as a [P2P sidecar](zcashd-compat.md), so your existing wallet RPC
integration keeps working.

Use this only if you need that integration. If you do not depend on the zcashd
wallet, just run Zebra. If you can move your wallet now,
[migrate it to Zallet](https://zcash.github.io/wallet/cli/migrate-zcashd-wallet.html) —
that is the supported destination, and the sidecar is a bridge to it.

## What you need

- **Your existing zcashd datadir**, cleanly halted. Its blocks are reused, so
  the sidecar starts from that height instead of genesis. No conversion, no
  reindex.
- **A synced Zebra state** — a _separate_ ~260 GB database. The zcashd datadir
  does nothing for Zebra: the two nodes sync independently, Zebra from the
  network and zcashd from Zebra. Zebra must reach the tip before the sidecar
  can.
- **The sidecar `zcashd` build**, which has the end-of-support halt disabled.
  Stock `zcashd` cannot be used. See
  [The sidecar zcashd build](zcashd-compat.md#the-sidecar-zcashd-build).

The steps below use the binaries directly. They apply unchanged to container
deployments — same config, same RPC calls, run inside the container — and on
hosts too old for the sidecar (see [Troubleshooting](#troubleshooting)) a
container is the way to run it at all.

## The migration

### 1. Back up the wallet

```bash
cp /path/to/datadir/wallet.dat /secure/backup/wallet.dat
```

### 2. Write the config

```toml
[network]
network = "Mainnet"

[state]
cache_dir = "/var/lib/zebra"        # Zebra's own state, separate from zcashd's

[rpc]
listen_addr = "127.0.0.1:8234"      # not 8232 — that stays with zcashd
cookie_dir = "/var/lib/zebra"       # must be writable by the Zebra user

[zcashd_compat]
enabled = true
manage_zcashd = true
zcashd_source = "path"
zcashd_path = "/usr/local/bin/zcashd"
zcashd_datadir = "/var/lib/zcashd"  # your existing datadir
zcashd_extra_args = ["-txindex=1"]  # only if the datadir was built with it
```

zcashd keeps port 8232 — the port your integration already targets — so Zebra's
RPC moves elsewhere.

If your datadir was built with the transaction index, that flag is required:
the supervisor passes no `-txindex` of its own, and zcashd refuses to start on
a mismatch. Check with
`grep -a "transaction index" <datadir>/debug.log | tail -1`, taking the **last**
line, since the setting can change over a datadir's life.

### 3. Start

```bash
zebrad -c zebrad.toml start --zcashd-compat
```

zcashd loads the existing block index (several minutes on mainnet), then the
wallet — **no rescan, no key import**. A 1 MB `wallet.dat` with 102 transparent
and 2 sapling keys loaded in 18 ms in testing.

Supervised zcashd runs with `-printtoconsole`, so it no longer writes
`debug.log`; its output is streamed into Zebra's log under the
`zcashd_compat.zcashd` target. Any `debug.log` left in the datadir is stale.

### 4. Wait for both nodes to reach the tip

Zebra syncs from the network; zcashd then follows Zebra at roughly 30
blocks/second on older blocks and 12–15 on recent ones. Until zcashd reaches
the tip, wallet operations are refused with `-28` and the message _"This wallet
operation is disabled while reindexing"_ — nothing is reindexing; the gate is
initial block download:

```console
$ zcash-cli -datadir=/var/lib/zcashd getblockchaininfo \
    | grep -E '"blocks"|initial_block_download_complete'
```

Key and address reads (`z_exportkey`, `listaddresses`) work throughout.
Balances and spending — including `getwalletinfo` — wait for the tip.

### 5. Verify

Check the shield with
[Verify the integration](zcashd-compat.md#verify-the-integration): one peer, no
P2P listener, miner RPCs returning `Method not found`. Then confirm both nodes
agree at the same height:

```console
$ zcash-cli -datadir=/var/lib/zcashd getbestblockhash
000000000082239da6e6046c781dd6f4eaea1f5d6deccf23b4f32b149fcc062e

$ curl --silent -u "$(cat /var/lib/zebra/.cookie)" --data-binary \
    '{"jsonrpc":"1.0","id":"m","method":"getbestblockhash","params":[]}' \
    -H 'Content-type: application/json' http://127.0.0.1:8234/
```

Zebra has no command-line RPC client. Authenticate with `-u "$(cat …/.cookie)"`
— the file already holds the `user:password` string `curl` expects.

Finally, `getwalletinfo` and `z_gettotalbalance` confirm the wallet.

## After the migration

Deprecated methods are disabled by default: `z_listaddresses` and friends
return a generic `-1` with the explanation buried in a help dump. Re-enable per
method with `zcashd_extra_args = ["-allowdeprecated=z_listaddresses"]`, or move
callers to the replacement.

More consequential: **Ironwood is permanently unsupported in the zcashd wallet,
and Orchard operations are rejected from NU6.3 onward**, including spends of
existing Orchard notes. Transparent and Sapling are unaffected. See
[Wallet shielded-pool support](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood),
and plan the move to Zallet accordingly.

## Troubleshooting

Supervised zcashd's output is in Zebra's log: `grep zcashd_compat zebrad.log`.

| Symptom | Cause | Fix |
|---|---|---|
| `GLIBC_2.38 not found` at spawn | the sidecar needs glibc ≥ 2.38 — **higher than `zebrad`'s own 2.34**, so Zebra running fine proves nothing. Preflight never execs the binary, so it passes | newer base (Ubuntu 24.04, Debian 13), or containers |
| Restarts every few minutes; `You need to rebuild the database using -reindex to change -txindex` | `-txindex` mismatch | `zcashd_extra_args = ["-txindex=1"]`. **Do not `-reindex`** — hours, and hundreds of GB, for nothing |
| Assertion failure and signal 6 at startup | RPC port collision; the sidecar aborts instead of reporting "port in use" | move Zebra's RPC off 8232 |
| Panic: must be able to write the auth cookie | `rpc.cookie_dir` unset or not writable; the panic does not name the path | set it to a writable path |
| `-28 … disabled while reindexing` | initial block download | wait for `initial_block_download_complete` |
| `curl: (3) URL malformed` against Zebra's RPC | base64 cookie contains `/`, which breaks `http://__cookie__:<secret>@host/` | use `-u "$(cat …/.cookie)"` |
| Preflight warns about combined capacity | it compares total mount size, not free space | informational |
| zcashd healthy, holds its 1 peer, motionless at exactly Zebra's height | Zebra is not advancing either | diagnose Zebra, not the sidecar |

**Reading a restart loop.** A permanent misconfiguration logs the same line as
a transient crash:

```text
WARN zcashd-compat zcashd child exited before shutdown, restarting
  status=ExitStatus(unix_wait_status(256)) restart_count=1 child_uptime_secs=425
```

`256` is `wait(2)` encoding for "exited 1". What identifies a configuration
error is that every attempt dies at the same point with the same message — read
zcashd's output above the warning. Do not expect matching uptimes: an identical
failure measured 425 s then 295 s here, the cache being warm the second time.

**A stalled sidecar usually means a stalled Zebra**, since zcashd can never be
ahead of its only peer. The tell is that the heights are _equal_ rather than
the sidecar trailing. Run a current `zebrad`: a stale build stalled a migration
a few thousand blocks short of the tip on a peer-selection bug already fixed
upstream, and it presented as a sidecar fault.

## Rolling back

There is no going back to stock `zcashd` — every 6.20.0 binary halts at 3,417,100
and refuses to restart. What you can do:

- **Run the sidecar build unsupervised**, pointed at a Zebra node you manage
  yourself. See [Running externally managed](zcashd-compat.md#running-externally-managed).
- **Bring it up offline for wallet access only**, with `-connect` set to an
  unreachable address. It will not sync, but the wallet loads and key exports
  work.

The datadir and `wallet.dat` stay in stock format throughout, so migrating the
wallet to Zallet remains available at any point.
