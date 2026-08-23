# Syncing from a State Snapshot

A full sync from genesis takes days on Mainnet. The Zcash Foundation publishes
snapshots of Zebra's cached state at <https://snapshots.zfnd.org/> so a new
node can start a few blocks behind the tip instead.

## Available snapshots

| Network | Archive | Manifest |
|---|---|---|
| Mainnet | [zebrad-state-mainnet-v28-20260728-post.tar.zst](https://snapshots.zfnd.org/mainnet/20260728-post/zebrad-state-mainnet-v28-20260728-post.tar.zst) (268.7 GB) | [manifest.json](https://snapshots.zfnd.org/mainnet/20260728-post/zebrad-state-mainnet-v28-20260728-post.manifest.json) |
| Testnet | [zebrad-state-testnet-v28-20260728.tar.zst](https://snapshots.zfnd.org/testnet/20260728/zebrad-state-testnet-v28-20260728.tar.zst) (9.1 GB) | [manifest.json](https://snapshots.zfnd.org/testnet/20260728/zebrad-state-testnet-v28-20260728.manifest.json) |

Each archive has a `.sha256` checksum at the same URL with `.sha256` appended.

Always read the manifest before using a snapshot: it has the exact chain
height, the database format version, and the `zebrad` version that produced
that specific file.

## On trust

A snapshot replaces the blocks Zebra would otherwise download and validate
itself. When you load one, you are trusting that the node it was cut from
validated the chain correctly up to the snapshot height; Zebra does not
re-validate the blocks it finds in its cached state.

The `.sha256` checksum only proves the file you downloaded is the file that was
published. The manifests are currently **unsigned**, so it does not prove the
file was published by the Zcash Foundation. If your node secures funds or
serves other people's wallets, and you cannot afford to trust a third party's
validation, sync from genesis instead.

## Before you start

1. **Check the database format version.** Compare `database_format_major_version`
   in the manifest with the version your `zebrad` build expects. Zebra logs it
   at startup as the `v<major>` directory in the cached state path
   (`Opened Zebra state cache at .../state/v28/mainnet`), and it is defined as
   `DATABASE_FORMAT_VERSION` in `zebra-state/src/constants.rs` at your
   release's tag. If the major versions differ, the snapshot won't load; don't
   use it.
2. **Free disk space.** The Mainnet archive is ~270 GB compressed and needs about
   that much again once extracted. Have at least 300 GB free, more if you keep
   the compressed archive around.
3. **Stop `zebrad`** if it is already running against the `cache_dir` you are
   about to use.
4. **Know your `cache_dir`.** This is `state.cache_dir` in your `zebrad.toml`
   (`~/.cache/zebra` by default on Linux, `/home/zebra/.cache/zebra` inside the
   Docker image). Getting this path wrong is the most common way this goes
   silently wrong; see the warning in step 2 below.

## Steps

### 1. Download and verify

```sh
curl -sSO https://snapshots.zfnd.org/mainnet/20260728-post/zebrad-state-mainnet-v28-20260728-post.tar.zst
curl -sSO https://snapshots.zfnd.org/mainnet/20260728-post/zebrad-state-mainnet-v28-20260728-post.tar.zst.sha256

sha256sum -c zebrad-state-mainnet-v28-20260728-post.tar.zst.sha256
```

Swap in the Testnet URLs from the table above if that's what you need.

### 2. Extract into your `cache_dir`

The archive already contains the `state/v28/mainnet/` path Zebra expects, so
extract it straight into `cache_dir`:

```sh
mkdir -p /path/to/your/cache_dir
tar --zstd -xf zebrad-state-mainnet-v28-20260728-post.tar.zst -C /path/to/your/cache_dir
```

To avoid keeping the compressed archive on disk, you can stream the download
straight into `tar`. This skips the checksum step, so only do it if you have
verified the checksum some other way:

```sh
curl -sS https://snapshots.zfnd.org/mainnet/20260728-post/zebrad-state-mainnet-v28-20260728-post.tar.zst \
  | tar --zstd -x -C /path/to/your/cache_dir
```

**This path has to match exactly.** If the directory you extract into is not
`state.cache_dir` in your `zebrad.toml`, `zebrad` won't error; it will create an
empty state there and start syncing from genesis, silently. Double-check the
path before you extract.

#### Docker

If you run Zebra with the `zfnd/zebra` image and the named volume from the
[Docker quick start](docker.md#quick-start), extract into the volume with a
throwaway container. The image's entrypoint fixes file ownership on the next
start, so extracting as root is fine:

```sh
docker run --rm \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  -v "$PWD":/snapshots:ro \
  alpine sh -c 'apk add --no-cache zstd tar >/dev/null && \
    tar --zstd -xf /snapshots/zebrad-state-mainnet-v28-20260728-post.tar.zst -C /home/zebra/.cache/zebra'
```

### 3. Start `zebrad`

```toml
# zebrad.toml
[state]
cache_dir = "/path/to/your/cache_dir"
```

```sh
zebrad start
```

Check the logs for the loaded tip. It should be close to `chain_tip.height`
from the manifest, not genesis:

```text
zebra_state::service: loaded Zebra state cache chain_tip=Some((block::Hash("0000..."), Height(3428145)))
```

From there `zebrad` syncs the remaining blocks up to the current chain tip.

## Troubleshooting

- **Sync starts from genesis anyway.** Your `cache_dir` doesn't match where you
  extracted the archive. Check both paths, and that the extracted tree is
  `<cache_dir>/state/v28/<network>/`.
- **`zebrad` won't start, or errors about the state format.** Compare
  `database_format_major_version` in the manifest with your build's
  `DATABASE_FORMAT_VERSION`. A mismatch won't fix itself on retry; you need a
  snapshot cut for your version, or a `zebrad` build matching the snapshot.
- **Checksum mismatch.** Re-download. Don't use a file that fails `sha256sum -c`.
