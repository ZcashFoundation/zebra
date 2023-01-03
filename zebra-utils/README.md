# Zebra Utilities

This crate contains tools for zebra maintainers.

## Programs

- [zebra-checkpoints](#zebra-checkpoints)
- [zebrad-hash-lookup](#zebrad-hash-lookup)
- [zebrad-log-filter](#zebrad-log-filter)
- [zcash-rpc-diff](#zcash-rpc-diff)

Binaries are easier to use if they are located in your system execution path.

### zebra-checkpoints

This command generates a list of zebra checkpoints, and writes them to standard output. Each checkpoint consists of a block height and hash.

To create checkpoints, you need a synchronized instance of `zebrad` or `zcashd`, and the `zcash-cli` RPC client.

`zebra-checkpoints` is a standalone rust binary, you can compile it using:

```sh
cargo install --locked --git https://github.com/ZcashFoundation/zebra zebra-utils
```

Then update the checkpoints using these commands:
```sh
zebra-checkpoints --last-checkpoint $(tail -1 zebra-consensus/src/checkpoint/main-checkpoints.txt | cut -d" " -f1) | tee --append zebra-consensus/src/checkpoint/main-checkpoints.txt &
zebra-checkpoints --last-checkpoint $(tail -1 zebra-consensus/src/checkpoint/test-checkpoints.txt | cut -d" " -f1) -- -testnet | tee --append zebra-consensus/src/checkpoint/test-checkpoints.txt &
wait
```

You can see all the `zebra-checkpoints` options using:

```sh
target/release/zebra-checkpoints --help
```

For more details, see the [`zebra-checkpoints` README.](https://github.com/ZcashFoundation/zebra/tree/main/zebra-consensus/src/checkpoint/README.md)

### zebrad-hash-lookup

Given a block hash the script will get additional information using `zcash-cli`.

```sh
$ echo "00000001f53a5e284393dfecf2a2405f62c07e2503047a28e2d1b6e76b25f863" | zebrad-hash-lookup
high: 3299
time: 2016-11-02T13:24:26Z
hash: 00000001f53a5e284393dfecf2a2405f62c07e2503047a28e2d1b6e76b25f863
prev: 00000001dbbb8b26eb92003086c5bd854e16d9f16e2e5b4fcc007b6b0ae57be3
next: 00000001ff3ac2b4ccb57d9fd2d1187475156489ae22337ca866bbafe62991a2
$
```
This program is commonly used as part of `zebrad-log-filter` where hashes will be captured from `zebrad` output.

### zebrad-log-filter

The program is designed to filter the output from the zebra terminal or log file. Each time a hash is seen the script will capture it and get the additional information using `zebrad-hash-lookup`.

Assuming `zebrad`, `zclash-cli`, `zebrad-hash-lookup` and `zebrad-log-filter` are in your path the program can used as:

```sh
$ zebrad -v start | zebrad-log-filter
...
block::Hash("
high: 2800
time: 2016-11-01T16:17:16Z
hash: 00000001ecd754790237618cb79c4cd302e52571ecda7a80e6113c5e423c0e55
prev: 00000003ed8623d9499f4bf80f8bc410066194bf6813762b31560f9319205bf8
next: 00000001436277884eef900772f0fcec9566becccebaab4713fd665b60fab309
"))) max_checkpoint_height=Height(419581)
...
```

### zcash-rpc-diff

This program compares `zebrad` and `zcashd` RPC responses.

Make sure you have zcashd and zebrad installed and synced.

The script:
1. gets the `zebrad` and `zcashd` tip height and network
2. sends the RPC request to both of them using `zcash-cli`
3. compares the responses using `diff`
4. leaves the full responses in files in a temporary directory, so you can check them in detail
5. if possible, compares different RPC methods for consistency

Assuming `zebrad`'s RPC port is 28232, you should be able to run:
```sh
$ zebra-utils/zcash-rpc-diff 28232 getinfo
Checking zebrad network and tip height...
Checking zcashd network and tip height...

Request:
getinfo

Querying zebrad main chain at height 1649797...
Querying zcashd main chain at height 1649797...

Response diff (between zcashd port and port 28232):
--- /run/user/1000/tmp.g9CJecu2Wo/zebrad-main-1649797-getinfo.json      2022-04-29 14:08:46.766240355 +1000
+++ /run/user/1000/tmp.g9CJecu2Wo/zcashd-main-1649797-getinfo.json      2022-04-29 14:08:46.769240315 +1000
@@ -1,4 +1,16 @@
 {
-  "build": "1.0.0-beta.8+54.ge83e93a",
-  "subversion": "/Zebra:1.0.0-beta.8/"
+  "version": 4070050,
+  "build": "v4.7.0-gitian",
+  "subversion": "/MagicBean:4.7.0/",
... more extra zcashd fields ...
 }
```

Sometimes zcashd will have extra fields (`+`) or different data (`-` and `+`).
And sometimes it will have the same data, but in a different order.

The script will warn you if the heights or networks are different,
then display the results of querying the mismatched node states.

The script accepts any RPC, with any number of arguments.
If a node doesn't implement an RPC, the script will exit with an error.

#### Configuration

The script uses the configured `zcash-cli` RPC port,
and the `zebrad` port supplied on the command-line.

It doesn't actually check what kind of node it is talking to,
so you can compare two `zcashd` or `zebrad` nodes if you want.
(Just edit the `zcash.conf` file used by `zcash-cli`, or edit the script.)

You can override the binaries the script calls using these environmental variables:
- `$ZCASH_CLI`
- `$DIFF`
- `$JQ`
