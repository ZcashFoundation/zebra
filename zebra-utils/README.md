# Zebra Utilities

This crate contains tools for zebra mantainers. 

## Programs

- [zebra-checkpoints](#zebra-checkpoints)
- [zebrad-hash-lookup](#zebrad-hash-lookup)
- [zebrad-log-filter](#zebrad-log-filter)

Binaries are easier to use if they are located in your system execution path.

### zebra-checkpoints

This software will output a list of zcash block hashes that are used as zebra checkpoints. To create checkpoints a synchronized instance of `zcashd` is needed and a `zcash-cli` wallet connected to it must be available locally. For more information about how to create a checkpoint list please check [here](https://github.com/ZcashFoundation/zebra/tree/main/zebra-consensus/src/checkpoint/README.md)


This is a standalone rust binary, can be compiled as:

```
cargo build --release --bin zebra-checkpoints
```

After built check the program help with:

```
./target/release/zebra-checkpoints --help
```

### zebrad-hash-lookup

Given a block hash the script will get additional information using `zcash-cli`.

```
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

```
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
