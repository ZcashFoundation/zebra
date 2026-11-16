# Migrating an existing zcashd node

Use [compat mode](zcashd-compat.md) while migrating legacy wallet RPC callers;
without wallet dependencies, [run Zebra directly](run.md). Zebra needs its own
state database and cannot import `zcashd` block files.

Zallet beta.3's direct `zebra` backend is incompatible with Zebra 7.0.0;
[the RPC-based `zaino` backend is unaffected](zallet.md).
Review the sidecar's [wallet pool restrictions](zcashd-compat.md#wallet-shielded-pool-support-orchard--ironwood)
before moving production traffic.

## What you need

- A cleanly stopped `zcashd` data directory, its configuration, and a verified
  wallet backup. Record the network, wallet filename, RPC credentials, and
  index settings.
- Separate storage for Zebra's state and enough time for its own sync, or a
  [Zebra state snapshot](snapshots.md).
- Zebra and a [compatible sidecar build](zcashd-compat.md#the-sidecar-zcashd-build).
- Capacity and permissions satisfying
  [compat preflight](zcashd-compat.md#hardware-preflight-linux).

The sidecar reuses the existing chain and wallet state from a compatible
`zcashd` data directory; Zebra still needs its own synchronization.

The procedure below uses Mainnet, `/var/lib/zcashd` for the existing sidecar
root, and `/var/lib/zebra` for Zebra. Substitute the actual paths. On Testnet or
Regtest, preserve the network-specific wallet subdirectory and configure both
processes for the same network.

## The migration

### 1. Back up the wallet

Stop the existing node cleanly and wait for it to exit before copying its
files. Preserve the entire data directory when rollback must also restore
chainstate and indexes. At minimum, back up the actual wallet file and
`zcash.conf` to restricted storage:

```sh
install -d -m 700 /secure/zcashd-backup
cp -p /var/lib/zcashd/wallet.dat /secure/zcashd-backup/wallet.dat
cp -p /var/lib/zcashd/zcash.conf /secure/zcashd-backup/zcash.conf
```

Use the configured wallet filename if it is not `wallet.dat`. Do not run the
old daemon and sidecar against the same directory, and do not overwrite the
backup during the migration.

### 2. Write the config

Create a Zebra config with separate RPC and state paths:

```toml
[network]
network = "Mainnet"

[state]
cache_dir = "/var/lib/zebra"

[rpc]
listen_addr = "127.0.0.1:8234"
cookie_dir = "/var/lib/zebra"

[zcashd_compat]
enabled = true
manage_zcashd = true
zcashd_source = "embedded"
zcashd_datadir = "/var/lib/zcashd"
zcashd_extra_args = ["-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1"]
```

This Mainnet layout keeps sidecar RPC on port 8232 for existing clients and
assigns Zebra port 8234. Keep both on loopback unless an authenticated,
encrypted deployment boundary protects them.

For a supplied binary, set `zcashd_source = "path"` and
`zcashd_path = "/usr/local/bin/zcashd"`; verify it separately because this
bypasses embedded archive verification.

Review the existing `zcash.conf` before startup:

- Preserve the transaction-index setting. The supervisor does not add
  `-txindex`; an existing `txindex=1` in `zcash.conf` is sufficient. Add
  `"-txindex=1"` to `zcashd_extra_args` only when needed to preserve the
  directory's existing setting. Changing it triggers a reindex requirement.
- Remove `connect`, `addnode`, `seednode`, `bind`, and `whitebind` entries that
  conflict with the supervisor's sole peer. Multi-valued peer options
  accumulate; a command-line setting does not erase existing peers.
- Preserve wallet, authentication, and ZMQ settings. The
  [supervisor](zcashd-compat.md#running-supervised) does not translate them
  or choose a nonconflicting RPC port.

Give the Zebra service account access to both data directories and the
wallet's existing authentication files.

### 3. Start

Start the supervised pair:

```sh
zebrad -c zebrad.toml start --zcashd-compat
```

Zebra runs preflight, resolves the binary, initializes its services, and
starts the sidecar after the configured delay, without waiting for the tip.
Honor reported index-upgrade, wallet-scan, or recovery requirements.
Read sidecar output in [Zebra's logs](zcashd-compat.md#running-supervised),
not the old `debug.log`.

### 4. Wait for both nodes to reach the tip

Query the sidecar:

```sh
zcash-cli -datadir=/var/lib/zcashd getblockchaininfo
zcash-cli -datadir=/var/lib/zcashd getpeerinfo
```

The sidecar must have one outbound peer pointing to Zebra. Its
`initial_block_download_complete` field must become `true` before
chain-dependent wallet operations are available. Those methods return `-28`
with `This wallet operation is disabled while reindexing.` during initial
block download even when no reindex is occurring. Key and address methods
such as `z_exportkey` and `listaddresses` are not subject to that gate.

Monitor [Zebra's synchronization](startup.md) separately. If both heights
stall, inspect Zebra's sync and peers; if only the sidecar stalls, inspect
its connection, consensus version, and logs. Equal heights do not prove
either node has reached the network tip.

### 5. Verify

Check [peer and listener isolation](zcashd-compat.md#verify-the-integration),
then compare block hashes at a fixed height available on both nodes:

```sh
height=$(zcash-cli -datadir=/var/lib/zcashd getblockcount)
zcash-cli -datadir=/var/lib/zcashd getblockhash "$height"
curl --silent --show-error --user "$(cat /var/lib/zebra/.cookie)" \
  --header 'Content-Type: application/json' \
  --data-binary "{\"jsonrpc\":\"2.0\",\"id\":\"migration\",\"method\":\"getblockhash\",\"params\":[$height]}" \
  http://127.0.0.1:8234/
```

Compare the sidecar's hash with the JSON `result`; recheck at a stable height
if a reorganization occurs. Protect the [RPC cookie](rpc.md#authentication);
do not put it in a URL.

Query wallet state through the sidecar and compare it with the inventory
recorded before migration:

```sh
zcash-cli -datadir=/var/lib/zcashd listaddresses
zcash-cli -datadir=/var/lib/zcashd getwalletinfo
zcash-cli -datadir=/var/lib/zcashd z_gettotalbalance
```

Check addresses, balances, application RPCs, and notification consumers before
restoring wallet traffic. A matching tip does not prove the correct wallet
was loaded.

## After the migration

The sidecar disables deprecated methods such as `z_listaddresses` by default,
but keeps `z_gettotalbalance` enabled. Migrate callers using the
[API table](zcashd-api-migration-guide.md). For a required legacy method,
append `-allowdeprecated=<method>` to `zcashd_extra_args`; do not replace
the existing arguments.

## Troubleshooting

| Symptom | Diagnosis and action |
| --- | --- |
| Hardware or capacity preflight fails | Resolve the reported problem; see [preflight and override limits](zcashd-compat.md#hardware-preflight-linux). |
| Dynamic-loader or missing-library error | Use a compatible runtime or verified native build; preflight does not load libraries. See [container options](zcashd-compat.md#quick-start-the-installer). |
| `reindex to change -txindex` | Restore the existing index setting. Reindex only for an intentional setting change or diagnosed damage. |
| Sidecar RPC cannot bind | Separate the RPC addresses/ports; preserve the sidecar's port if clients depend on it. |
| Zebra cannot write its auth cookie | Check permissions and symlinks against the [authentication requirements](rpc.md#authentication). |
| Wallet RPC returns `-28` | Wait for sidecar initial block download; inspect logs if height stalls. |
| Wallet operation rejects Orchard or Ironwood | Migrate the restricted wallet workflow; changing peers or RPC routing does not help. |
| Sidecar exits or cannot spawn | Read the child error before the supervisor warning; [retries are indefinite](zcashd-compat.md#supervision-and-shutdown). |
| Zebra runs without a managed sidecar | Check `manage_zcashd`, startup logs, and supervisor errors; supervisor failure does not stop Zebra. |
| Sidecar cannot connect across hosts or containers | Check destination and firewall, then configure the trusted [source IP](zcashd-compat.md#sidecar-peer-privileges). |
| Both nodes stop at an upgrade boundary | Check both versions; upgrading Zebra does not update an explicit `zcashd_path` binary. |

## Rolling back

Stop wallet traffic, then [stop the supervised pair](zcashd-compat.md#supervision-and-shutdown).
Check for a surviving child before changing binaries or supervision.

To undo supervision without discarding wallet progress:

1. Set `[zcashd_compat] manage_zcashd = false`; keep compat mode enabled for
   the sidecar's trusted-peer handling.
2. Restart Zebra with the same state and RPC configuration.
3. Start the same compatible sidecar externally against its existing data
   directory, with exactly one `-connect` peer pointing to Zebra. See
   [Running externally managed](zcashd-compat.md#running-externally-managed).
4. Repeat the fixed-height hash and wallet checks before restoring traffic.

For a binary or data rollback, preserve the failed directory and restore a
clean backup separately. Use binaries compatible with current consensus
rules and the backup's format; an obsolete stock `zcashd` is not a supported
replacement. Do not assume on-disk updates are reversible by downgrading.

Reconcile wallet activity since the backup before spending: it can omit later
keys and transactions. Retain the original wallet and subsequent backups
until migration is complete. Disabling supervision does not convert wallet
formats.
