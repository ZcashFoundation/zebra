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
every archive available for that network, newest first, where the newest entry
carries `"latest"` in its `roles`. Resolve it at download time:

```sh
url=$(curl -s https://snapshots.zfnd.org/mainnet/snapshots.json \
  | jq -r 'map(select(.roles | index("latest")))[0].url')

curl -sS -O "$url" -O "$url.sha256"
sha256sum -c "$(basename "$url").sha256"
```

Use `testnet` in place of `mainnet` for the Testnet archive. Don't use an
archive that fails `sha256sum -c`; re-download it.

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

A snapshot only loads into a `zebrad` whose state database format _major_
version matches the archive. If they differ, the snapshot won't load; don't use
it. Get a snapshot cut for your version, or run a `zebrad` that matches.

Both sides of that comparison are easy to read:

- The archive: `db_major` in `snapshots.json`, the `v<major>` in its filename,
  and `database_format_major_version` in its manifest.
- Your build: `DATABASE_FORMAT_VERSION` in `zebra-state/src/constants.rs` at
  your release's tag. Zebra also logs it at startup as the `v<major>` component
  of the cached state path:

  ```text
  zebra_state::...::disk_db: Opened Zebra state cache at /home/user/.cache/zebra/state/v28/mainnet
  ```

The manifest is also where you'll find the snapshot's chain height and the
`zebrad` version that produced it.

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
should be close to the manifest's `chain_tip.height`, not genesis:

```text
zebra_state::service: loaded Zebra state cache chain_tip=Some((block::Hash("0000..."), Height(3459525)))
```

### Streaming, to avoid needing the space twice

Downloading and then extracting needs the archive and the extracted state on
disk at once, and the extracted state is larger than the archive. To avoid
holding both, pipe the download straight into `tar`:

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
- **`zebrad` won't start, or errors about the state format.** Compare
  `database_format_major_version` in the manifest with your build's
  `DATABASE_FORMAT_VERSION`. A mismatch won't fix itself on retry.
- **Checksum mismatch.** Re-download. Don't use a file that fails
  `sha256sum -c`.
