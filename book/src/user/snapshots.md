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
archive, what a snapshot proves and how to check it, which snapshots your build
can actually load, and where the archive has to land.

## Resolving the current archive

There is no `latest` alias URL, so don't hardcode an archive name. Each network
publishes `https://snapshots.zfnd.org/<network>/snapshots.json`: an array of
every archive available for that network, newest first, where the newest entry
carries `"latest"` in its `roles`. Resolve it at download time:

```sh
network=mainnet   # or testnet
entry=$(curl -fsS "https://snapshots.zfnd.org/$network/snapshots.json" \
  | jq -c 'map(select(.roles | index("latest")))[0]')

url=$(echo "$entry" | jq -r .url)
manifest_url=$(echo "$entry" | jq -r .manifest_url)
bundle_url=$(echo "$entry" | jq -r '.attestation_bundle_url // empty')
[ -n "$bundle_url" ] || echo "This snapshot is not attested."
```

An attested entry has `signing_status` set to `attested` and an
`attestation_bundle_url`. Older entries are `unsigned` and have no bundle, so
you can't check where they came from. If the entry you resolved is unsigned,
wait for an attested snapshot, or sync from genesis.

Each entry also carries `height`, `db_major`, `sha256`, and `zebrad_version`, so
a script can check compatibility before it commits to a long download:

```sh
echo "$entry" \
  | jq -r '"height=\(.height) db_major=\(.db_major) zebrad=\(.zebrad_version)"'
```

## What a snapshot proves

A snapshot replaces the blocks Zebra would otherwise download and validate
itself. How far you can trust one comes down to four separate questions:

| Question                                       | What answers it                                                                              | What it leaves open                                                      |
| ---------------------------------------------- | -------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------ |
| 1. Did the archive arrive intact?              | Its SHA-256 matches `archive.sha256` in the manifest.                                        | Who produced it.                                                         |
| 2. Did it come from the Foundation's producer? | The manifest's attestation verifies, so the `archive.sha256` inside it is the producer's.    | Whether the state the producer archived is correct.                      |
| 3. Does it follow the expected chain?          | Your node's block at the last checkpoint height matches your release's checkpoint list.      | Anything that doesn't change a block hash, such as the state's contents. |
| 4. Is the state correct?                       | Nothing. No check proves it.                                                                 | Nullifiers, UTXOs, anchors, value pools, and the rest of the state.      |

The Foundation's snapshot producer is the root of trust. If it archived a
wrong state, every check on this page still passes.

The `.sha256` file next to each archive is served from the same host as the
archive, so a host that serves a bad archive serves a matching checksum with
it. It answers only the first question.

### What `zebrad` checks after a restore

`zebrad` does not re-validate the blocks in a restored state. When it commits
new blocks on top of the restored tip, it checks two parts of that state
against the new block headers:

- **The history tree** ([ZIP-221](https://zips.z.cash/zip-0221)). The first new
  block's header commits to it, so a wrong history tree is caught at that block.
- **The Sapling, Orchard, and Ironwood note commitment frontiers at the tip.**
  Their roots go into the history tree, so a wrong frontier is caught when the
  second new block is committed.

A mismatch rejects the block with `InvalidBlockCommitment`. The node doesn't
panic. It stops making progress, and the sync stalls.

`zebrad` reads the rest of the restored state but never checks it against any
commitment. That includes:

- the nullifier sets
- the transparent UTXO set
- the Sprout, Sapling, Orchard, and Ironwood anchor sets
- the chain value pools
- the stored blocks, and the address and transaction indexes

Tampering with these has different effects. A removed nullifier or an added
anchor lets an invalid spend through without any sign. An added nullifier, or a
removed UTXO or anchor, makes valid blocks fail, which looks like a stalled
sync. Changed value pools can make the node accept or reject blocks differently
from the rest of the network. Changed stored blocks or indexes change what the
node returns over RPC. The
[research ticket](https://github.com/ZcashFoundation/zebra/issues/11914) lists
every part of the stored state, with code references.

### Verify a snapshot

These steps need an attested snapshot, and they use the variables from
[Resolving the current archive](#resolving-the-current-archive). You need
`curl`, `jq`, `sha256sum`, and the [GitHub CLI](https://cli.github.com/)
(`gh`). The commands were tested with `gh` 2.102.0. Verifying needs no GitHub
login.

1. **Download the manifest and its attestation bundle.** Both are small, so get
   them before the archive. The manifest records the archive's SHA-256 and the
   finalized chain tip inside the archive. The bundle is the Sigstore signature
   over the manifest.

   ```sh
   curl -fsSO "$manifest_url" -O "$bundle_url"
   manifest=$(basename "$manifest_url")
   bundle=$(basename "$bundle_url")
   ```

2. **Verify the manifest attestation.** Generate the trusted root yourself with
   `gh`. It is your trust anchor, so never download it from the snapshot site.

   ```sh
   gh attestation trusted-root > trusted_root.jsonl
   gh attestation verify "$manifest" \
     --bundle "$bundle" \
     --custom-trusted-root trusted_root.jsonl \
     --repo ZcashFoundation/zcash-fleet-ops \
     --signer-workflow ZcashFoundation/zcash-fleet-ops/.github/workflows/snapshot-cut.yml \
     --source-ref refs/heads/main
   ```

   This proves that the Foundation's `snapshot-cut.yml` workflow, running from
   the `main` branch, produced this exact manifest. It says nothing about the
   chain data. Don't use a snapshot whose manifest fails verification.

3. **Download the archive and check it against the manifest.** The attestation
   covers the manifest's `archive.sha256`, so this check ties the archive to the
   signed manifest.

   ```sh
   curl -fsSO "$url"
   archive=$(basename "$url")
   echo "$(jq -r .archive.sha256 "$manifest")  $archive" | sha256sum -c
   ```

   This catches a corrupted or substituted archive. Don't use an archive that
   fails the check.

4. **Extract the archive and start `zebrad`.** Follow
   [Extract into your `cache_dir`](#extract-into-your-cache_dir), into a
   `cache_dir` with no earlier state for that network.

5. **Compare the starting tip with the manifest.** The manifest's `chain_tip`
   is the finalized tip inside the archive, and a node restored from it starts
   at exactly that tip. On its first start, `zebrad` logs that tip in the
   `loaded Zebra state cache` line. Its hash and height must equal these:

   ```sh
   jq -r '.chain_tip | "\(.height) \(.block_hash)"' "$manifest"
   ```

   This catches a node that didn't open the archive you verified, for example
   because of a wrong `cache_dir` or state left over from an earlier run. Only
   the first start counts, because the tip moves on as the node syncs.

6. **Compare the last checkpoint with your release.** Each Zebra release
   compiles in a list of checkpoints: block hashes that Zebra maintainers
   generate from a synced node and update in releases. The last line of
   `main-checkpoints.txt` (`test-checkpoints.txt` on Testnet) is the highest.
   Fetch the file from the tag of the release you run, and compare that line
   with the node's block at the same height, using the `getblockhash` RPC.

   RPC is off unless you set `rpc.listen_addr`. It uses cookie authentication
   by default, with the cookie in `rpc.cookie_dir`, which defaults to
   `~/.cache/zebra` on Linux even if you change `state.cache_dir`. Adjust the
   first four lines to your setup:

   ```sh
   tag=vX.Y.Z                    # the Zebra release tag you run
   file=main-checkpoints.txt     # test-checkpoints.txt on Testnet
   rpc=http://127.0.0.1:8232/    # 18232 is the usual Testnet port
   cookie=~/.cache/zebra/.cookie

   last=$(curl -fsSL "https://raw.githubusercontent.com/ZcashFoundation/zebra/$tag/zebra-chain/src/parameters/checkpoint/$file" | tail -n 1)
   height=${last% *} hash=${last#* }
   node_hash=$(curl -sS -u "$(cat "$cookie")" -H 'Content-type: application/json' \
     --data-binary "{\"jsonrpc\":\"1.0\",\"id\":\"snapshot\",\"method\":\"getblockhash\",\"params\":[$height]}" \
     "$rpc" | jq -r .result)
   [ "$node_hash" = "$hash" ] && echo match || echo MISMATCH
   ```

   This confirms that the chain your node follows contains your release's
   highest checkpoint. `zebrad` runs the same comparison for every compiled
   checkpoint at startup, and exits if one differs. So on a running node, a
   mismatch usually means the list you fetched is for a different release or
   network. This check doesn't catch deliberate tampering: someone who changes
   nullifiers or UTXOs leaves every block hash unchanged.

### Nodes that accept deposits or hold funds

If your node accepts deposits or custodies funds, sync from genesis. Every
check above leaves the nullifier set unverified. A snapshot with one nullifier
removed would let the node accept a deposit that spends an already-spent note,
and `zebrad` would not detect it. The same applies if you can't afford to trust
a third party's validation for any other reason, such as serving other people's
wallets.

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

If that `cache_dir` has held state for this network before, remove
`<cache_dir>/state/v<major>/<network>` and `<cache_dir>/non_finalized_state/<network>`
first. Extracting over an old database can leave its files next to the
archive's, and `zebrad` restores an old non-finalized backup on top of the
snapshot.

Once it starts, confirm from the logs that it picked the snapshot up. The tip
should not be genesis. For a verified snapshot, compare it with the manifest as
in step 5 of [Verify a snapshot](#verify-a-snapshot):

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
damaged input — so this is mostly safe against accidental corruption. It also
skips the check of the archive against the attested manifest in
[Verify a snapshot](#verify-a-snapshot), because that check needs the whole
file. Verify the manifest first. Then, if you want the archive checked, download
it to disk and check its digest before you extract rather than streaming.

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
- **Attestation verification fails.** Don't use the snapshot. Download the
  manifest and its bundle again and retry. A `no custom verifier found for
  issuer` error means your `trusted_root.jsonl` is damaged, for example by
  losing its final newline, so regenerate it with `gh attestation trusted-root`.
- **Digest mismatch.** `sha256sum -c` against the manifest's `archive.sha256`
  failed, so the file is not the archive the manifest describes. Download it
  again, and don't use a file that still fails.
- **The tip or checkpoint hash doesn't match.** Don't use the state. Check that
  you extracted the archive for the right network into a `cache_dir` with no
  earlier state for that network, then compare again. If it still differs, sync
  from genesis.
