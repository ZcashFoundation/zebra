# Syncing from a State Snapshot

A full sync from genesis takes days on Mainnet. The Zcash Foundation publishes
snapshots of Zebra's cached state at <https://snapshots.zfnd.org/>, so a new
node can start a few blocks behind the tip instead.

**Start at that site.** It lists the current archive for each network with its
height, size, and checksum, and it has the download, verify, and extract
commands. It is regenerated every time a snapshot is cut, so it is always
current; this page deliberately does not repeat archive names, dates, or sizes,
because those change with every cut and older archives are pruned.

What follows is the Zebra side of the operation: how to resolve the current
archive, what you are trusting, which snapshots your build can actually load,
and where the archive has to land.

## Resolving the current archive

There is no `latest` alias URL, so don't hardcode an archive name. Each network
publishes `https://snapshots.zfnd.org/<network>/snapshots.json`: an array of
every archive available for that network, newest first. The newest archive of
each database major carries `"latest-v<major>"` in its `roles`. Resolve the one
for your build at download time:

```sh
major=29   # your zebrad build's database format major
url=$(curl -fsS https://snapshots.zfnd.org/mainnet/snapshots.json \
  | jq -r --arg role "latest-v$major" 'map(select(.roles | index($role)))[0].url')
echo "$url"
```

Use `testnet` in place of `mainnet` for the Testnet archive. To find your
`major`, see [Check the database format version](#check-the-database-format-version).
If `url` is `null`, no archive for that major is published yet, and that
section says when the previous major works instead.

Then download the checksum and the archive, and check the archive:

```sh
curl --fail -O "$url.sha256"
for attempt in $(seq 10); do
  curl --fail -C - -O "$url" && break
  sleep 10
done
sha256sum -c "$(basename "$url").sha256"
```

A Mainnet download can take hours, and the connection can drop partway. The
loop resumes from the bytes already on disk, and so does running it again after
it gives up. It avoids `curl --retry` because `curl` releases before 8.17
restart a retried download from the beginning. Don't use an archive that fails
`sha256sum -c`: delete it and download it again.

Each entry also carries `height`, `db_major`, `sha256`, `zebrad_version`, and
`manifest_url`, so a script can check compatibility before it commits to a
long download:

```sh
curl -s https://snapshots.zfnd.org/mainnet/snapshots.json \
  | jq -r '.[0] | "height=\(.height) db_major=\(.db_major) zebrad=\(.zebrad_version)"'
```

## On trust

A snapshot replaces the blocks Zebra would otherwise download and validate
itself. When you load one, you are trusting that the node it was cut from
validated the chain correctly up to the snapshot height. Zebra does not
re-validate the blocks it finds in its cached state, so nothing downstream will
catch a bad snapshot for you.

The `.sha256` checksum is narrower than it looks. It proves the bytes you
received are the bytes that host served — no more. It is published by the same
host as the archive, so it cannot show the archive came from the Zcash
Foundation: a host serving a bad archive serves a matching checksum with it. The
manifests are currently **unsigned**, so there is no first-party way to
authenticate a snapshot today.

This is true however you fetch a snapshot — downloaded and verified, streamed
straight into `tar`, or unpacked inside a container. Verifying the checksum
protects you against a corrupted transfer, not against a compromised or
substituted archive. If you need more assurance than that, the expected digest
has to reach you out of band: from an operator you trust who has verified the
same archive, or from a digest you recorded earlier through a channel you
trust. And if your node secures funds or serves other people's wallets, and you
cannot afford to trust a third party's validation at all, sync from genesis
instead.

## Check the database format version

Use the archive tagged `latest-v<your major>`, where `<your major>` is the
state database format _major_ version of your `zebrad` build. An archive from
the previous major also works if your release can upgrade it in place, which
Zebra 7.0.0 (major 29) does for v28 archives on its first start. `zebrad`
ignores an archive from any other major: it starts an empty state and syncs from
genesis, without an error.

To find both majors:

- The archive: `db_major` in `snapshots.json`, the `v<major>` in its filename,
  and `database_format_major_version` in its manifest.
- Your build: `DATABASE_FORMAT_VERSION` in `zebra-state/src/constants.rs` at
  your release's tag. Zebra also logs it at startup as the `v<major>` component
  of the cached state path:

  ```text
  zebra_state::...::disk_db: Opened Zebra state cache at /home/user/.cache/zebra/state/v29/mainnet
  ```

The manifest is also where you'll find the snapshot's chain height and the
`zebrad` version that produced it.

### Starting from the previous major

On its first start, a `zebrad` on major 29 looks for a major 28 state. If
`<cache_dir>/state/v28/<network>` holds a database and
`<cache_dir>/state/v29/<network>` does not, `zebrad` moves the directory to the
v29 path and then upgrades the database format in place. The move is a rename,
so it is fast: in a restore test it took well under a second, and the node
committed new blocks within 21 seconds on Mainnet and 10 seconds on Testnet.

Nothing is moved if `<cache_dir>/state/v29/<network>` already holds a database,
for example because `zebrad` started before you extracted the archive. Stop
`zebrad`, delete that directory, and start it again.

The log shows the move and the upgrade:

```text
zebra_state::...::disk_db: moved state cache from "<cache_dir>/state/v28/mainnet" to "<cache_dir>/state/v29/mainnet"
zebra_state::...::upgrade: trying to open older database format: launching upgrade task running_version=29.0.0 disk_version=28.0.0
zebra_state::...::upgrade: marked database format as upgraded running_version=29.0.0 disk_version=28.0.0 format_upgrade_version=29.0.0
```

The move is one-way: afterwards, a `zebrad` on major 28 (such as 6.x) can't use
the directory, and `state.delete_old_database = false` doesn't prevent the
move. To keep a way to roll back, keep the archive or a copy of `state/v28`. For
how upgrades work, see
[Upgrading the State Database](../dev/state-db-upgrades.md#upgrading-the-state-database).

### How long older majors stay published

The newest archive of an older major stays published, as `latest-v<major>`, at
least until the last `zebrad` release that uses that major reaches end of
support. See [Supported Releases](../dev/release-process.md#supported-releases).

## Extract into your `cache_dir`

The archive already contains the `state/v<major>/<network>/` path Zebra
expects, so extract it straight into `cache_dir` — there is no path to
assemble by hand:

```sh
mkdir -p /path/to/your/cache_dir
tar --zstd -xf "$(basename "$url")" -C /path/to/your/cache_dir
```

`cache_dir` is `state.cache_dir` in your `zebrad.toml`, which defaults to
`~/.cache/zebra` on Linux. In the Docker image it is `ZEBRA_STATE__CACHE_DIR`,
which defaults to `/home/zebra/.cache/zebra`.

**This path has to match exactly.** If the directory you extract into is not
the `cache_dir` Zebra runs with, `zebrad` won't error; it will create an empty
state there and start syncing from genesis, silently. Double-check both paths
before you extract, and stop `zebrad` first if it is already running against
that directory.

Once it starts, confirm from the logs that it picked the snapshot up. The tip
is not genesis. If the manifest's `chain_tip.block_hash` is set, the logged
height equals `chain_tip.height`. If it is `null`, as in older manifests, the
logged height is about 1,000 blocks lower: those manifests record the source
node's tip, and up to 1,000 of its newest blocks aren't finalized, so they
aren't in the archive. `zebrad` downloads them again within minutes.

```text
zebra_state::service: loaded Zebra state cache chain_tip=Some((block::Hash("0000..."), Height(3459525)))
```

### Streaming, to avoid needing the space twice

Downloading and then extracting needs the archive and the extracted state on
disk at once, about twice the archive size. To avoid holding both, pipe the
download straight into `tar`. A stream can't resume: if the connection drops,
`tar` stops partway and leaves an incomplete state directory in `cache_dir`,
which you must delete before you retry. On Mainnet, downloading to disk first
is more reliable. To stream anyway:

```sh
curl -sS "$url" | tar --zstd -x -C /path/to/your/cache_dir
```

The trade-off is that you write the archive into your state directory before
you have checked it. A transfer that is corrupted or truncated on the way will
still fail loudly — TLS detects a truncated stream, and `zstd` and `tar` reject
damaged input — so this is mostly safe against accidental corruption. It gives
up nothing against a substituted archive, because as [On trust](#on-trust)
explains, the published checksum never covered that case either. If you have an
expected digest from out of band, download to disk and verify it before you
extract rather than streaming.

## Docker

If you run Zebra with the `zfnd/zebra` image and the named volume from the
[Docker quick start](./docker.md#quick-start), extract into the volume with a
throwaway container. The image's entrypoint fixes file ownership on the next
start, so extracting as root is fine:

```sh
docker run --rm \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  -v "$PWD":/snapshots:ro \
  alpine sh -c 'apk add --no-cache zstd tar >/dev/null && \
    tar --zstd -xf /snapshots/<archive>.tar.zst -C /home/zebra/.cache/zebra'
```

Replace `<archive>` with the file you downloaded, and keep the volume name and
mount point in step with whatever your `docker run` or compose file uses.

You can stream into the volume instead, with the same trade-off described in
[Streaming](#streaming-to-avoid-needing-the-space-twice) — note the `-i` so the
container reads stdin:

```sh
curl -sS "$url" | docker run --rm -i \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  alpine sh -c 'apk add --no-cache zstd tar >/dev/null && \
    tar --zstd -x -C /home/zebra/.cache/zebra'
```

## Troubleshooting

- **Sync starts from genesis anyway.** Your `cache_dir` doesn't match where you
  extracted the archive. Check both paths, and that the extracted tree is
  `<cache_dir>/state/v<major>/<network>/`.
- **Sync starts from genesis even though the paths match.** The archive is from
  a major your build doesn't use, so `zebrad` ignored it and logged `creating
  new database with the current format`. Compare `database_format_major_version`
  in the manifest with your build's `DATABASE_FORMAT_VERSION`, and see
  [Check the database format version](#check-the-database-format-version).
- **Checksum mismatch.** Re-download. Don't use a file that fails
  `sha256sum -c`.
