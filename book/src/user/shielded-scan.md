# Zebra Shielded Scanning

The `zebra-scanner` binary is a standalone application that utilizes Zebra libraries to scan for transactions associated with specific Sapling viewing keys. It stores the discovered transactions and scanning progress data in a RocksDB database.

For this application to function, it requires access to a Zebra node's RPC server and state cache.
 
For now, we only support Sapling, and only store transaction IDs in the scanner results database.

Ongoing development is tracked in issue [#7728](https://github.com/ZcashFoundation/zebra/issues/7728).

## Important Security Warning

Zebra's shielded scanning feature has known security issues. It is for experimental use only.

Do not use regular or sensitive viewing keys with Zebra's experimental scanning feature. Do not use this feature on a shared machine. We suggest generating new keys for experimental use or using publicly known keys.

## Build & Install

Use [Zebra 1.9.0](https://github.com/ZcashFoundation/zebra/releases/tag/v1.9.0) or greater, or the `main` branch to get the latest features.

You can also use Rust's `cargo` to install `zebra-scanner` from the latest release Zebra repository:

```bash
cargo install --locked --git https://github.com/ZcashFoundation/zebra zebra-scan
```

The scanner binary will be at `~/.cargo/bin/zebra-scanner`, which should be in your `PATH`.

## Arguments

Retrieve the binary arguments with:

```bash
zebra-scanner --help
```

## Scanning the Block Chain

Before starting, ensure a `zebrad` node is running locally with the RPC endpoint open. Refer to the [lightwalletd zebrad setup](https://zebra.zfnd.org/user/lightwalletd.html#configure-zebra-for-lightwalletd) or [zebrad mining setup](https://zebra.zfnd.org/user/mining.html#configure-zebra-for-mining) for instructions.

To initiate the scanning process, you need the following:

- A zebrad cache state directory. This can be obtained from the running zebrad configuration file, under the `state` section in the `cache_dir` field.
- A key to scan with, optionally including a birthday height, which specifies the starting height for the scanning process for that key.
- A zebrad RPC endpoint address. This can be found in the running zebrad configuration file, under the `rpc` section in the `listen_addr` field.


Sapling diversifiable/extended full viewing keys strings start with `zxviews` as
described in
[ZIP-32](https://zips.z.cash/zip-0032#sapling-extended-full-viewing-keys).

For example, to scan the block chain with the [public ZECpages viewing
key](https://zecpages.com/boardinfo), use:

```bash
RUST_LOG=info zebra-scanner --sapling-keys-to-scan '{"key":"zxviews1q0duytgcqqqqpqre26wkl45gvwwwd706xw608hucmvfalr759ejwf7qshjf5r9aa7323zulvz6plhttp5mltqcgs9t039cx2d09mgq05ts63n8u35hyv6h9nc9ctqqtue2u7cer2mqegunuulq2luhq3ywjcz35yyljewa4mgkgjzyfwh6fr6jd0dzd44ghk0nxdv2hnv4j5nxfwv24rwdmgllhe0p8568sgqt9ckt02v2kxf5ahtql6s0ltjpkckw8gtymxtxuu9gcr0swvz", "birthday_height": 419200}' --zebrad-cache-dir /media/alfredo/stuff/chain/zebra --zebra-rpc-listen-addr '127.0.0.1:8232'
```

- A birthday lower than the Sapling activation height defaults to Sapling activation height.
- A birthday greater than or equal to Sapling activation height will start scanning at provided height, improving scanner speed.

The scanning process begins once Zebra syncs its state past the Sapling activation height. Scanning a synced state takes between 12 and 24 hours. The scanner searches for transactions containing Sapling notes with outputs decryptable by the provided viewing keys.

You will see log messages in the output every 10,000 blocks scanned, similar to:

```
...
2024-07-13T16:07:47.944309Z  INFO zebra_scan::service::scan_task::scan: Scanning the blockchain for key 0, started at block 571001, now at block 580000, current tip 2556979
2024-07-13T16:08:07.811013Z  INFO zebra_scan::service::scan_task::scan: Scanning the blockchain for key 0, started at block 571001, now at block 590000, current tip 2556979
...
```

If your Zebra instance goes down for any reason, the Zebra scanner will resume the task. Upon a new start, Zebra will display:

```
2024-07-13T16:07:17.700073Z  INFO zebra_scan::storage::db: Last scanned height for key number 0 is 590000, resuming at 590001
2024-07-13T16:07:17.706727Z  INFO zebra_scan::service::scan_task::scan: got min scan height start_height=Height(590000)
```

## Displaying Scanning Results

An easy way to query the results is to use the
[Scanning Results Reader](https://github.com/ZcashFoundation/zebra/tree/main/zebra-utils#scanning-results-reader).

## Querying Raw Scanning Results

A more advanced way to query results is to use `ldb` tool, which requires a certain level of expertise.

Install `ldb`:

```bash
sudo apt install rocksdb-tools
```

Run `ldb` with the scanner database:

```bash
ldb --db="$HOME/.cache/zebra/private-scan/v1/mainnet" --secondary_path= --column_family=sapling_tx_ids --hex scan
```

Some of the output will be markers the scanner uses to keep track of progress, however, some of them will be transactions found.

To learn more about how to filter the database please refer to [RocksDB Administration and Data Access Tool](https://github.com/facebook/rocksdb/wiki/Administration-and-Data-Access-Tool)
