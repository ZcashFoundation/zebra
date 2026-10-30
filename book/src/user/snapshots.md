# Syncing from a state snapshot

The Zcash Foundation publishes compressed Zebra state archives at
<https://snapshots.zfnd.org/>. Zebra resumes syncing from the archived tip.
Check [database compatibility](#check-the-database-format-version) before
downloading.

## On trust

A snapshot substitutes the producer's validation for your own initial sync.
Zebra checks database structure and stored checkpoint hashes on startup, but
it does not fully revalidate every stored block and index. Those checks do not
make an untrusted snapshot safe. Sync from genesis if your security policy
requires validating the chain without trusting a state provider.

A SHA-256 checksum detects a damaged or incomplete download. A checksum served
beside an archive does not authenticate its producer: a compromised server can
replace both files.

A GitHub artifact attestation authenticates the manifest's producing workflow;
the manifest's archive digest binds it to the archive. It does not prove that
the chain data is valid. For unsigned manifests, obtain the expected digest
through an independent trusted channel if you need protection against
compromise of the download server.

## Check the database format version

Zebra 7.0.0 uses database format **29.0.0**, with finalized state under
`<cache_dir>/state/v29/<network>/`. Compare the manifest's
`database_format_major_version` with the [state format][state-constants], not
the producer's Zebra release number.

Zebra also reuses compatible format 28 state without a resync, moving
`state/v28/<network>/` to `state/v29/<network>/` when no usable format 29
database exists there. The old state must resolve inside the cache root;
symlinks outside it are skipped. Reuse considers only the immediately
preceding major version, not format 27 or older. Do not rename a database
directory manually to claim compatibility.

Restore into a fresh cache root. Keep an independent backup for rollback:
reuse moves the database rather than copying it. See [upgrading](upgrading.md)
for migration and rollback procedures.

[state-constants]: https://github.com/ZcashFoundation/zebra/blob/main/zebra-state/src/constants.rs

## Resolve the archive

Install `curl`, `jq`, GNU `tar` with Zstandard support, `zstd`, and
`sha256sum`. Run the following commands in the same shell and download
directory. Use `network=testnet` for public Testnet.

The per-network index is newest first. The `latest` role is not filtered by
database version, so select the newest format that Zebra 7.0.0 can restore:

```sh
network=mainnet
curl -fLsS "https://snapshots.zfnd.org/$network/snapshots.json" \
  -o snapshots.json
snapshot=$(jq -ce '
  map(select(.db_major == 29 or .db_major == 28))
  | first // error("no compatible snapshot")
' snapshots.json)
url=$(printf '%s\n' "$snapshot" | jq -er '.url')
manifest_url=$(printf '%s\n' "$snapshot" | jq -er '.manifest_url')
archive=${url##*/}
manifest=${manifest_url##*/}
curl -fLsS "$manifest_url" -o "$manifest"
jq '{network, chain_tip, database_format_version, archive}' "$manifest"
```

Stop if a command fails. Confirm the manifest's network, database format,
`archive.extraction_path`, and `chain_tip.note`.

Allocate space for the compressed archive, extracted database, and ongoing
growth. The manifest's `archive.size_bytes` is only the compressed size.

## Verify and download

### Signed manifests

If the publication site lists an attestation for the selected manifest,
download its bundle and verify it with the GitHub CLI. Obtain the trusted
roots from the CLI itself, not from the snapshot server:

```sh
curl -fLsS "$manifest_url.sigstore.json" -o "$manifest.sigstore.json"
gh attestation trusted-root > trusted_root.jsonl
gh attestation verify "$manifest" \
  --bundle "$manifest.sigstore.json" \
  --custom-trusted-root trusted_root.jsonl \
  --repo ZcashFoundation/zcash-fleet-ops \
  --signer-workflow ZcashFoundation/zcash-fleet-ops/.github/workflows/snapshot-cut.yml \
  --source-ref refs/heads/main
```

Do not use a snapshot whose manifest verification fails. An unsigned manifest
cannot pass this step; use the trust policy described in [On trust](#on-trust)
rather than treating a missing attestation as successful verification.

### Archive checksum

Download the archive and verify it against the manifest's digest before
extracting it:

```sh
curl -fL "$url" -o "$archive"
printf '%s  %s\n' "$(jq -er '.archive.sha256' "$manifest")" "$archive" \
  | sha256sum --check
```

For an unsigned manifest, this checks transfer integrity only. For a verified
manifest, it also checks that the archive matches the authenticated digest.
Do not extract a file that fails verification. Re-download it and investigate
persistent mismatches.

Do not pipe an unverified download directly into the live state directory.
Streaming extraction writes files before the complete archive digest is
known and can leave partial state after a failed transfer.

## Extract into your cache directory

[Stop Zebra](run.md#stopping-zebra) before replacing its state. Extract into a
new cache root, not over an existing RocksDB database. Keep the old root until
the restored node works and any rollback requirements are satisfied.

Replace `/path/to/zebra-cache` with the new cache root:

```sh
cache_dir=/path/to/zebra-cache
mkdir -p "$cache_dir"
tar --zstd -xf "$archive" -C "$cache_dir"
```

The archives contain the `state/v<major>/<network>/` prefix. Do not extract into
that subdirectory a second time; the resulting path is, for example,
`/path/to/zebra-cache/state/v28/mainnet/`.

Make the extracted files writable by the account that runs Zebra. Set the
same root in its configuration, leaving persistent state enabled:

```toml
[state]
cache_dir = "/path/to/zebra-cache"
ephemeral = false
```

Match `[network] network` to the archive. Check
[environment overrides](config.md#environment-variables), container mounts,
and [state paths](config.md#state).

Start Zebra with that configuration:

```sh
zebrad --config /path/to/zebrad.toml start
```

Check `Opened Zebra state cache at` for the selected path and `chain_tip` in
`loaded Zebra state cache` for the restored height. Reuse logs
`moved state cache from` with the old and new paths.

## Docker

Stop the Zebra container before restoring its volume. The example below uses
the `zebrad-cache` volume and `/home/zebra/.cache/zebra` mount from the
[Docker quick start](docker.md#quick-start). Use a new volume when replacing
an existing database, then update the Zebra container's volume mapping.

After downloading and verifying the archive on the host, extract it through
a temporary container:

```sh
docker run --rm \
  -v zebrad-cache:/home/zebra/.cache/zebra \
  -v "$PWD":/snapshots:ro \
  -e ARCHIVE="$archive" \
  alpine sh -c 'apk add --no-cache zstd tar &&
    tar --zstd -xf "/snapshots/$ARCHIVE" -C /home/zebra/.cache/zebra'
```

The [standard image entrypoint](docker.md) repairs cache ownership on startup.
A custom non-root entrypoint must arrange write permissions itself. Keep the
volume mount and `ZEBRA_STATE__CACHE_DIR` consistent.

## Troubleshooting

- **Zebra syncs from genesis.** Check the network, effective cache root,
  extraction prefix, and database format. An unsupported major directory is
  not a substitute for the directory Zebra opens. A missing usable database
  causes Zebra to create an empty one.
- **Zebra ignores the restored database.** Check the
  [reuse conditions](#check-the-database-format-version); restore into a fresh
  root rather than combining databases.
- **The database cannot open.** Preserve the full error and follow
  [database troubleshooting](troubleshooting.md#the-database-is-locked-or-cannot-open).
- **The starting height differs from the manifest.** Read `chain_tip.note`.
  Older manifests can describe a live source-node tip rather than the
  archived finalized tip. A restart can also restore separately cached
  non-finalized blocks; do not mix unrelated backups into the new root.

See [troubleshooting](troubleshooting.md) for runtime failures after restoration.
