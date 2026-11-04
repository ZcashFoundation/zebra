# Upgrading Zebra

Two deadlines force upgrades. A network upgrade changes the consensus rules at
a fixed height, and only releases that implement it can follow the chain past
that height. On Mainnet, each release also halts once the chain passes its
[end-of-support height](run.md#end-of-support). Releases can also change the
state format, the configuration schema, RPC responses, metrics, and logs; the
release notes list every change that needs operator action under **Breaking
Changes**.

## When to upgrade

| Trigger | Deadline | Where to find it |
| --- | --- | --- |
| Network upgrade | Before its activation height | [Activation heights](../index.html#networks-and-network-upgrades); release notes of the first release that supports it |
| End of support (Mainnet only) | Before the chain passes the release's support height | Startup log `Zebra release is supported until block N`; the `getdeprecationinfo` RPC field `end_of_service.block_height` |
| Security fix | On release | [Security advisories](https://github.com/ZcashFoundation/zebra/security/advisories) and the release's **Security** notes |

Zebra logs `Your Zebra release is too old and it will stop running at block N`
from 14 days before the support height. The `getdeprecationinfo` field
`end_of_service.estimated_time` reports the halt as a Unix timestamp, 24 hours
early. Both estimates assume the node is synced to the network tip.

The `zebrad` version is a [SemVer](https://semver.org/) version. A major
release adds Mainnet network-upgrade support or breaks an operator interface:
configuration, CLI, RPC, or a state format that older releases cannot read.
Minor and patch releases preserve compatibility for existing features, but a
newly enabled feature can persist state that builds without that feature must
not open; see [rolling back](#rolling-back) and
[Zebra versioning](../dev/release-process.md#versioning). Release candidates
are prereleases for testing and do not move the Docker `latest` tag.

## Before upgrading

Choose a supported release that implements upgrades active now and those
activating before your next planned update.

1. Read the [release notes](https://github.com/ZcashFoundation/zebra/releases)
   of every release after the one you run, up to the target. Act on each
   **Breaking Changes** entry: Zebra refuses to start with a configuration
   field it does not recognize, and dashboards, alerts, and RPC clients break
   on renamed metrics, log lines, or response fields.
2. Check whether the target changes the major state format. The release notes
   say so; the running format is the `v<major>` directory under
   `<[state] cache_dir>/state/`. If the major changes, take a
   [backup](#backing-up-the-state): no older release can read the upgraded
   state. If your state is not one of the target's registered reusable sources,
   the target cannot reuse it; see [first start](#what-happens-on-first-start).
3. Upgrade the software that depends on Zebra in the same window (see
   [dependent software](#dependent-software)).
4. Download and verify the new release as for a
   [new installation](install.md#pre-built-binaries).

## Backing up the state

A backup is a copy of one network's state directory, taken while no process
has the database open. Keep it outside `[state] cache_dir`, where no Zebra
process moves, upgrades, or deletes it.

1. Stop Zebra ([Stopping Zebra](run.md#stopping-zebra)) and every process that
   opens its database directly.
2. Copy the directory into a new, dated backup directory. For a Mainnet node
   with the default cache directory on Linux, before upgrading to Zebra 7.0.0:

   ```sh
   backup=/srv/zebra-backup/$(date +%F)-v28
   mkdir -p /srv/zebra-backup
   mkdir "$backup" && cp -a ~/.cache/zebra/state/v28/mainnet "$backup/"
   ```

   For the Docker image, copy out of the state volume:

   ```sh
   docker run --rm \
     -v zebrad-cache:/cache -v /srv/zebra-backup:/backup alpine \
     sh -c 'b="/backup/$(date +%F)-v28" &&
       mkdir "$b" && cp -a /cache/state/v28/mainnet "$b/"'
   ```

   The `mkdir` of the dated directory fails if that backup already exists, so
   a repeated command never mixes two copies.

The copy needs as much free space as the directory occupies (`du -sh` it
first). A recent [snapshot](snapshots.md) of the old format can replace a
backup for rollback, at the cost of syncing the blocks after its height.

## Upgrading

Stop the old release, replace it, and start the new one with the same
configuration and state directory. Never run two `zebrad` processes on the same
state.

### Binary installations

Replace the executable with one of:

```sh
cargo binstall zebrad@7.0.0
cargo install --locked --version 7.0.0 zebrad
```

or with the verified release archive. Then start `zebrad` as before.

### Docker

Stop the container without Docker's default 10-second kill deadline, then
recreate it from the new tag with the same volume, ports, and environment:

```sh
docker stop --timeout -1 zebra
docker rm zebra
docker run -d --name zebra --restart unless-stopped \
  -p 8233:8233 -v zebrad-cache:/home/zebra/.cache/zebra \
  zfnd/zebra:7.0.0
```

[Zebra with Docker](docker.md) documents the image and its tags.

## What happens on first start

The new release looks for its own major-version directory,
`<cache_dir>/state/v<major>/<network>`, and acts on what it finds:

| On disk | First start of the new release |
| --- | --- |
| The current major directory | Opens it. If the on-disk minor or patch format is older than the release's, a background task upgrades it in place while the node runs and syncs. A newer on-disk format is opened as compatible and marked as downgraded (see [rolling back](#rolling-back)). |
| Registered reusable source directories, with no current database | Renames the newest existing source directory to the new major (log `moved state cache from … to …`), then applies every intervening upgrade in version order. No copy and no resync; the source directory no longer exists. |
| The selected source directory, but resolving outside `[state] cache_dir` through a symlink | Does not move it (log `skipped reusing previous state cache`) and syncs a new database from genesis. |
| No current database or registered reusable source | Syncs a new database from genesis. Cleanup (below) keeps reusable sources on disk. |

Format upgrades log `trying to open older database format: launching upgrade
task`, then `Zebra automatically upgraded the database format` and `database
format is valid` when done. Stopping Zebra during an upgrade is safe: the
version file advances only after each step completes, and the next start
resumes the remaining work.

The setting `[state] delete_old_database` (default `true`) makes each start
delete older major-version directories under the cache directory, except
registered reusable source directories (`v25` through `v30` for format 32).
Setting it to `false` keeps every directory, but does not prevent the rename
above. Renaming or symlinking a versioned directory by hand does not convert
its format.

To avoid a sync from genesis when skipping majors, restore a
[snapshot](snapshots.md) whose format the target can open or reuse, instead of
starting from the old directory.

## Dependent software

| Software | Coupling | Action |
| --- | --- | --- |
| Processes that open the database directly: [Zallet's `zebra` backend](zallet.md), Zaino's read-state source | Built against a `zebra-state` version that reads one major format | Upgrade them to a build that reads the new format. Start Zebra first, wait for `database format is valid`, then start them. |
| RPC clients: lightwalletd, Zaino's RPC source, Zallet's `zaino` backend, mining pools | JSON-RPC and gRPC responses | Check the release notes' RPC entries. |
| Managed zcashd-compat sidecar (`[zcashd_compat] zcashd_source = "embedded"`) | Each `zebrad` release pins one sidecar build | Nothing: the new release downloads its pinned build. |
| Sidecar from `zcashd_source = "path"` or run externally | The sidecar must also implement each network upgrade | Install the build pinned in [`zcashd-compat-manifest.json`](https://github.com/ZcashFoundation/zebra/blob/main/zebrad/zcashd-compat-manifest.json); see [zcashd-compat mode](zcashd-compat.md). |
| Dashboards and alerts | Metric names and labels, log lines | Check the release notes' metrics and log entries. |

Record the Zebra revision used to build each direct reader, including cached
binaries; the downstream application's revision alone does not establish
state-format compatibility.

## Verifying the upgrade

- The executable reports its version: `zebrad --version`. A running node
  reports it in the `getinfo` RPC field `build`, in the Prometheus metric
  `zebrad_build_info{version="…"}`, and in its peer user agent
  `/Zebra:<version>/`.
- The logs show the state handling above and then sync progress.
- The [readiness endpoint](health.md) returns `200` once the node is back
  near the network tip.
- The `getblockchaininfo` RPC lists the network upgrades the release knows
  in `upgrades`, with their activation heights.

## Rolling back

Rolling back is limited by the same deadlines: the older release cannot follow
the chain past a network upgrade it does not implement, and on Mainnet it halts
at its own support height.

Within the same major state format, an older release can start on the existing
state only when it understands every persisted feature in that state. Ordinarily
it opens a newer minor or patch format and records the downgrade (`trying to open
newer database format: data should be compatible`).

Snapshot-born state uses the v32 directory barrier and a `snapshot_base` marker.
Never open it with an older build or rename it to v30, v31, v29, or any older
directory: those builds would treat its incomplete history as an archive. Keep
running a snapshot-aware release, or recover from a separate compatible archive
backup or a full sync.

Across a major format change, the older release looks for its own, older
directory, which the upgrade renamed away. Without the backup it syncs from
genesis. To restore the backup:

1. Stop Zebra and every process that opens its database directly.
2. Move the new major directory aside, so that a later upgrade reuses the
   restored directory instead, then restore the backup. Each `mkdir` fails
   instead of reusing an existing directory:

   ```sh
   state=~/.cache/zebra/state
   backup=/srv/zebra-backup/2026-10-23-v28   # the backup to restore
   mkdir /srv/zebra-backup/abandoned-v29 &&
     mv "$state/v29/mainnet" /srv/zebra-backup/abandoned-v29/
   mkdir -p "$state/v28"
   mkdir "$state/v28/mainnet" &&
     cp -a "$backup/mainnet/." "$state/v28/mainnet/"
   ```

3. Start the older release.

Roll back direct readers to builds for the older format at the same time.

Archives that have never been pruned or snapshot-imported are an exception for
the fixed-height upgrades through format 32: upgrades from v29 and v30 rewrite
no records, so the directory can be moved back for Zebra 7.0.0. See
[format 32 and recovery](pruning.md#format-32-recovery-and-copies).

## Zebra 7.1.0

- **State formats 29 and 30 → 32.** With no existing v32 database, the first
  start moves the newest reusable v30 or v29 source to `state/v32/<network>`
  without a resync. The intermediate 30.1 upgrade adds the empty `snapshot_base`
  column family; these fixed-height upgrades rewrite no existing records. Keep
  a separate source archive backup before upgrading if rollback matters:
  disabling old-database cleanup does not preserve the moved directory. Format
  v31 remains reserved for
  [#12182's variable-height record migration](https://github.com/ZcashFoundation/zebra/pull/12182).
- **Snapshot-born state is not downgrade-compatible.** Format 32 holds a
  `snapshot_base` marker recording where an imported snapshot's history starts.
  Never open that state with an older build, and never move it to `v30`, `v31`,
  `v29`, or any older directory: those builds would treat its missing pre-window
  bodies as a complete archive. A snapshot-born node must keep running a
  snapshot-aware release or recover from a separate compatible archive backup
  or a full sync.
- **Direct database readers.** Upgrade them to a build that understands format
  32, snapshot history boundaries, and unavailable historical bodies before
  they open the upgraded state. The never-pruned archive rollback exception
  above does not apply to pruned or snapshot-born databases.

## Zebra 7.0.0

- **NU7.** Mainnet activates NU7 at height 3,543,405; 6.x releases cannot
  follow the chain past it. After activation, Mainnet nodes disconnect peers
  that advertise a protocol version below 170190.
- **State format 28 → 29.** The first start renames `state/v28/<network>` to
  `state/v29/<network>` without a resync. Zebra 6.x cannot read format 29:
  back up `state/v28` first. A format-27 state (Zebra 2.4 to 5.x) is not
  reused; restore a snapshot instead of syncing from genesis.
- **Support window.** 7.0.0 is supported for 56 days after its estimated
  release height, instead of the usual 12 weeks.
- **Dependent software.**
  - The zcashd-compat sidecar must be `zebra-compat-v1.3.0`; earlier builds
    stop at NU7.
  - Zallet 0.1.0-beta.3's `zebra` backend reads format 28 and cannot read a
    7.0.0 state; its RPC-based `zaino` backend is unaffected.
- **Breaking changes.** These are in the notes for
  [7.0.0](https://github.com/ZcashFoundation/zebra/releases/tag/v7.0.0) and
  [7.0.0-rc.0](https://github.com/ZcashFoundation/zebra/releases/tag/v7.0.0-rc.0):
  - Per-peer labels are removed from the network metrics.
  - `getblocktemplate` advertises only `time` as mutable.
  - Custom testnets reject configurations that were previously accepted and
    store their state in new directories; resync custom networks.
