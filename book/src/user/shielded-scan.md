# Zebra Shielded Scanning

This document describes Zebra's shielded scanning from users' perspective.

For now, we only support Sapling, and only store transaction IDs in the scanner results database.
Ongoing development is tracked in issue [#7728](https://github.com/ZcashFoundation/zebra/issues/7728).

## Important Security Warning

Zebra's shielded scanning feature has known security issues. It is for experimental use only.

Do not use regular or sensitive viewing keys with Zebra's experimental scanning
feature. Do not use this feature on a shared machine. We suggest generating new
keys for experimental use or publicly known keys.

## Build & Install

Use [Zebra 1.6.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.6.0)
or greater, or the `main` branch to get the latest features, and enable the
`shielded-scan` feature during the build. You can also use Rust's `cargo` to
install the latest release:

```bash
cargo install --features shielded-scan --locked --git https://github.com/ZcashFoundation/zebra zebrad
```

Zebra binary will be at `~/.cargo/bin/zebrad`, which should be in your `PATH`.

## Configuration

Generate a configuration file with the default settings:

```bash
zebrad generate -o ~/.config/zebrad.toml
```


In the generated `zebrad.toml` file, use:

- the `[shielded_scan]` table for database settings, and
- the `[shielded_scan.sapling_keys_to_scan]` table for diversifiable full viewing keys.

Sapling diversifiable/extended full viewing keys strings start with `zxviews` as
described in
[ZIP-32](https://zips.z.cash/zip-0032#sapling-extended-full-viewing-keys).

For example, to scan the block chain with the [public ZECpages viewing
key](https://zecpages.com/boardinfo), use:

```toml
[shielded_scan.sapling_keys_to_scan]
"zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz" = 419200
```

Where the number 419200 is the birthday of the key:
- birthday lower than the Sapling activation height defaults to Sapling activation height.
- birthday greater or equal than Sapling activation height will start scanning at provided height, improving scanner speed.

## Scanning the Block Chain

Simply run

```bash
zebrad
```

The scanning will start once Zebra syncs its state past the Sapling activation
height. Scanning a synced state takes between 12 and 24 hours. The scanner looks
for transactions containing Sapling notes with outputs decryptable by the
provided viewing keys.

You should see log messages in the output every 10 000 blocks scanned, similar
to:

```
2023-12-16T12:14:41.526740Z  INFO zebra_scan::storage::db: Last scanned height for key number 0 is 435000, resuming at 435001
2023-12-16T12:14:41.526745Z  INFO zebra_scan::storage::db: loaded Zebra scanner cache
...
2023-12-16T12:15:19.063796Z  INFO {zebrad="39830b0" net="Main"}: zebra_scan::scan: Scanning the blockchain for key 0, started at block 435001, now at block 440000, current tip 2330550
...
```

The Zebra scanner will resume the task if your Zebra instance went down for any
reason. In a new start, Zebra will display:

```
Last scanned height for key number 0 is 1798000, resuming at 1798001
```

## Displaying Scanning Results

An easy way to query the results is to use the
[Scanning Results Reader](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils#scanning-results-reader).

## Querying Raw Scanning Results

A more advanced way to query results is to use `ldb` tool, requires a certain level of expertise.

Install `ldb`:

```bash
sudo apt install rocksdb-tools
```

Run `ldb` with the scanner database:

```bash
ldb --db="$HOME/.cache/zebra/private-scan/v1/mainnet" --secondary_path= --column_family=sapling_tx_ids --hex scan
```

Some of the output will be markers the scanner uses to keep track of progress, however, some of them will be transactions found.

To lean more about how to filter the database please refer to [RocksDB Administration and Data Access Tool](https://github.com/facebook/rocksdb/wiki/Administration-and-Data-Access-Tool)
